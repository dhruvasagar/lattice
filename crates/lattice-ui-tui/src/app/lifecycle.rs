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
use lattice_grammar::register::Register;
use lattice_protocol::Event;
use lattice_protocol::position::Position;
use lattice_runtime::{RuntimeError, block_on, spawn_document};
use lattice_syntax::{Lang, Syntax};
use std::time::Duration;

use super::{App, BufferId, EchoLevel, PositionSource, PrevPaneState, preview_register};
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferKind};
use crate::help::HelpBuffer;
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

    /// `:ls` / `:buffers` -- render every open buffer (regardless
    /// of kind) in a help-style view. The `%` marker points at
    /// whichever buffer the active pane is currently showing.
    pub(super) fn do_list_buffers(&mut self) {
        let ids = self.buffers.sorted_ids();
        let active_id = self.active_pane_buffer_id();
        let doc_count = self.buffers.document_ids_sorted().len();
        let tree_count = self.buffers.file_tree_ids_sorted().len();
        let help_count = self.buffers.help_ids_sorted().len();
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "{} open buffer(s) ({} document, {} tree, {} help):",
            ids.len(),
            doc_count,
            tree_count,
            help_count,
        ));
        lines.push(String::new());
        for id in ids {
            let Some(entry) = self.buffers.get(id) else {
                continue;
            };
            let active_marker = if id == active_id { "%" } else { " " };
            let listed_marker = if entry.flags.listed { " " } else { "u" };
            match &entry.data {
                BufferData::Document(d) => {
                    let path = d
                        .handle
                        .path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(no file)".to_string());
                    let dirty = if d.handle.dirty() { "[+]" } else { "   " };
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} doc  {dirty} {path}",
                        id.0
                    ));
                }
                BufferData::FileTree(t) => {
                    // M.3.2.c.2: prefer root from buffer-locals.
                    let root = self
                        .buffer_locals
                        .get(&id)
                        .and_then(|locals| locals.get::<crate::modes::FileTreeRoot>())
                        .map(|r| r.0.clone())
                        .unwrap_or_else(|| t.root.clone());
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} tree     {}",
                        id.0,
                        root.display()
                    ));
                }
                BufferData::Help(h) => {
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} help     {}",
                        id.0, h.title,
                    ));
                }
                BufferData::Oil(o) => {
                    // M.3.2.c.3: prefer dir from buffer-locals.
                    let dir = self
                        .buffer_locals
                        .get(&id)
                        .and_then(|locals| locals.get::<crate::modes::OilDir>())
                        .map(|d| d.0.clone())
                        .unwrap_or_else(|| o.dir.clone());
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} oil      {}",
                        id.0,
                        dir.display()
                    ));
                }
            }
        }
        self.open_help(
            HelpBuffer::from_lines("buffers", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Vim's `:reg` -- list every register's contents in the echo area.
    /// v1 shows the unnamed `""`, the numbered `"0`, and the named
    /// alphabetic registers in alphabetical order.
    pub(super) fn do_list_registers(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        if let Some(reg) = &self.unnamed_register {
            lines.push(format!("\"\"  {}", preview_register(&reg.content)));
        }
        let mut keys: Vec<Register> = self.registers.keys().copied().collect();
        keys.sort_by_key(|k| match k {
            Register::Named(c) => format!("a{c}"),
            Register::Numbered(n) => format!("b{n}"),
            Register::System => "z+".into(),
            _ => "z".into(),
        });
        for k in keys {
            // The keys came from `self.registers.keys()`, so the lookup
            // can't fail unless someone races us -- which we don't.
            let Some(entry) = self.registers.get(&k) else {
                continue;
            };
            let label = match k {
                Register::Named(c) => format!("\"{c}"),
                Register::Numbered(n) => format!("\"{n}"),
                Register::System => "\"+".into(),
                _ => "?".into(),
            };
            lines.push(format!("{label}  {}", preview_register(&entry.content)));
        }
        if lines.is_empty() {
            self.set_message(EchoLevel::Info, "no registers set".to_string());
        } else {
            self.set_message(EchoLevel::Info, lines.join("  |  "));
        }
    }

    /// Vim's `:marks` -- list every set mark's name + position.
    pub(super) fn do_list_marks(&mut self) {
        let mut entries: Vec<(char, Position)> = self.marks.iter().map(|(c, p)| (*c, *p)).collect();
        entries.sort_by_key(|(c, _)| *c);
        if entries.is_empty() {
            self.set_message(EchoLevel::Info, "no marks set".to_string());
            return;
        }
        let parts: Vec<String> = entries
            .into_iter()
            .map(|(c, p)| format!("{c}={}:{}", p.line + 1, p.byte))
            .collect();
        self.set_message(EchoLevel::Info, parts.join("  "));
    }

    /// Replace the actor's document outright. Used by `:edit
    /// path`. The actor swaps state in place and republishes the
    /// snapshot.
    pub(super) fn replace_document_blocking(&self, document: Document) {
        let _ = block_on(self.document.replace(document));
    }

    /// Adopt a freshly-built help buffer as the active view. Records
    /// the current document cursor on the position-history ring as
    /// an `AutoJump` (so `<C-o>` from inside the help buffer returns
    /// to the document spot the user opened from), then flips
    /// `active_buffer` to `Help`. Used by every `:describe-*` /
    /// `:apropos` / `:keymap` entry point.
    ///
    /// **Popup vs in-pane.** This is the *popup* path -- the help
    /// content sits on the App's transient `help_buffer` slot and
    /// renders as a centred overlay. The complementary
    /// [`Self::open_help_in_pane`] path registers the buffer in
    /// [`BufferRegistry`] and swaps the active pane to it; that's
    /// what `:lsp-log` / `:lsp-server-log` / `:lsp-trace-log` (Phase
    /// 3) and future persistent help views route through.
    pub(super) fn open_help(&mut self, buffer: HelpBuffer) {
        // Record the *document* cursor (we're still active=Document
        // here, since open_help precedes the active_buffer flip).
        // Skip the push if we're already in Help (a help->help
        // re-open from a link follow); the inter-help transition
        // is recorded by `do_help_follow_link` itself.
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Sync the active pane's cursor / scroll stash *before*
        // swapping `active_buffer` to Help. Once active is Help,
        // the active pane's buffer (Document) no longer matches
        // `app.active_buffer`, so the renderer paints it as
        // visually inactive -- reading from `pane.cursor` rather
        // than `app.cursor`. Without this snapshot the pane stash
        // is whatever it was last set to (often (0,0)) and the
        // doc visibly jumps to the top of file when the popup
        // opens.
        self.snapshot_active_pane();
        // Capture pre-help state so dismiss restores the user
        // cleanly. Mirrors `activate_help_in_pane` / `focus_help_popup`.
        if !matches!(self.active_buffer, BufferKind::Help) {
            let active = self.pane_tree.active();
            self.prev_pane_for_help = Some(PrevPaneState {
                buffer: active.buffer,
                buffer_id: active.buffer_id,
                cursor: self.cursor,
                scroll: self.scroll,
            });
        }
        // Load the help buffer's cursor / scroll into the App's
        // hot path. Motion / scroll / search read / write them
        // uniformly across buffer kinds.
        let stash_cursor = buffer.cursor;
        let stash_scroll = buffer.scroll as u32;
        self.help_buffer = Some(buffer);
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
    }

    /// Adopt a help buffer into the unified [`BufferRegistry`] and
    /// swap the active pane to it -- the in-pane counterpart to
    /// [`Self::open_help`]. Used by persistent help views (LSP logs,
    /// `:diagnostics`, `:apropos` once migrated) that should live as
    /// real buffers: split-able, switchable via `:bn` / `:b N`,
    /// listed by `:ls`.
    ///
    /// De-duplicates by title -- re-running the command surfaces the
    /// existing buffer rather than allocating a new one. Returns the
    /// `BufferId` either way so callers can wire follow-up state
    /// (Phase 4 live-tail subscriptions key off this id).
    ///
    /// **Hot-path model.** The registry entry is the durable record
    /// (`:ls` / `:bn` / picker discovery); the App's `help_buffer`
    /// slot mirrors the active in-pane help so the keymap +
    /// renderer stay single-path. Pane-switch hooks
    /// ([`Self::snapshot_active_pane`] / [`Self::load_active_pane`])
    /// sync the two at boundaries -- same pattern as Document's
    /// `syntax`/`folds` snapshots.
    pub(super) fn open_help_in_pane(&mut self, buffer: HelpBuffer) -> BufferId {
        if let Some(existing_id) = self.buffers.help_with_title(&buffer.title) {
            // Already open: refresh its content (so `:lsp-log` re-
            // run picks up new records) and switch the active pane
            // to it.
            if let Some(slot) = self.buffers.help_mut(existing_id) {
                *slot = buffer;
            }
            self.activate_help_in_pane(existing_id);
            return existing_id;
        }
        let id = BufferId::next();
        // Clone for the registry record; the active hot-path copy
        // lands on `self.help_buffer` via `activate_help_in_pane`.
        // HelpBuffer's heavy field is the rope (O(1) clone); the
        // markdown highlight Vec is the only allocation cost.
        // Note: `buffer.id` from `from_lines` and the registered
        // `id` here are intentionally different. The mismatch is
        // load-bearing for `activate_help_in_pane`'s
        // refresh-from-registry logic which fires when
        // `pane.buffer_id != help_buffer.id`. Production reader
        // sites that look up `buffer_locals` use
        // `pane.buffer_id` (the registered id), not `help.id`.
        let registry_copy = buffer.clone();
        self.buffers.insert(BufferEntry {
            id,
            flags: BufferFlags::default(),
            data: BufferData::Help(registry_copy),
        });
        // M.3.1: activate help-mode for this buffer so its
        // ReadOnly = true contribution lands in the resolved
        // options cache.
        self.activate_major_for_buffer_kind(id, BufferKind::Help);
        // M.3.2.b.1: mirror help-mode-owned data into the
        // buffer-locals map. The data is parsed at HelpBuffer
        // construction (links from markdown source, anchors
        // from headings, highlights from tree-sitter); this
        // step copies it into the typed-map so future reads
        // can transition off `HelpBuffer.X` and onto
        // `app.buffer_locals[id].get::<HelpLinks>()` etc.
        // (M.3.2.b.2 flips readers, then drops the fields
        // from `HelpBuffer`.)
        self.seed_help_locals(id, &buffer);
        // Take ownership of the original for the popup hot-path.
        self.help_buffer = Some(buffer);
        self.activate_help_in_pane(id);
        id
    }

    /// Mirror help-mode-owned data from a `HelpBuffer` into
    /// the buffer-locals map for `buffer_id`. Called at help-
    /// buffer creation time (M.3.2.b.1). Idempotent: a second
    /// call with the same buffer overwrites the prior locals
    /// since `BufferLocals::insert` is replace-on-collision.
    fn seed_help_locals(
        &mut self,
        buffer_id: BufferId,
        buffer: &HelpBuffer,
    ) {
        let locals = self
            .buffer_locals
            .entry(buffer_id)
            .or_default();
        locals.insert(crate::modes::HelpLinks(buffer.links.clone()));
        locals.insert(crate::modes::HelpAnchors(buffer.anchors.clone()));
        locals.insert(crate::modes::HelpHighlights(buffer.highlights.clone()));
    }

    pub(super) fn save_blocking(&mut self) -> Result<std::path::PathBuf, RuntimeError> {
        // BeforeSave fires before the actor commits, so a future
        // veto-class handler (§5.10.2) can format / sanitize the
        // buffer before it hits disk. v1 is observation-only, so
        // BeforeSave runs only for telemetry / autocmd compatibility.
        let snap = self.document.snapshot();
        if let Some(path) = snap.path.as_ref() {
            self.event_bus.publish(Event::BeforeSave {
                id: snap.id,
                path: (**path).clone(),
            });
        }
        // LSP textDocument/willSave (Phase 4.3) fan-out: every
        // server attached to the buffer that advertises the
        // notification gets a heads-up before the disk write.
        // Manual reason today (`TextDocumentSaveReason::Manual`).
        self.fire_will_save_notifications();
        // willSaveWaitUntil block-on-response (Phase 4.3).
        // Each server advertising the request returns a Vec<
        // TextEdit> the editor applies pre-save. Format-on-
        // save flows through here when the server emits one.
        // Bounded by a 500ms timeout so a buggy server can't
        // hang the save.
        self.run_will_save_wait_until_blocking();
        let result = block_on(self.document.save());
        if let Ok(path) = result.as_ref() {
            self.event_bus.publish(Event::DocumentSaved {
                id: snap.id,
                path: path.clone(),
            });
            // Fire didSave to every server that wants it.
            self.fire_did_save_notifications();
        }
        result
    }

    /// Walk the buffer's attached servers; fire
    /// `textDocument/willSave` to each that advertises it.
    /// Cheap on no-LSP buffers (the URI lookup short-circuits).
    /// Notification only -- responses, if any, drop on the floor.
    fn fire_will_save_notifications(&self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return;
        };
        let uri = uri.clone();
        let handles = self.lsp.servers_for(&uri);
        let params = lsp_types::WillSaveTextDocumentParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            reason: lsp_types::TextDocumentSaveReason::MANUAL,
        };
        for h in handles {
            if h.capabilities().wants_will_save() {
                let _ = h.will_save(params.clone());
            }
        }
    }

    /// Run `textDocument/willSaveWaitUntil` against every
    /// server advertising the request; collect their TextEdits
    /// and apply them pre-save.
    ///
    /// Audit slice 5 / M4: the previous shape iterated servers
    /// sequentially with a 500ms timeout per server, so total
    /// UI-thread block was up to `500ms × N`. New shape runs
    /// every server's request concurrently under one shared
    /// 500ms budget -- worst-case UI block is bounded at 500ms
    /// regardless of how many servers are attached. The
    /// remaining sync `block_on` is queued for the eventual
    /// two-phase save (kick off → return → drain on completion);
    /// the bounded-parallel fix covers the audit's actual
    /// concern (1.5s+ stalls for multi-server saves) without
    /// the behavioural change of fully-async save.
    fn run_will_save_wait_until_blocking(&mut self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id).cloned()
        else {
            return;
        };
        let handles = self.lsp.servers_for(&uri);
        let interested: Vec<lattice_lsp::ServerHandle> = handles
            .into_iter()
            .filter(|h| h.capabilities().wants_will_save_wait_until())
            .collect();
        if interested.is_empty() {
            return;
        }
        let params = lsp_types::WillSaveTextDocumentParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            reason: lsp_types::TextDocumentSaveReason::MANUAL,
        };
        // One cancellation token per request; on overall
        // timeout we cancel every in-flight one so slow servers
        // stop wasting the LSP runtime's worker time.
        let tokens: Vec<lattice_protocol::CancellationToken> = (0..interested.len())
            .map(|_| lattice_protocol::CancellationToken::new())
            .collect();
        let pending: Vec<_> = interested
            .iter()
            .zip(tokens.iter())
            .map(|(handle, token)| {
                handle.will_save_wait_until(params.clone(), token.clone())
            })
            .collect();
        let cancel_tokens = tokens.clone();
        let all_edits: Vec<lsp_types::TextEdit> = block_on(async move {
            // Spawn each request onto a `JoinSet` so they run
            // concurrently on the LSP runtime. The shared
            // 500ms deadline below caps the *total* UI-thread
            // block.
            let mut set: tokio::task::JoinSet<Vec<lsp_types::TextEdit>> =
                tokio::task::JoinSet::new();
            for fut in pending {
                set.spawn(async move {
                    fut.await.ok().flatten().unwrap_or_default()
                });
            }
            let deadline = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(deadline);
            let mut acc: Vec<lsp_types::TextEdit> = Vec::new();
            loop {
                tokio::select! {
                    next = set.join_next() => match next {
                        Some(Ok(edits)) => acc.extend(edits),
                        Some(Err(_)) => {} // task panicked; skip
                        None => break,     // every task done
                    },
                    _ = &mut deadline => {
                        // Bound the total UI-thread block at
                        // 500ms; any server still in flight
                        // gets cancelled so its response (if it
                        // eventually arrives) doesn't try to
                        // apply edits to a post-save buffer.
                        for t in &cancel_tokens { t.cancel(); }
                        set.abort_all();
                        break;
                    }
                }
            }
            acc
        });
        if !all_edits.is_empty() {
            // Apply pre-save edits as one undo unit. A failed
            // apply echoes but doesn't abort the save -- the
            // user's data still hits disk.
            if let Err(e) = self.apply_lsp_text_edits(all_edits) {
                self.set_message(
                    EchoLevel::Warn,
                    format!("willSaveWaitUntil: apply failed: {e}"),
                );
            }
        }
    }

    /// Walk the buffer's attached servers; fire
    /// `textDocument/didSave` to each that wants it. When the
    /// server requested `includeText`, attach the post-save
    /// text from the rope.
    fn fire_did_save_notifications(&self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return;
        };
        let uri = uri.clone();
        let handles = self.lsp.servers_for(&uri);
        let snap = self.document.snapshot();
        let full_text = snap.buffer.as_string();
        for h in handles {
            let caps = h.capabilities();
            if !caps.wants_did_save() {
                continue;
            }
            let text = if caps.did_save_include_text() {
                Some(full_text.clone())
            } else {
                None
            };
            let params = lsp_types::DidSaveTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: uri.clone(),
                },
                text,
            };
            let _ = h.did_save(params);
        }
    }

    pub(super) fn save_as_blocking(&self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        let snap = self.document.snapshot();
        self.event_bus.publish(Event::BeforeSave {
            id: snap.id,
            path: path.clone(),
        });
        let result = block_on(self.document.save_as(path.clone()));
        if result.is_ok() {
            self.event_bus
                .publish(Event::DocumentSaved { id: snap.id, path });
        }
        result
    }

    /// Vim's `<C-l>` -- force a fresh redraw to recover from any
    /// visual glitch. Concretely:
    ///
    /// - bumps the parsed-version mirror so the next
    ///   `maybe_reparse_syntax` actually re-runs the parser even if
    ///   the document version hasn't changed (covers the rare case
    ///   where a fold or syntax cache went stale);
    /// - clears the cached `visible_highlights` and pane highlights
    ///   so the next frame's `refresh_highlights` repopulates from
    ///   scratch;
    /// - sets `pending_redraw` so the runtime clears the terminal
    ///   on the next frame, scrubbing leftover ANSI sequences from
    ///   crashed external programs / partial repaints.
    pub(super) fn do_redraw_screen(&mut self) {
        // Force a syntax reparse on the next frame.
        self.last_parsed_text_version = u64::MAX;
        // Drop cached spans AND the cache key so
        // refresh_highlights's B.3 cache check sees a miss and
        // recomputes. Without clearing the key, the next
        // refresh_highlights computes the same key as the
        // previous frame (snapshot didn't change), hits the
        // cache, and returns the (now empty) `visible_highlights`
        // -- which manifests as syntax highlighting visibly
        // disappearing after `<C-l>` until the user scrolls (or
        // anything else invalidates the key). Regression test
        // pinned in `redraw_screen_repopulates_visible_highlights`.
        self.visible_highlights.clear();
        self.visible_highlights_key = None;
        self.pane_highlights.clear();
        // Recompute folds in case the fold set drifted from the
        // current document state (paranoia; the seam already runs
        // on every reparse, but `<C-l>` is the explicit "reset"
        // hook so we err on the side of re-running it).
        self.recompute_folds();
        // Tell the runtime to clear the terminal on next frame.
        self.pending_redraw = true;
        self.set_message(EchoLevel::Info, "redraw".to_string());
    }
}
