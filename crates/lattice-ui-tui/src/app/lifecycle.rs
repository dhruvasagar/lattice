//! Buffer-creation, activation transitions, and shutdown.
//! R.1.16 lands a focused subset (pane navigation); the
//! bulk migrates with follow-up slices.
//!
//! Methods that live here:
//! - `do_navigate_pane` (`<C-w>h/j/k/l` -- step cardinally
//!   to the spatial neighbour of the active pane; geometry
//!   from `PaneTree::compute_rects`).
//! - `activate_pane` (swap the App's hot-path cursor /
//!   scroll with the target pane's stash).
//! - `load_active_pane` (inverse of `snapshot_active_pane`:
//!   pull stashed cursor / scroll back into App; restore
//!   help-buffer mirror when the pane points at a different
//!   help buffer than the one currently in the hot-path
//!   slot).
//!
//! Stays in app.rs (deferred to follow-up lifecycle slices):
//! - `App::new` (the big constructor).
//! - The activate_* family (activate_document,
//!   activate_buffer, activate_file_tree, activate_oil,
//!   activate_help_in_pane).
//! - `snapshot_active_pane` / `snapshot_active_document`
//!   (already pub(super); used widely).
//! - `do_split_pane` / `do_close_pane` /
//!   `gc_unreferenced_panel_buffers`.
//! - `do_edit` / `do_write` / `do_quit` /
//!   `do_buffer_*` (ex-command bodies; lifecycle-shaped).
//! - `set_viewport_height`, `pending_redraw` handling,
//!   per-loop-iteration state hooks.

use lattice_core::{CoreError, Document};
use lattice_protocol::Event;
use lattice_protocol::position::Position;
use lattice_runtime::{RuntimeError, spawn_document};
use lattice_syntax::{Lang, Syntax};

use super::{App, BufferId, EchoLevel, PositionSource, PrevPaneState};
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferKind};
use crate::pane::{PaneDirection, SplitOrientation};

impl App {
    /// Switch the active document to `id`. Snapshots the current
    /// active state into its entry, then loads from the
    /// destination's entry. No-op if `id` is already active or
    /// not registered.
    pub fn activate_document(&mut self, id: BufferId) {
        if id == self.document_buffer_id && matches!(self.active_buffer, BufferKind::Document) {
            return;
        }
        if self.buffers.document(id).is_none() {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a document", id.0));
            return;
        }
        self.snapshot_active_pane();
        // Same-document fast path: returning to the document
        // buffer that `self.document` still points at (e.g. from
        // a help-in-pane overlay or a file-tree pane).
        // Help overlay leaves `entry.syntax` as None (no stash);
        // file-tree leaves it as Some (stashed via
        // snapshot_active_document). The "is the entry stashed?"
        // check is `entry.syntax.is_some()`; folds piggyback.
        if id == self.document_buffer_id {
            self.active_buffer = BufferKind::Document;
            let pane = self.pane_tree.active_mut();
            pane.buffer = BufferKind::Document;
            pane.buffer_id = id;
            if let Some(entry) = self.buffers.document_mut(id)
                && entry.syntax.is_some()
            {
                self.syntax = entry.syntax.take();
                self.last_parsed_text_version = entry.last_parsed_text_version;
                self.folds = std::mem::take(&mut entry.folds);
            }
            return;
        }
        self.snapshot_active_document();
        // Load destination.
        let entry = self
            .buffers
            .document_mut(id)
            .expect("document() lookup above succeeded");
        self.document = entry.handle.clone();
        // Rebuild the cache against the activated document's
        // published-cell; the previous cache pointed at the old
        // document.
        self.snapshot_cache = self.document.snapshot_cache();
        self.syntax = entry.syntax.take();
        self.last_parsed_text_version = entry.last_parsed_text_version;
        // Folds round-trip with the buffer (see DocumentEntry
        // doc-comment). On first activation the entry is empty
        // and `activate_buffer_state` seeds from foldmethod;
        // subsequent re-activations restore the user's
        // open/closed state.
        self.folds = std::mem::take(&mut entry.folds);
        self.document_buffer_id = id;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Document;
        pane.buffer_id = id;
        // Per-document transient state resets.
        self.current_match = None;
        self.all_matches.clear();
        self.search_line = None;
        self.cursor = Position::ZERO;
        self.scroll = 0;
        self.load_active_pane();
        // Single principled hook for everything that needs to
        // come up with the buffer (parse, folds, highlight cache).
        self.activate_buffer_state();
        self.set_message(
            EchoLevel::Info,
            format!(
                "switched to buffer #{} {}",
                id.0,
                self.document
                    .path()
                    .map(|p| format!("\"{}\"", p.display()))
                    .unwrap_or_else(|| "(no file)".into())
            ),
        );
    }

    /// Switch the active pane to whatever buffer `id` references,
    /// regardless of kind. Document buffers route through
    /// `activate_document`; tree buffers update the active pane +
    /// load the tree's stash; help buffers go through
    /// `activate_help_in_pane`.
    pub fn activate_buffer(&mut self, id: BufferId) {
        let kind = match self.buffers.get(id) {
            Some(entry) => entry.kind(),
            None => {
                self.set_message(EchoLevel::Error, format!("buffer #{} not found", id.0));
                return;
            }
        };
        match kind {
            BufferKind::Document => self.activate_document(id),
            BufferKind::FileTree => self.activate_file_tree(id),
            BufferKind::Help => self.activate_help_in_pane(id),
            BufferKind::Oil => self.activate_oil(id),
        }
    }

    /// Switch the active pane to the file-tree buffer with `id`.
    /// Snapshots the current active state first; the pane's
    /// stashed cursor / scroll load into the tree's hot fields
    /// via `load_active_pane`.
    pub fn activate_file_tree(&mut self, id: BufferId) {
        if self.buffers.file_tree(id).is_none() {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a tree", id.0));
            return;
        }
        if id == self.active_pane_buffer_id() && matches!(self.active_buffer, BufferKind::FileTree)
        {
            return;
        }
        self.snapshot_active_pane();
        self.snapshot_active_document();
        let (stash_cursor, stash_scroll) = self
            .buffers
            .file_tree(id)
            .map(|t| (t.cursor, t.scroll as u32))
            .unwrap_or((Position::ZERO, 0));
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::FileTree;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::FileTree;
        pane.buffer_id = id;
        pane.cursor = stash_cursor;
        pane.scroll = stash_scroll;
    }

    /// Switch the active pane to the oil buffer with `id`.
    pub fn activate_oil(&mut self, id: BufferId) {
        if self.buffers.oil(id).is_none() {
            return;
        }
        let oil_cursor = self.buffers.oil(id).map(|o| o.cursor).unwrap_or(Position::ZERO);
        let oil_scroll = self.buffers.oil(id).map(|o| o.scroll).unwrap_or(0);
        self.active_buffer = BufferKind::Oil;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Oil;
        pane.buffer_id = id;
        pane.cursor = oil_cursor;
        pane.scroll = oil_scroll as u32;
        self.cursor = oil_cursor;
        self.scroll = oil_scroll as u32;
    }

    /// Switch the active pane to an existing help buffer in the
    /// registry. Snapshots prior pane state so `<C-o>` returns the
    /// user to the document/cursor they came from. The registry's
    /// HelpBuffer is mirrored into `self.help_buffer` so the
    /// existing keymap + render paths transparently target it.
    pub(super) fn activate_help_in_pane(&mut self, id: BufferId) {
        if self.buffers.help(id).is_none() {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a help buffer", id.0));
            return;
        }
        // Skip the auto-jump push during picker-preview hovers --
        // the user hasn't committed to this buffer yet, so we
        // don't want every cursor over a candidate to bloat the
        // jump list. The real push happens on `PickerAccept` if
        // the user commits.
        if !self.previewing && matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Capture pre-activation pane + active state so dismiss
        // can restore the user to whatever buffer they came from.
        // Only set when transitioning into help from a non-help
        // buffer; help-to-help transitions (link follows etc.)
        // preserve the original origin.
        if !matches!(self.active_buffer, BufferKind::Help) {
            let active = self.pane_tree.active();
            self.prev_pane_for_help = Some(PrevPaneState {
                buffer: active.buffer,
                buffer_id: active.buffer_id,
                cursor: self.cursor,
                scroll: self.scroll,
            });
        }
        self.snapshot_active_pane();
        // Note: do NOT call snapshot_active_document here. Help
        // is rendered as a popup overlay over the underlying
        // document; the pane's per-frame paint draws the active
        // document via draw_buffer(snap) which reads from
        // self.syntax / self.folds for highlights + fold overlays.
        // Stashing those onto the document entry would leave
        // self.syntax = None for the duration of the help session,
        // so the document underneath the popup paints
        // unhighlighted. The hot-path state stays live; the
        // round-trip back to the same document via
        // activate_document early-returns on matching
        // document_buffer_id.
        if self.help_buffer.as_ref().map(|h| h.id) != Some(id)
            && let Some(reg_help) = self.buffers.help(id)
        {
            self.help_buffer = Some(reg_help.clone());
        }
        let (stash_cursor, stash_scroll) = self
            .help_buffer
            .as_ref()
            .map(|h| (h.cursor, h.scroll as u32))
            .unwrap_or((Position::ZERO, 0));
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Help;
        pane.buffer_id = id;
        pane.cursor = stash_cursor;
        pane.scroll = stash_scroll;
    }

    /// `:bnext` / `:bn` -- cycle to the next listed buffer in id
    /// order, regardless of kind. Skips unlisted buffers; if every
    /// other buffer is unlisted, no-op.
    pub(super) fn do_buffer_next(&mut self) {
        let Some(target) = self.next_listed_buffer_id() else {
            self.set_message(EchoLevel::Info, "only one listed buffer".to_string());
            return;
        };
        self.activate_buffer(target);
    }

    /// `:bprev` / `:bp` -- cycle to the previous listed buffer.
    pub(super) fn do_buffer_prev(&mut self) {
        let Some(target) = self.prev_listed_buffer_id() else {
            self.set_message(EchoLevel::Info, "only one listed buffer".to_string());
            return;
        };
        self.activate_buffer(target);
    }

    /// `:bd[elete]` -- close the active buffer (whichever the
    /// active pane shows). v1 picks any other buffer to activate;
    /// if no others remain, the close is rejected. For document
    /// buffers `!` bypasses the dirty check; tree buffers are
    /// always read-only and skip the dirty guard.
    pub(super) fn do_buffer_delete(&mut self, force: bool) {
        if self.buffers.len() <= 1 {
            self.set_message(
                EchoLevel::Error,
                "Cannot delete the only buffer".to_string(),
            );
            return;
        }
        let to_remove = self.active_pane_buffer_id();
        // Dirty check applies to documents only.
        if let Some(d) = self.buffers.document(to_remove)
            && !force
            && d.handle.dirty()
        {
            self.set_message(
                EchoLevel::Error,
                "no write since last change (add ! to override)".to_string(),
            );
            return;
        }
        let ids = self.buffers.sorted_ids();
        let Some(successor) = ids.iter().copied().find(|id| *id != to_remove) else {
            return;
        };
        self.activate_buffer(successor);
        // Detach from LSP before dropping the buffer registry
        // entry so the supervisor sees the URI go away while the
        // BufferId is still mapped.
        self.lsp_close_buffer(to_remove);
        self.buffers.remove(to_remove);
        // Re-point any pane still referencing the removed buffer.
        let new_id = self.active_pane_buffer_id();
        let new_kind = self.active_buffer;
        for pane in self.pane_tree.leaves_mut() {
            if pane.buffer_id == to_remove {
                pane.buffer_id = new_id;
                pane.buffer = new_kind;
            }
        }
        self.set_message(EchoLevel::Info, format!("buffer #{} deleted", to_remove.0));
    }

    /// Listed buffer ids in ascending order across kinds. `:bn` /
    /// `:bp` cycle through this; unlisted buffers (vim
    /// `nobuflisted`) are filtered out.
    fn listed_buffer_ids_sorted(&self) -> Vec<BufferId> {
        self.buffers.listed_ids_sorted()
    }

    fn next_listed_buffer_id(&self) -> Option<BufferId> {
        let ids = self.listed_buffer_ids_sorted();
        if ids.len() <= 1 {
            return None;
        }
        let cur = self.active_pane_buffer_id();
        let pos = ids.iter().position(|id| *id == cur)?;
        Some(ids[(pos + 1) % ids.len()])
    }

    fn prev_listed_buffer_id(&self) -> Option<BufferId> {
        let ids = self.listed_buffer_ids_sorted();
        if ids.len() <= 1 {
            return None;
        }
        let cur = self.active_pane_buffer_id();
        let pos = ids.iter().position(|id| *id == cur)?;
        Some(ids[if pos == 0 { ids.len() - 1 } else { pos - 1 }])
    }

    /// `:e[dit] FILE` (DESIGN.md §5.9 multi-buffer). If a buffer
    /// for `path` is already open, switch to it; otherwise spawn
    /// a fresh document actor, register it, and switch the active
    /// pane to the new buffer. With no path, re-edit the current
    /// buffer's path (force-reload from disk; `!` required when
    /// dirty).
    pub(super) fn do_edit(&mut self, path: Option<std::path::PathBuf>, force: bool) {
        let target = match path {
            Some(p) => p,
            None => match self.document.path() {
                Some(p) => p,
                None => {
                    self.set_message(EchoLevel::Error, "no file name".to_string());
                    return;
                }
            },
        };
        // Directories defer to `:Tree path` so `:e folder` opens
        // the file-tree buffer.
        if let Ok(meta) = std::fs::metadata(&target)
            && meta.is_dir()
        {
            self.do_open_oil(Some(target));
            return;
        }
        // If `target` is already open, switch to it. The dirty
        // check only applies when we'd discard the current buffer.
        if let Some(existing_id) = self.find_document_by_path(&target) {
            if existing_id == self.document_buffer_id {
                // Re-edit current: reload from disk (vim's `:e`).
                if !force && self.document.dirty() {
                    self.set_message(
                        EchoLevel::Error,
                        "no write since last change (add ! to override)".to_string(),
                    );
                    return;
                }
                let new_doc = match Document::open(&target) {
                    Ok(d) => d,
                    Err(e) => {
                        self.set_message(EchoLevel::Error, format!("open error: {e}"));
                        return;
                    }
                };
                let lang = Lang::detect_from_path(new_doc.path());
                let initial_text = new_doc.text();
                let initial_text_version = new_doc.text_version();
                let syntax: Option<lattice_syntax::SyntaxHandle> =
                    match Syntax::for_language_with_registry(lang, self.lang_registry.clone()) {
                        Ok(Some(mut s)) => {
                            s.parse_at(&initial_text, initial_text_version);
                            Some(lattice_syntax::SyntaxHandle::seeded_with_runtime(
                                s,
                                crate::runtime::lsp_runtime().handle(),
                            ))
                        }
                        _ => None,
                    };
                self.last_parsed_text_version = initial_text_version;
                self.syntax = syntax;
                self.replace_document_blocking(new_doc);
                self.cursor = Position::ZERO;
                self.scroll = 0;
                self.current_match = None;
                self.all_matches.clear();
                self.search_line = None;
                self.last_search = None;
                self.last_find = None;
                self.last_change = None;
                self.last_visual = None;
                self.visual_anchor = None;
                self.replace_history.clear();
                self.position_history.clear();
                self.position_history_cursor = 0;
                self.folds.clear();
                self.set_message(
                    EchoLevel::Info,
                    format!("\"{}\" reloaded", target.display()),
                );
            } else {
                // Different already-open buffer: switch to it.
                self.activate_document(existing_id);
                self.set_message(
                    EchoLevel::Info,
                    format!("\"{}\" (already open)", target.display()),
                );
            }
            return;
        }
        // Brand-new file: open a fresh actor and register it.
        let new_doc = match Document::open(&target) {
            Ok(d) => d,
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("open error: {e}"));
                return;
            }
        };
        let lang = Lang::detect_from_path(new_doc.path());
        let initial_text = new_doc.text();
        let initial_text_version = new_doc.text_version();
        let syntax: Option<lattice_syntax::SyntaxHandle> =
            match Syntax::for_language_with_registry(lang, self.lang_registry.clone()) {
                Ok(Some(mut s)) => {
                    s.parse_at(&initial_text, initial_text_version);
                    Some(lattice_syntax::SyntaxHandle::seeded_with_runtime(
                        s,
                        crate::runtime::lsp_runtime().handle(),
                    ))
                }
                _ => None,
            };
        let new_handle = spawn_document(new_doc, self.registry.clone());
        let new_id = BufferId::next();
        self.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: new_id,
                handle: new_handle.clone(),
                syntax: None,
                last_parsed_text_version: 0,
                last_synced_syntax_version: 0,
                folds: Vec::new(),
            }),
        });
        // Save the currently-active buffer's hot-path state into
        // its registry entry, then load the new buffer's into the
        // hot path.
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.active_buffer = BufferKind::Document;
        self.document_buffer_id = new_id;
        self.document = new_handle;
        self.snapshot_cache = self.document.snapshot_cache();
        self.syntax = syntax;
        self.last_parsed_text_version = self.document.text_version();
        self.cursor = Position::ZERO;
        self.scroll = 0;
        self.current_match = None;
        self.all_matches.clear();
        self.search_line = None;
        self.last_search = None;
        self.last_find = None;
        self.last_change = None;
        self.last_visual = None;
        self.visual_anchor = None;
        self.replace_history.clear();
        self.folds.clear();
        // Position history follows the active buffer.
        self.position_history.clear();
        self.position_history_cursor = 0;
        self.pane_tree.active_mut().buffer = BufferKind::Document;
        self.pane_tree.active_mut().buffer_id = new_id;
        self.activate_buffer_state();
        // Event-driven LSP attach.
        self.publish_document_opened_for_active();
        self.set_message(EchoLevel::Info, format!("\"{}\" opened", target.display()));
    }

    /// `:w[rite] [path]` -- save the active buffer to disk. Oil
    /// buffers route through OilBuffer::apply (diff-and-apply
    /// filesystem ops); document buffers route through
    /// save_blocking / save_as_blocking against the document
    /// actor.
    pub(super) fn do_write(&mut self, path: Option<std::path::PathBuf>) {
        if matches!(self.active_buffer, BufferKind::Oil) {
            let oil_id = self.active_pane_buffer_id();
            // M.3.2.c.3: read dir from buffer-locals for the
            // status message; fall back to the struct field.
            let dir_display = self
                .buffer_locals
                .get(&oil_id)
                .and_then(|locals| locals.get::<crate::modes::OilDir>())
                .map(|d| d.0.display().to_string())
                .or_else(|| {
                    self.buffers
                        .oil(oil_id)
                        .map(|o| o.dir.display().to_string())
                })
                .unwrap_or_default();
            if let Some(oil) = self.buffers.oil_mut(oil_id) {
                match oil.apply() {
                    Ok(()) => self.set_message(EchoLevel::Info, format!("oil: applied changes in {dir_display}")),
                    Err(e) => self.set_message(EchoLevel::Error, format!("oil apply error: {e}")),
                }
            }
            return;
        }
        let result: Result<String, RuntimeError> = match path {
            Some(p) => self
                .save_as_blocking(p.clone())
                .map(|()| p.display().to_string()),
            None => self.save_blocking().map(|p| p.display().to_string()),
        };
        match result {
            Ok(displayed) => self.set_message(EchoLevel::Info, format!("\"{displayed}\" written")),
            Err(RuntimeError::Core(CoreError::NoPath)) => {
                self.set_message(EchoLevel::Error, "no file name (use :w <path>)".to_string());
            }
            Err(e) => self.set_message(EchoLevel::Error, format!("write error: {e}")),
        }
    }

    /// `:q[uit]` -- request editor shutdown. Honors the dirty
    /// guard unless `force` (a trailing `!`). Publishes
    /// `Event::BeforeQuit` for observability; subscribers see it
    /// but cannot veto in v1.
    pub(super) fn do_quit(&mut self, force: bool) {
        if !force && self.document.dirty() {
            self.set_message(
                EchoLevel::Error,
                "no write since last change (add ! to override)".to_string(),
            );
            return;
        }
        // BeforeQuit is observation-only in v1 (no veto seam yet).
        // Subscribers see it; the quit proceeds regardless.
        self.event_bus.publish(Event::BeforeQuit);
        self.should_quit = true;
    }

    /// Split the active pane along `orientation`. The new sibling
    /// inherits the active pane's content + cursor + scroll (so a
    /// fresh `<C-w>s` shows the same view in both panes, vim's
    /// default). Active stays on the original pane.
    pub(super) fn do_split_pane(&mut self, orientation: SplitOrientation) {
        // Save the App's hot-path cursor/scroll into the active
        // pane's stash so the new sibling clones a fresh snapshot.
        self.snapshot_active_pane();
        let _new_idx = self.pane_tree.split_active(orientation);
    }

    /// Close the active pane. The first surviving pane becomes
    /// active. No-op when only one pane is open (vim leaves the
    /// last window alone; closing it would mean closing the editor).
    /// Singleton transient buffers (file tree) get garbage-collected
    /// if no surviving pane references them.
    pub(super) fn do_close_pane(&mut self) {
        if self.pane_tree.len() <= 1 {
            self.set_message(EchoLevel::Warn, "Already only one pane".to_string());
            return;
        }
        self.snapshot_active_pane();
        if !self.pane_tree.close_active() {
            return;
        }
        self.load_active_pane();
        self.gc_unreferenced_panel_buffers();
    }

    /// Drop singleton non-document buffers (currently: file tree)
    /// when no pane still references them. Document buffers are
    /// no-op stub left in for backwards compatibility with the
    /// pre-registry refactor. Trees now live in the unified buffer
    /// registry alongside documents (DESIGN.md §5.9), so closing
    /// the only pane that referenced a tree leaves the tree in the
    /// registry where `:bn` / `:bp` can reach it. Use `:bd` to
    /// actually drop a tree buffer.
    pub(super) fn gc_unreferenced_panel_buffers(&mut self) {}

    /// Step cardinally to the spatial neighbour of the active pane.
    /// Geometry comes from `PaneTree::compute_rects` so the walk
    /// matches what the renderer drew.
    pub(super) fn do_navigate_pane(&mut self, direction: PaneDirection) {
        let area = self.buffer_area_rect();
        let Some(target) = self.pane_tree.navigate(direction, area) else {
            return;
        };
        self.activate_pane(target);
    }

    /// Make pane `idx` the active one, swapping the App's hot-path
    /// cursor / scroll with the target pane's stash.
    pub(super) fn activate_pane(&mut self, idx: usize) {
        if idx == self.pane_tree.active_index() {
            return;
        }
        self.snapshot_active_pane();
        if !self.pane_tree.set_active(idx) {
            return;
        }
        self.load_active_pane();
    }

    /// Inverse of `snapshot_active_pane`: pull the freshly
    /// activated pane's stashed cursor / scroll back into the App's
    /// hot-path fields. `active_buffer` is denormalized from the
    /// pane's `buffer` kind.
    ///
    /// **Unified hot-path**: `self.cursor` and `self.scroll` are
    /// the active buffer's, regardless of kind. Help / file-tree
    /// keep their own cursor / scroll fields as **save state** --
    /// updated at the snapshot boundary so the registry record is
    /// archival-correct, but the *live* cursor is `self.cursor`
    /// for every motion / scroll / search / render path.
    pub(super) fn load_active_pane(&mut self) {
        let pane = *self.pane_tree.active();
        self.active_buffer = pane.buffer;
        self.cursor = pane.cursor;
        self.scroll = pane.scroll;
        // Help: restore the registry copy into the hot-path slot
        // if the active pane points at a different help buffer
        // than the one currently mirrored.
        if matches!(pane.buffer, BufferKind::Help)
            && self.help_buffer.as_ref().map(|h| h.id) != Some(pane.buffer_id)
            && let Some(reg) = self.buffers.help(pane.buffer_id)
        {
            self.help_buffer = Some(reg.clone());
        }
    }
}
