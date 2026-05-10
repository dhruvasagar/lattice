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

use lattice_core::buffer::AppliedEdit;
use lattice_core::{CoreError, Document, FoldMethod};
use lattice_grammar::register::Register;
use lattice_protocol::Event;
use lattice_protocol::position::Position;
use lattice_runtime::{RuntimeError, block_on, spawn_document};
use lattice_syntax::{Lang, Syntax};
use std::time::Duration;

use super::{App, BufferId, EchoLevel, PositionSource, PrevPaneState, preview_register};
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferKind};
use crate::help::HelpContent;
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
            // M.3.2.c.5: pull stashed mode-state out of buffer_locals
            // when re-activating a buffer the user just left for a
            // pane overlay. The `_is_some()` guard preserves the
            // help-overlay invariant -- when the active buffer
            // returns from a popup that didn't focus into help, no
            // sync happened, so locals are stale and we leave
            // App.syntax / App.folds untouched.
            let stashed_syntax = self
                .buffer_locals
                .get(&id)
                .and_then(|l| l.get::<crate::modes::DocumentSyntax>())
                .and_then(|s| s.0.clone());
            if stashed_syntax.is_some() {
                self.syntax = stashed_syntax;
                self.last_parsed_text_version = self
                    .buffer_locals
                    .get(&id)
                    .and_then(|l| l.get::<crate::modes::DocumentLastParsedTextVersion>())
                    .map(|v| v.0)
                    .unwrap_or(0);
                self.last_synced_syntax_version = self
                    .buffer_locals
                    .get(&id)
                    .and_then(|l| l.get::<crate::modes::DocumentLastSyncedSyntaxVersion>())
                    .map(|v| v.0)
                    .unwrap_or(0);
                self.folds = self
                    .buffer_locals
                    .get(&id)
                    .and_then(|l| l.get::<crate::modes::DocumentFolds>())
                    .map(|f| f.0.clone())
                    .unwrap_or_default();
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
        // M.3.2.c.5: pull stashed mode-state out of buffer_locals
        // (formerly held on `entry.syntax` / `entry.folds` etc.).
        // First activation has empty locals; `activate_buffer_state`
        // seeds via the foldmethod / reparse seam.
        self.syntax = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentSyntax>())
            .and_then(|s| s.0.clone());
        self.last_parsed_text_version = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastParsedTextVersion>())
            .map(|v| v.0)
            .unwrap_or(0);
        self.last_synced_syntax_version = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastSyncedSyntaxVersion>())
            .map(|v| v.0)
            .unwrap_or(0);
        self.folds = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentFolds>())
            .map(|f| f.0.clone())
            .unwrap_or_default();
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
            }),
        });
        // M.3.2.c.5: seed empty mode-state into the new buffer's
        // locals so reader accessors resolve uniformly.
        self.seed_empty_document_locals(new_id);
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
            // M.3.2.c.5: read dir through buffer_locals exclusively.
            let dir_display = self
                .buffer_locals
                .get(&oil_id)
                .and_then(|locals| locals.get::<crate::modes::OilDir>())
                .map(|d| d.0.display().to_string())
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

    /// `:q[uit]` -- vim-style window close. With multiple panes
    /// open, closes the active pane (no dirty check; the buffer
    /// lives on in the registry / other panes). With one pane
    /// left, runs the dirty guard against any document with
    /// unsaved changes and shuts down the editor. `force` (`!`)
    /// bypasses the dirty guard. Publishes `Event::BeforeQuit`
    /// for observability when the editor actually quits.
    pub(super) fn do_quit(&mut self, force: bool) {
        if self.pane_tree.len() > 1 {
            self.do_close_pane();
            return;
        }
        if !force {
            let dirty_id = self
                .buffers
                .document_ids_sorted()
                .into_iter()
                .find(|id| {
                    self.buffers
                        .document(*id)
                        .is_some_and(|d| d.handle.dirty())
                });
            if let Some(id) = dirty_id {
                self.set_message(
                    EchoLevel::Error,
                    format!(
                        "no write since last change for buffer #{} (add ! to override)",
                        id.0
                    ),
                );
                return;
            }
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
                BufferData::Oil(_) => {
                    // M.3.2.c.5: read dir through buffer_locals
                    // exclusively. The struct field stays as
                    // vestigial for tests.
                    let dir = self
                        .buffer_locals
                        .get(&id)
                        .and_then(|locals| locals.get::<crate::modes::OilDir>())
                        .map(|d| d.0.display().to_string())
                        .unwrap_or_default();
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} oil      {}",
                        id.0,
                        dir
                    ));
                }
            }
        }
        self.open_popup(
            HelpContent::from_lines("buffers", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            crate::popup::PopupPlacement::Centered,
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

    /// Look up a buffer by file path. Used by `:e FILE` to detect
    /// "already open"; later by `:b NAME` for completion.
    pub(super) fn find_document_by_path(&self, path: &std::path::Path) -> Option<BufferId> {
        self.buffers.document_with_path(path)
    }

    /// Save the currently-active document's hot-path state
    /// (`syntax`, `last_parsed_text_version`, `folds`) into its
    /// [`DocumentEntry`]. Called before switching the active
    /// buffer so the rotation is round-trippable.
    ///
    /// Guarded by `active_buffer == Document`: when the active
    /// buffer is a file tree or help, `self.syntax` was already
    /// moved into the document entry on the *previous* transition
    /// (when we left the document). Calling this again would
    /// `take()` an already-None value and overwrite the entry's
    /// stashed syntax, dropping the highlight state on the floor
    /// (the visible symptom: opening `:Tree` and pressing `q`
    /// returned to the document with no syntax colours).
    pub(super) fn snapshot_active_document(&mut self) {
        if !matches!(self.active_buffer, BufferKind::Document) {
            return;
        }
        // M.3.2.c.5: stash mode-state into buffer_locals (the
        // canonical home post-DocumentEntry-field-retirement).
        // Round-tripping every field — including
        // `last_synced_syntax_version` — preserves the syntax
        // worker baseline across a switch-away-and-back so an
        // out-of-order reparse race can't slip through.
        let id = self.document_buffer_id;
        let syntax = self.syntax.take();
        let last_parsed = self.last_parsed_text_version;
        let last_synced = self.last_synced_syntax_version;
        let folds = std::mem::take(&mut self.folds);
        let locals = self.buffer_locals.entry(id).or_default();
        locals.insert(crate::modes::DocumentSyntax(syntax));
        locals.insert(crate::modes::DocumentLastParsedTextVersion(last_parsed));
        locals.insert(crate::modes::DocumentLastSyncedSyntaxVersion(last_synced));
        locals.insert(crate::modes::DocumentFolds(folds));
    }

    /// Lifecycle hook fired after a document buffer becomes the
    /// active buffer (either via [`Self::activate_document`] or
    /// after `:e <path>` opens a fresh file). Refreshes anything
    /// that "lives with the buffer until it closes" so the user
    /// sees consistent state without having to reach for `<C-l>`.
    ///
    /// New buffer-level state plugs in here: keep the path
    /// principled instead of sprinkling per-option fixups across
    /// every entry point that changes the active buffer.
    pub(super) fn activate_buffer_state(&mut self) {
        // Make sure the syntax tree matches the current text. If
        // the entry stashed a parse for the document's current
        // version this no-ops; otherwise it parses + recomputes
        // folds in lockstep via the seam in `maybe_reparse_syntax`.
        self.maybe_reparse_syntax();
        // First-activation case: a freshly-opened file (or one we
        // never visited before) has an empty fold list and the
        // reparse seam may have been a no-op (text version already
        // matched the entry's stashed parse). Seed the fold list
        // from the active foldmethod so the gutter shows ▸ markers
        // and `za` works without a manual `<C-l>`. `Manual` skips
        // the seed (the user's `zf` ranges are authoritative).
        if self.folds.is_empty() && !matches!(self.foldmethod(), FoldMethod::Manual) {
            self.recompute_folds();
        }
        // Drop frame-level highlight caches so the next
        // `refresh_highlights` repopulates against the activated
        // buffer's content rather than the previous buffer's.
        self.visible_highlights.clear();
        self.pane_highlights.clear();
    }

    /// What `:bn` / `:bp` consider the "current" buffer for
    /// stepping. The active pane's buffer_id is the source of
    /// truth (the active pane is what the user sees).
    pub(super) fn active_pane_buffer_id(&self) -> BufferId {
        self.pane_tree.active().buffer_id
    }

    /// Copy the App's hot-path cursor / scroll into the active
    /// pane's stash. Called before any operation that flips which
    /// pane is active.
    ///
    /// **Unified hot-path**: `self.cursor` and `self.scroll` are
    /// the active buffer's regardless of kind, so the snapshot
    /// reads from there uniformly. Help / file-tree records are
    /// also synced into their kind-specific cursor / scroll fields
    /// (and the registry copy for help) so the archival state stays
    /// current; live state always lives on `self`.
    pub(super) fn snapshot_active_pane(&mut self) {
        let cursor = self.cursor;
        let scroll = self.scroll;
        let pane_id = self.pane_tree.active().buffer_id;
        // Mirror live state into the buffer-specific stash + the
        // registry record for archival / cross-pane round-trips.
        match self.active_buffer {
            BufferKind::Help => {
                if let Some(h) = self.help_buffer.as_mut() {
                    h.cursor = cursor;
                    h.scroll = scroll as usize;
                    if h.id == pane_id
                        && let Some(reg) = self.buffers.help_mut(pane_id)
                    {
                        *reg = h.clone();
                    }
                }
            }
            BufferKind::FileTree => {
                if let Some(t) = self.buffers.file_tree_mut(pane_id) {
                    t.cursor = cursor;
                    t.scroll = scroll as usize;
                }
            }
            BufferKind::Oil => {
                if let Some(o) = self.buffers.oil_mut(pane_id) {
                    o.cursor = cursor;
                    o.scroll = scroll as usize;
                }
            }
            BufferKind::Document => {}
        }
        let active = self.pane_tree.active_mut();
        active.cursor = cursor;
        active.scroll = scroll;
    }

    /// Build + publish [`Event::DocumentChanged`] from the current
    /// snapshot and the edits that were just applied. Called from
    /// every path that mutates the buffer (apply_edit / batch /
    /// undo / redo). The applied edits ride on the event so
    /// downstream subscribers (notably the per-server LSP fan-in)
    /// can sync without re-walking the buffer or holding the
    /// supervisor lock.
    pub(super) fn publish_document_changed(&mut self, applied: &[AppliedEdit]) {
        let snap = self.document.snapshot();
        let path = snap.path().map(|p| p.to_path_buf());
        let edits: Vec<lattice_protocol::event::AppliedEdit> = applied
            .iter()
            .map(|a| lattice_protocol::event::AppliedEdit {
                original_range: a.original_range,
                inserted_range: a.inserted_range,
                replaced_text: a.replaced_text.clone(),
                inserted_text: a.inserted_text.clone(),
            })
            .collect();
        self.event_bus.publish(Event::DocumentChanged {
            id: snap.id,
            path,
            version: snap.version,
            edits,
        });
        // Slice B.2 part 2: accumulate tree-sitter-shaped edit
        // deltas for the next syntax reparse request.
        // `maybe_reparse_syntax` drains this and ships them to
        // the worker, which applies them via tree.edit() before
        // running an incremental Parser::parse. If no syntax
        // handle is attached, skip the push to keep the vec
        // bounded.
        if self.syntax.is_some() {
            self.pending_syntax_edits
                .extend(applied.iter().map(|a| a.delta));
            // Slice C.3: shift `visible_highlights` synchronously
            // so line indices track the post-edit content even
            // before the worker publishes a fresh snapshot. For
            // line-deletes, this drains the deleted lines'
            // entries from the cached spans; for line-inserts,
            // it inserts empty placeholders. The result is that
            // unchanged-content lines below an edit keep their
            // (still-correct) spans at their NEW indices --
            // eliminating the "lines below the delete flicker"
            // user-visible symptom. Combined with the stale-
            // snapshot hold in `refresh_highlights`, the cached
            // spans never go through an empty/wrong intermediate
            // state during the worker window.
            for a in applied {
                self.shift_highlights_for_edit(&a.delta);
            }
        }
    }

    /// Build + publish [`Event::SelectionsChanged`] from the current
    /// snapshot. Called whenever the App's view of selections
    /// rotates (visual extension, dispatcher SelectionChange effect,
    /// `gv` reselect, etc.).
    pub(super) fn publish_selections_changed(&self) {
        let snap = self.document.snapshot();
        self.event_bus.publish(Event::SelectionsChanged {
            id: snap.id,
            version: snap.version,
            selections: (*snap.selections).clone(),
        });
    }

    /// Total area available to pane content in screen-cell units.
    /// Currently the buffer area = full terminal minus the mode
    /// line (1 row) and the echo / cmdline area (1 row). Width is
    /// the terminal width; v1 doesn't track terminal width as
    /// state, so we estimate from `viewport_height` and a constant
    /// width that the renderer overrides with the real terminal
    /// width before navigation. Good enough until B.1.c has the
    /// per-frame terminal size cached on App.
    pub(super) fn buffer_area_rect(&self) -> crate::pane::PaneRect {
        crate::pane::PaneRect {
            x: 0,
            y: 0,
            width: self.terminal_width.unwrap_or(120),
            height: self.viewport_height as u16,
        }
    }

    /// M.4: status-line label for a pane, dispatched on its
    /// `BufferKind`. Centralises the per-kind formatting so the
    /// renderer doesn't `match buffer.kind` directly. When mode-
    /// contributed status renderers land, this method dispatches
    /// through the active major mode instead of the kind enum.
    pub fn pane_status_label(&self, pane: &crate::pane::PaneState) -> String {
        match pane.buffer {
            BufferKind::Document => self
                .buffers
                .document(pane.buffer_id)
                .map(|e| {
                    let path = e
                        .handle
                        .path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "[no name]".to_string());
                    let dirty = if e.handle.dirty() { " [+]" } else { "" };
                    format!("{path}{dirty}")
                })
                .unwrap_or_else(|| "[no buffer]".to_string()),
            BufferKind::Help => self
                .help_buffer
                .as_ref()
                .map(|h| format!("[help] {}", h.title))
                .unwrap_or_else(|| "[help]".to_string()),
            BufferKind::FileTree => {
                let root = self
                    .buffer_locals
                    .get(&pane.buffer_id)
                    .and_then(|locals| locals.get::<crate::modes::FileTreeRoot>())
                    .map(|r| r.0.clone());
                root.map(|p| format!("[tree] {}", p.display()))
                    .unwrap_or_else(|| "[tree]".to_string())
            }
            BufferKind::Oil => self
                .buffers
                .oil(pane.buffer_id)
                .map(|o| {
                    let dirty = if o.is_dirty() { " [+]" } else { "" };
                    let dir = self
                        .buffer_locals
                        .get(&pane.buffer_id)
                        .and_then(|locals| locals.get::<crate::modes::OilDir>())
                        .map(|d| d.0.display().to_string())
                        .unwrap_or_default();
                    format!("[oil] {dir}{dirty}")
                })
                .unwrap_or_else(|| "[oil]".to_string()),
        }
    }

    /// Jump to `path:line:col` (LSP 0-based line, utf-8 byte
    /// column). Single entrypoint shared by the picker accept
    /// path (`JumpToLspLocation`) and the `do_help_follow_link`
    /// Source-link dispatch. Pushes the pre-jump cursor onto
    /// position history with `PluginPush` so `<C-o>` walks back.
    pub(super) fn jump_to_file_line_col(
        &mut self,
        path: &std::path::Path,
        line: u32,
        col: u32,
    ) {
        // Push pre-jump cursor before any state mutates.
        self.push_position_history(self.cursor, PositionSource::PluginPush);

        let same_buffer = self
            .document
            .path()
            .map(|p| p == path)
            .unwrap_or(false);
        if !same_buffer {
            self.do_edit(Some(path.to_path_buf()), false);
        }
        // Clamp the target line to the buffer's line count so a
        // stale picker entry doesn't crash with an out-of-range
        // cursor (e.g. user edited the file after the picker
        // populated). `last_addressable_line` accounts for
        // ropey's trailing-newline pseudo-line.
        let snap = self.document.snapshot();
        let line = line.min(super::last_addressable_line(&snap.buffer));
        let line_len = super::line_byte_len(&snap.buffer, line);
        let col = col.min(line_len);
        self.cursor = Position::new(line, col);
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
    pub(super) fn open_help_in_pane(&mut self, content: HelpContent) -> BufferId {
        let HelpContent { buffer, metadata } = content;
        if let Some(existing_id) = self.buffers.help_with_title(&buffer.title) {
            // Already open: refresh its content (so `:lsp-log` re-
            // run picks up new records) and switch the active pane
            // to it. Re-seed buffer_locals with the fresh metadata
            // so live-tail readers (link / anchor / highlights)
            // see the updated parse.
            if let Some(slot) = self.buffers.help_mut(existing_id) {
                *slot = buffer;
            }
            self.seed_help_metadata_locals(existing_id, metadata);
            self.activate_help_in_pane(existing_id);
            return existing_id;
        }
        let id = BufferId::next();
        // Clone for the registry record; the active hot-path copy
        // lands on `self.help_buffer` via `activate_help_in_pane`.
        // Note: `buffer.id` (the construction-time id) and the
        // registered `id` here are intentionally different. The
        // mismatch is load-bearing for `activate_help_in_pane`'s
        // refresh-from-registry logic which fires when
        // `pane.buffer_id != help_buffer.id`. Production reader
        // sites that look up `buffer_locals` use `pane.buffer_id`
        // (the registered id).
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
        // M.3.2.c.5: seed parsed metadata into buffer_locals
        // under the *registered* id (the locals key the renderer
        // and link-follow path resolve against).
        self.seed_help_metadata_locals(id, metadata);
        // Take ownership of the original for the popup hot-path.
        self.help_buffer = Some(buffer);
        self.activate_help_in_pane(id);
        id
    }

    /// M.3.2.c.5: seed an empty set of document mode-locals for a
    /// freshly-registered document buffer. Subsequent activation
    /// transitions read through these slots; if the slot is
    /// missing the accessor returns the type's natural default.
    /// Idempotent (replace-on-collision).
    pub(super) fn seed_empty_document_locals(&mut self, buffer_id: BufferId) {
        let locals = self.buffer_locals.entry(buffer_id).or_default();
        locals.insert(crate::modes::DocumentSyntax(None));
        locals.insert(crate::modes::DocumentLastParsedTextVersion(0));
        locals.insert(crate::modes::DocumentLastSyncedSyntaxVersion(0));
        locals.insert(crate::modes::DocumentFolds(Vec::new()));
    }

    /// M.3.2.c.4 mirror for the active document: copy the App's
    /// hot-path fields (`syntax`, `last_parsed_text_version`,
    /// `last_synced_syntax_version`, `folds`) into the buffer-
    /// locals map for `self.document_buffer_id`. Called from
    /// every site that mutates those fields so reader-side flips
    /// (M.3.2.c.4 follow-up + retirement) can resolve mode-owned
    /// state through `buffer_locals` uniformly across active /
    /// inactive buffers.
    #[allow(dead_code)]
    pub(super) fn seed_active_document_locals(&mut self) {
        if !matches!(self.active_buffer, BufferKind::Document) {
            return;
        }
        let id = self.document_buffer_id;
        let syntax = self.syntax.clone();
        let last_parsed = self.last_parsed_text_version;
        let last_synced = self.last_synced_syntax_version;
        let folds = self.folds.clone();
        let locals = self.buffer_locals.entry(id).or_default();
        locals.insert(crate::modes::DocumentSyntax(syntax));
        locals.insert(crate::modes::DocumentLastParsedTextVersion(last_parsed));
        locals.insert(crate::modes::DocumentLastSyncedSyntaxVersion(last_synced));
        locals.insert(crate::modes::DocumentFolds(folds));
    }

    // ---- M.3.2.c.4 reader accessors ----
    //
    // These resolve mode-owned document state through
    // `buffer_locals` so callers don't have to branch on
    // active-vs-inactive. The active buffer's hot-path fields
    // (`App.syntax`, `App.folds`, etc.) remain canonical;
    // locals mirror them at de-activation boundaries so reads
    // for inactive buffers route through this path uniformly.

    /// Mode-owned syntax handle for `id`. For the active
    /// document this is `App.syntax` (the live hot-path slot);
    /// for inactive documents it routes through `buffer_locals`.
    /// Returns `None` for `Lang::Plain` documents and for
    /// non-document buffers.
    pub(crate) fn document_syntax_for(
        &self,
        id: BufferId,
    ) -> Option<&lattice_syntax::SyntaxHandle> {
        if id == self.document_buffer_id
            && matches!(self.active_buffer, BufferKind::Document)
        {
            return self.syntax.as_ref();
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentSyntax>())
            .and_then(|s| s.0.as_ref())
    }

    /// Mode-owned fold list for `id`. Same active / inactive
    /// resolution as [`Self::document_syntax_for`]. Returns an
    /// empty slice for buffers that have no folds yet (or for
    /// non-document buffers).
    #[allow(dead_code)]
    pub(crate) fn document_folds_for(&self, id: BufferId) -> &[crate::app::Fold] {
        if id == self.document_buffer_id
            && matches!(self.active_buffer, BufferKind::Document)
        {
            return &self.folds;
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentFolds>())
            .map(|f| f.0.as_slice())
            .unwrap_or(&[])
    }

    /// Mode-owned `last_parsed_text_version` for `id`.
    #[allow(dead_code)]
    pub(crate) fn document_last_parsed_text_version_for(&self, id: BufferId) -> u64 {
        if id == self.document_buffer_id
            && matches!(self.active_buffer, BufferKind::Document)
        {
            return self.last_parsed_text_version;
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastParsedTextVersion>())
            .map(|v| v.0)
            .unwrap_or(0)
    }

    /// Mode-owned `last_synced_syntax_version` for `id`.
    #[allow(dead_code)]
    pub(crate) fn document_last_synced_syntax_version_for(&self, id: BufferId) -> u64 {
        if id == self.document_buffer_id
            && matches!(self.active_buffer, BufferKind::Document)
        {
            return self.last_synced_syntax_version;
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastSyncedSyntaxVersion>())
            .map(|v| v.0)
            .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::*;
    use crate::app::test_helpers::{app_with, attach_test_syntax, fresh_workspace, invoke_motion, set_rust_syntax, submit_ex, unique_tempdir, write_temp_file, write_workspace_config};
    use lattice_protocol::edit::Edit;

    #[test]
    fn maybe_reparse_syntax_drains_pending_edits_and_updates_version() {
        let mut a = app_with("hello", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        let initial_synced = a.last_synced_syntax_version;
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 1);
        // Drive the reparse-request seam directly (mirrors what
        // the runtime loop does at the end of each Action).
        a.maybe_reparse_syntax();
        // Edits drained.
        assert_eq!(a.pending_syntax_edits.len(), 0);
        // Version baseline advanced -- next request will use
        // this as `from_version`.
        assert!(a.last_synced_syntax_version > initial_synced);
        assert_eq!(a.last_synced_syntax_version, a.document.text_version());
    }

    #[test]
    fn edit_loads_named_file() {
        let dir = unique_tempdir();
        let path = dir.join("hello.txt");
        std::fs::write(&path, "loaded contents\nsecond line").unwrap();
        let mut a = app_with("original", 10);
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        assert_eq!(a.document.text(), "loaded contents\nsecond line");
        assert_eq!(a.cursor, Position::ZERO);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_refuses_when_dirty() {
        let mut a = app_with("modified", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.document.dirty());
        submit_ex(&mut a, "e /nonexistent");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        // Document unchanged.
        assert_eq!(a.document.text(), "Xmodified");
    }

    #[test]
    fn edit_force_overrides_dirty_guard() {
        let dir = unique_tempdir();
        let path = dir.join("forced.txt");
        std::fs::write(&path, "loaded").unwrap();
        let mut a = app_with("dirty content", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("Z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        let cmd = format!("e! {}", path.display());
        submit_ex(&mut a, &cmd);
        assert_eq!(a.document.text(), "loaded");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_preserves_registers_across_swap() {
        let dir = unique_tempdir();
        let path = dir.join("preserve.txt");
        std::fs::write(&path, "new content").unwrap();
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert!(a.unnamed_register.is_some());
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        // Register survives.
        assert!(a.unnamed_register.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_resets_per_document_state() {
        let dir = unique_tempdir();
        let path = dir.join("reset.txt");
        std::fs::write(&path, "fresh").unwrap();
        let mut a = app_with("aaa\nbbb\nccc", 10);
        a.cursor = Position::new(2, 1);
        a.apply(invoke_motion(a.builtins.goto_first_line));
        // Now position_history has an entry.
        assert!(!a.position_history.is_empty());
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        assert!(a.position_history.is_empty());
        assert_eq!(a.cursor, Position::ZERO);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_unknown_path_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "e /absolutely/does/not/exist/anywhere.txt");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        // Buffer unchanged.
        assert_eq!(a.document.text(), "hello");
    }

    #[test]
    fn split_pane_horizontal_creates_second_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneHorizontal);
        assert_eq!(a.pane_tree.len(), 2);
        // Active stays on original.
        assert_eq!(a.pane_tree.active_index(), 0);
    }

    #[test]
    fn split_pane_vertical_creates_second_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        assert_eq!(a.pane_tree.len(), 2);
    }

    #[test]
    fn close_pane_collapses_split() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::ClosePane);
        assert_eq!(a.pane_tree.len(), 1);
    }

    #[test]
    fn quit_with_multiple_panes_closes_active_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        assert_eq!(a.pane_tree.len(), 2);
        a.do_quit(false);
        assert!(!a.should_quit, "extra pane: :q must not exit the editor");
        assert_eq!(a.pane_tree.len(), 1);
    }

    #[test]
    fn quit_with_multiple_panes_skips_dirty_check() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.document.dirty());
        a.do_quit(false);
        assert!(!a.should_quit);
        assert_eq!(a.pane_tree.len(), 1);
    }

    #[test]
    fn quit_with_last_pane_clean_quits_editor() {
        let mut a = app_with("xx", 10);
        a.do_quit(false);
        assert!(a.should_quit);
    }

    #[test]
    fn quit_with_last_pane_dirty_refuses() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.do_quit(false);
        assert!(!a.should_quit);
        assert!(
            a.last_message
                .as_ref()
                .map(|m| m.text.contains("no write since last change"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn quit_force_with_last_pane_dirty_quits() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.do_quit(true);
        assert!(a.should_quit);
    }

    #[test]
    fn open_help_popup_preserves_doc_pane_cursor_for_render() {
        // Bug: invoking a popup-mode help command (`:lsp-status`,
        // `:describe-key`, etc.) flipped `active_buffer` to Help
        // without first syncing the doc's `app.cursor` /
        // `app.scroll` into the active pane's stash. The renderer
        // reads `pane.cursor` for any pane whose buffer kind
        // doesn't match `active_buffer` (popup mode = mismatch),
        // so the doc visibly jumped to wherever pane.cursor was
        // last (often (0,0)).
        let mut a = app_with("line0\nline1\nline2\nline3\nline4\n", 5);
        a.cursor = Position::new(3, 2);
        a.scroll = 1;
        a.do_lsp_status();
        // After open_help, active is Help but the active pane
        // still shows the doc -- pane.cursor must reflect where
        // the doc was, not the help buffer's (0,0).
        let pane = a.pane_tree.active();
        assert_eq!(
            pane.cursor,
            Position::new(3, 2),
            "doc's pre-help cursor must be stashed onto pane.cursor"
        );
        assert_eq!(pane.scroll, 1);
    }

    #[test]
    fn split_inherits_cursor_and_scroll_from_active() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.cursor = Position::new(2, 0);
        a.scroll = 1;
        a.apply(Action::SplitPaneVertical);
        // Both panes should have (line=2, scroll=1) initially.
        let panes = a.pane_tree.leaves();
        assert_eq!(panes[0].cursor.line, 2);
        assert_eq!(panes[0].scroll, 1);
        assert_eq!(panes[1].cursor.line, 2);
        assert_eq!(panes[1].scroll, 1);
    }

    #[test]
    fn edit_new_file_registers_a_second_buffer() {
        let path = write_temp_file("a", "alpha\n");
        let mut a = app_with("xx", 10);
        let initial_id = a.document_buffer_id;
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Both buffers exist; active switched to the new one.
        assert_eq!(a.buffers.document_ids_sorted().len(), 2);
        assert_ne!(a.document_buffer_id, initial_id);
        assert_eq!(a.document.text(), "alpha\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ls_renders_help_with_every_open_buffer() {
        let path = write_temp_file("c", "x\n");
        let mut a = app_with("xx", 10);
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        a.command_line = "ls".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("buffers help");
        let body = h.content.as_string();
        // Two buffers listed.
        assert!(body.contains("2 open buffer"));
        assert!(body.contains("2 document"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tree_open_makes_filetree_active() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("a.txt"), "alpha").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Filetree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.active_buffer, BufferKind::FileTree);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_close_returns_to_document() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-close-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Filetree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        a.apply(Action::HelpDismiss);
        assert_eq!(a.active_buffer, BufferKind::Document);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_motion_routes_through_active_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-motion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("a.txt"), "x").ok();
        std::fs::write(dir.join("b.txt"), "y").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Filetree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let line_down = a.builtins.line_down;
        a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        // After unification, `self.cursor` is the active buffer's
        // cursor. The tree's own `cursor` field is archival save-
        // state synced at activation transitions.
        assert_eq!(a.cursor.line, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_follow_on_file_opens_document_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("alpha.txt"), "hello").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Filetree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Move cursor to the alpha.txt entry (row 1).
        let line_down = a.builtins.line_down;
        a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        // Follow.
        a.apply(Action::FollowLink);
        // Active pane now shows the file's Document buffer; the
        // tree stays in the registry (reachable via :bn / :b).
        assert_eq!(a.active_buffer, BufferKind::Document);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        assert_eq!(a.document.text(), "hello");
        std::fs::remove_dir_all(&dir).ok();
    }


    #[test]
    fn open_help_in_pane_registers_buffer_and_activates_pane() {
        let mut app = app_with("hi\n", 5);
        let buf = HelpContent::from_lines(
            "test-help",
            vec!["# heading".into(), "body".into()],
        );
        let id = app.open_help_in_pane(buf);
        // Lives in the registry as a Help variant.
        assert!(app.buffers.help(id).is_some());
        // Active pane points at it.
        assert_eq!(app.active_pane_buffer_id(), id);
        assert!(matches!(app.active_buffer, BufferKind::Help));
        // Hot-path popup slot mirrors the registry copy.
        assert_eq!(
            app.help_buffer.as_ref().unwrap().title,
            "test-help"
        );
        // :ls walks the registry; help variants count.
        assert!(app.buffers.help_ids_sorted().contains(&id));
    }

    #[test]
    fn open_help_in_pane_dedups_by_title() {
        let mut app = app_with("hi\n", 5);
        let id1 = app.open_help_in_pane(HelpContent::from_lines(
            "lsp:rust",
            vec!["v1".into()],
        ));
        let id2 = app.open_help_in_pane(HelpContent::from_lines(
            "lsp:rust",
            vec!["v2 (refreshed)".into()],
        ));
        assert_eq!(id1, id2, "same title returns same BufferId");
        // Refresh path overwrote the body.
        let body = app.help_buffer.as_ref().unwrap().content.as_string();
        assert!(body.contains("refreshed"));
        // Single help entry in the registry.
        assert_eq!(app.buffers.help_ids_sorted().len(), 1);
    }

    #[test]
    fn active_pane_content_height_subtracts_status_row_in_horizontal_split() {
        // Single pane: content = full buffer height.
        let mut app = app_with("hi\n", 5);
        assert_eq!(app.active_pane_content_height(20), 20);
        // Horizontal split -> two panes, each ~half the buffer
        // height; minus the per-pane status row.
        app.pane_tree
            .split_active(crate::pane::SplitOrientation::Horizontal);
        let content = app.active_pane_content_height(20);
        // 20 / 2 = 10; minus status row = 9.
        assert_eq!(content, 9);
    }

    #[test]
    fn persistent_lsp_log_level_applies_from_toml_tree() {
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp]\nlog-level = \"debug\"\n";
        app.lsp_config_tree = toml_text.parse().expect("toml parse");
        app.apply_persistent_lsp_editor_options();
        // Effect: a Debug-level record on an unattached server lands
        // in the ring. Default min-level is Info; without the TOML
        // override the record would be filtered before it reached
        // the ring.
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Debug,
            lattice_lsp::LogSource::Client,
            "after-toml",
        );
        let recs = app.lsp_logger.snapshot_server(&id);
        assert!(
            recs.iter().any(|r| r.message == "after-toml"),
            "Debug record should pass through after TOML log-level=debug",
        );
    }

    #[test]
    fn persistent_lsp_log_level_warns_on_unknown_value() {
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp]\nlog-level = \"babble\"\n";
        app.lsp_config_tree = toml_text.parse().expect("toml parse");
        app.apply_persistent_lsp_editor_options();
        let msg = app.last_message.as_ref().expect("warn echo");
        assert!(
            msg.text.contains("lsp.log-level") && msg.text.contains("babble"),
            "echo should name the key + value, got {}",
            msg.text
        );
    }

    #[test]
    fn persistent_lsp_log_level_silent_when_missing() {
        let mut app = app_with("hi\n", 5);
        app.last_message = None;
        // Empty tree: nothing under [lsp].
        app.lsp_config_tree = toml::Table::new();
        app.apply_persistent_lsp_editor_options();
        assert!(
            app.last_message.is_none(),
            "no echo when key is absent (default applies)",
        );
    }



    #[test]
    fn load_persistent_config_applies_scalar_override_from_project_toml() {
        let ws = fresh_workspace("scalar-override");
        write_workspace_config(&ws, "tabstop = 4\n");
        let mut a = app_with("", 5);
        // tabstop default is 8; override should land before
        // first frame.
        assert_eq!(*a.config.get_typed::<lattice_config::Tabstop>().unwrap(), 8);
        a.load_persistent_config(Some(&ws));
        assert_eq!(*a.config.get_typed::<lattice_config::Tabstop>().unwrap(), 4);
    }

    #[test]
    fn load_persistent_config_buckets_per_language_section() {
        let ws = fresh_workspace("per-lang-bucket");
        write_workspace_config(
            &ws,
            "[completion.per-language.markdown]\n\
             auto_trigger = false\n\
             [completion.per-language.rust]\n\
             auto_trigger = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // Both per-language entries land in the structural
        // bucket, keyed by full dotted path.
        let paths = a.pending_structural_section_paths("completion.per-language");
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"completion.per-language.markdown".to_string()));
        assert!(paths.contains(&"completion.per-language.rust".to_string()));
        // Drain markdown -> sub-table accessible.
        let md = a
            .take_pending_structural_section("completion.per-language.markdown")
            .expect("markdown section drained");
        assert_eq!(
            md.get("auto_trigger").and_then(|v| v.as_bool()),
            Some(false),
        );
        // After drain, only rust remains.
        let after = a.pending_structural_section_paths("completion.per-language");
        assert_eq!(after, vec!["completion.per-language.rust".to_string()]);
    }

    #[test]
    fn load_persistent_config_collects_unknown_plugin_section_for_later_drain() {
        // Extensibility: a user writes `[plugin.X]` before the
        // plugin host exists. Loader buckets it; nothing warns;
        // the host (Phase 7) drains it when it registers.
        let ws = fresh_workspace("plugin-deferred");
        write_workspace_config(
            &ws,
            "[plugin.rust-analyzer]\nclippy = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        let paths = a.pending_structural_section_paths("plugin");
        assert_eq!(paths, vec!["plugin.rust-analyzer".to_string()]);
        let body = a
            .take_pending_structural_section("plugin.rust-analyzer")
            .expect("plugin section drained");
        assert_eq!(body.get("clippy").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn load_persistent_config_warning_surfaces_on_unknown_key() {
        let ws = fresh_workspace("unknown-key");
        write_workspace_config(&ws, "no_such_option = 42\n");
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // The echo carries the loader's warning.
        let msg = a.last_message.as_ref().expect("warning echoed");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("config:"), "got `{}`", msg.text);
        assert!(
            msg.text.contains("no_such_option"),
            "got `{}`",
            msg.text,
        );
    }

    #[test]
    fn tree_sitter_source_emits_definition_position_symbols_for_rust() {
        let source = "fn outer(arg: i32) {\n    let local = arg;\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor at end-of-buffer with empty query so every
        // candidate matches uniformly; the matcher won't drop
        // anything for prefix mismatch.
        a.cursor = Position::new(2, 1);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
        let ts_texts: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.source.as_ref().map(|s| s.as_str()) == Some(tree_sitter_id))
            .map(|c| c.text.as_str())
            .collect();
        for expected in &["outer", "arg", "local"] {
            assert!(
                ts_texts.contains(expected),
                "expected `{expected}` in tree-sitter candidates: {ts_texts:?}",
            );
        }
    }

    #[test]
    fn tree_sitter_source_skipped_by_per_language_override() {
        let source = "fn outer() {\n    let local = 1;\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(2, 1);
        // Override the active language (test buffer has no
        // path -> language id is "") to exclude tree-sitter.
        a.per_language_completion.insert(
            String::new(),
            lattice_completion::PerLanguageOverrides {
                sources: Some(vec![lattice_completion::SourceId::new(
                    lattice_completion::BufferWordsSource::ID,
                )]),
                ..Default::default()
            },
        );
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
        for cand in &state.raw {
            let src = cand.source.as_ref().map(|s| s.as_str()).unwrap_or("");
            assert_ne!(
                src, tree_sitter_id,
                "tree-sitter source filtered out for this language",
            );
        }
    }

    #[test]
    fn tree_sitter_and_buffer_words_emit_independently_for_same_name() {
        // `outer` appears as a function definition (captured
        // by tree-sitter) AND as a referenced word (captured
        // by buffer-words). Both sources contribute their
        // tagged copy in `state.raw` -- the producers run
        // independently. Visual dedup at the renderer (4.2.g.7
        // polish) collapses them to a single popup row, so
        // `state.rendered` has exactly one entry for `outer`,
        // tagged with the higher-priority source (buffer-words
        // at 100 > tree-sitter at 80).
        let source = "fn outer() {\n    outer();\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(2, 1);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let raw_sources: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.text == "outer")
            .map(|c| c.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert!(
            raw_sources.contains(&lattice_completion::BufferWordsSource::ID),
            "buffer-words copy present in raw set: {raw_sources:?}",
        );
        assert!(
            raw_sources.contains(&lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID),
            "tree-sitter copy present in raw set: {raw_sources:?}",
        );
        let rendered_outer: Vec<&str> = state
            .rendered
            .iter()
            .filter(|c| c.raw.text == "outer")
            .map(|c| c.raw.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(rendered_outer.len(), 1, "popup deduped to one row");
        assert_eq!(
            rendered_outer[0],
            lattice_completion::BufferWordsSource::ID,
            "higher-priority source's row survives the dedup",
        );
    }

    #[test]
    fn tree_sitter_source_silent_without_syntax_attached() {
        // No `set_rust_syntax` -> `app_with` leaves
        // `self.syntax = None`; tree-sitter source emits
        // nothing.
        let mut a = app_with("alpha bravo charlie", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 19);
        a.do_completion_trigger();
        if let Some(state) = a.insert_completion.as_ref() {
            let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
            for cand in &state.raw {
                assert_ne!(
                    cand.source.as_ref().map(|s| s.as_str()),
                    Some(tree_sitter_id),
                );
            }
        }
    }

    #[test]
    fn load_persistent_config_silent_when_no_files_present() {
        let ws = fresh_workspace("no-files");
        // Empty workspace -- no .lattice/config.toml. Loader
        // produces no messages; the modeline stays clean.
        let mut a = app_with("", 5);
        let prior = a.last_message.clone();
        a.load_persistent_config(Some(&ws));
        // No new echo (modeline message is whatever the test
        // setup left, which for app_with is None).
        assert_eq!(a.last_message, prior);
    }

    #[test]
    fn open_help_in_pane_seeds_help_locals() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines(
            "test-locals",
            vec![
                "# Heading One".to_string(),
                "see [ex:write](command:ex:write)".to_string(),
            ],
        );
        let help_id = a.open_help_in_pane(help);
        let locals = a
            .buffer_locals
            .get(&help_id)
            .expect("buffer_locals should be populated for help buffer");
        // Links parsed from `[ex:write](command:ex:write)`.
        let links = locals
            .get::<crate::modes::HelpLinks>()
            .expect("HelpLinks local seeded");
        assert_eq!(links.0.len(), 1);
        // Anchors come from heading slug generation. `from_lines`
        // doesn't auto-anchor headings (only
        // `from_lines_and_anchors` plumbs anchors); the seed
        // should still be present, just empty.
        let anchors = locals
            .get::<crate::modes::HelpAnchors>()
            .expect("HelpAnchors local seeded (possibly empty)");
        assert_eq!(anchors.0.len(), 0);
        // Highlights are empty without a markdown registry.
        let highlights = locals
            .get::<crate::modes::HelpHighlights>()
            .expect("HelpHighlights local seeded (possibly empty)");
        assert_eq!(highlights.0.len(), 0);
    }

    #[test]
    fn file_tree_locals_carry_owner_metadata() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-m3-2-c-2-meta-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let mut a = app_with("hi", 5);
        a.do_open_file_tree(Some(tmp.clone()));
        let tree_id = a.active_pane_buffer_id();
        let locals = a.buffer_locals.get(&tree_id).unwrap();
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        assert!(descriptors.len() >= 3);
        for d in &descriptors {
            assert_eq!(d.owner_mode, "file-tree-mode");
            assert!(
                d.name.starts_with("file-tree-mode."),
                "name {:?} should be namespaced under file-tree-mode",
                d.name
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn boot_seeds_initial_document_locals() {
        // M.3.2.c.4: the initial document buffer should have its
        // four mode-owned locals (DocumentSyntax, last_parsed,
        // last_synced, folds) seeded with empty defaults. Reader-
        // side flips later in this slice route through this map.
        let a = app_with("hello", 10);
        let id = a.document_buffer_id;
        let locals = a
            .buffer_locals
            .get(&id)
            .expect("initial document has buffer_locals");
        assert!(
            locals.get::<crate::modes::DocumentSyntax>().is_some(),
            "DocumentSyntax local seeded"
        );
        assert!(
            locals
                .get::<crate::modes::DocumentLastParsedTextVersion>()
                .is_some(),
            "DocumentLastParsedTextVersion local seeded"
        );
        assert!(
            locals
                .get::<crate::modes::DocumentLastSyncedSyntaxVersion>()
                .is_some(),
            "DocumentLastSyncedSyntaxVersion local seeded"
        );
        assert!(
            locals.get::<crate::modes::DocumentFolds>().is_some(),
            "DocumentFolds local seeded"
        );
    }

    #[test]
    fn snapshot_active_document_mirrors_into_locals() {
        // After de-activating a document (which moves App.syntax
        // / App.folds into entry.syntax / entry.folds), the
        // buffer-locals for that document should reflect the
        // entry's new contents.
        let mut a = app_with("hello\nworld", 10);
        let active_id = a.document_buffer_id;
        // Force a non-default fold so the mirror has something
        // to observe.
        a.folds.push(crate::app::Fold {
            start_line: 0,
            end_line: 1,
            closed: false,
            identity: None,
        });
        a.last_parsed_text_version = 42;
        a.last_synced_syntax_version = 41;
        a.snapshot_active_document();
        let locals = a
            .buffer_locals
            .get(&active_id)
            .expect("locals exist for active id");
        let parsed = locals
            .get::<crate::modes::DocumentLastParsedTextVersion>()
            .expect("last_parsed mirrored");
        let synced = locals
            .get::<crate::modes::DocumentLastSyncedSyntaxVersion>()
            .expect("last_synced mirrored");
        let folds = locals
            .get::<crate::modes::DocumentFolds>()
            .expect("folds mirrored");
        assert_eq!(parsed.0, 42);
        assert_eq!(synced.0, 41);
        assert_eq!(folds.0.len(), 1);
        assert_eq!(folds.0[0].start_line, 0);
    }

    #[test]
    fn popup_dismiss_does_not_jolt_backdrop_scroll() {
        // Open a centred popup over a long doc so the doc's scroll
        // sits at a non-zero baseline; dismiss; assert the doc's
        // scroll round-trips. The bug: `ensure_cursor_visible`
        // fires after dispatch with `viewport_height` still set to
        // the popup's small inner height, recomputing scroll
        // against the wrong viewport and shifting the backdrop.
        let many_lines: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut a = app_with(&many_lines, 30);
        // Park the cursor mid-document so scroll is non-zero.
        a.cursor = lattice_protocol::position::Position::new(40, 0);
        a.scroll = 30;
        let pre_cursor = a.cursor;
        let pre_scroll = a.scroll;
        // :lsp-status opens a centred popup (focuses Help mode).
        a.do_lsp_status();
        // viewport_height is now the popup's inner height (small).
        // Set it explicitly to mimic what runtime would do.
        a.set_viewport_height(
            a.help_popup_inner_height(30).unwrap_or(a.viewport_height),
        );
        // Dismiss the popup (the dispatch path calls this on Esc).
        a.dismiss_popup();
        // Now simulate what `apply` does post-dispatch: the fix is
        // that ensure_cursor_visible gets skipped on this transition,
        // so we don't even need to call it. Verify cursor + scroll
        // are restored to pre-popup values.
        assert_eq!(a.cursor, pre_cursor, "cursor restored");
        assert_eq!(a.scroll, pre_scroll, "scroll restored without jolt");
    }

    #[test]
    fn popup_with_long_content_scrolls_when_cursor_descends() {
        // Popup with 50 lines of content; focused (active_buffer =
        // Help). Step the cursor past the popup viewport's bottom
        // row and assert popup scroll advances to keep the cursor
        // visible. Goes through the input + keymap layer (`press`)
        // so the path matches what real `j` keystrokes hit, not
        // the App's direct-Invoke shortcut.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let lines: Vec<String> = (0..50).map(|i| format!("popup line {i}")).collect();
        let mut a = app_with("backdrop\n", 30);
        let buf = crate::help::HelpContent::from_lines("status", lines);
        a.open_popup(buf, crate::popup::PopupPlacement::Centered);
        // Mimic the runtime's per-frame viewport set: in popup
        // mode it's the popup's inner height, not the doc area's.
        let inner = a
            .help_popup_inner_height(30)
            .expect("popup inner height available");
        a.set_viewport_height(inner);
        // Press `j` enough times to step past the visible
        // viewport. Each iteration mirrors the runtime: refresh
        // viewport_height, then process the keystroke.
        for _ in 0..(inner + 5) {
            a.set_viewport_height(
                a.help_popup_inner_height(30).unwrap_or(a.viewport_height),
            );
            crate::app::test_helpers::press(
                &mut a,
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
            );
        }
        assert!(
            a.cursor.line >= inner,
            "cursor should descend past the visible viewport, got line {} (inner {})",
            a.cursor.line,
            inner
        );
        assert!(
            a.scroll > 0,
            "scroll should advance once cursor leaves the visible window, got scroll {}",
            a.scroll
        );
        assert!(
            a.cursor.line < a.scroll + inner,
            "cursor must still be inside the scrolled viewport (cursor {}, scroll {}, inner {})",
            a.cursor.line,
            a.scroll,
            inner
        );
    }

    #[test]
    fn document_syntax_for_inactive_resolves_through_locals() {
        // For an inactive document buffer the accessor must
        // resolve through buffer_locals (since `App.syntax` only
        // holds the active document's handle).
        use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
        use crate::buffers::{BufferFlags, BufferId};
        let mut a = app_with("active", 5);
        // Manufacture a second document buffer + seed empty
        // locals to validate the accessor path. Real `:e <new>`
        // does this through `do_edit`.
        let inactive_id = BufferId::next();
        let doc_handle = a.document.clone();
        a.buffers.insert(BufferEntry {
            id: inactive_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: inactive_id,
                handle: doc_handle,
            }),
        });
        a.seed_empty_document_locals(inactive_id);
        // Read for the inactive buffer flows through locals; syntax
        // is None which the accessor returns as None.
        assert!(
            a.document_syntax_for(inactive_id).is_none(),
            "accessor returns None for empty locals"
        );
        // last_parsed / last_synced come back as 0 on the inactive
        // buffer's empty locals.
        assert_eq!(a.document_last_parsed_text_version_for(inactive_id), 0);
        assert_eq!(a.document_last_synced_syntax_version_for(inactive_id), 0);
        // folds slice is empty.
        assert!(a.document_folds_for(inactive_id).is_empty());
    }

    #[test]
    fn document_locals_carry_owner_metadata() {
        let a = app_with("hi", 10);
        let id = a.document_buffer_id;
        let locals = a.buffer_locals.get(&id).unwrap();
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        for d in descriptors.iter().filter(|d| d.name.starts_with("text-mode.")) {
            assert_eq!(d.owner_mode, "text-mode");
        }
        // At minimum the four document locals.
        let names: Vec<_> = descriptors.iter().map(|d| d.name).collect();
        assert!(names.contains(&"text-mode.syntax"));
        assert!(names.contains(&"text-mode.last-parsed-text-version"));
        assert!(names.contains(&"text-mode.last-synced-syntax-version"));
        assert!(names.contains(&"text-mode.folds"));
    }

    #[test]
    fn open_oil_seeds_oil_locals() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-m3-2-c-3-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();

        let locals = a
            .buffer_locals
            .get(&oil_id)
            .expect("oil locals seeded");
        let dir = locals
            .get::<crate::modes::OilDir>()
            .expect("OilDir local present");
        assert_eq!(dir.0, tmp);

        // Owner-mode metadata.
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        let oil_descriptors: Vec<_> = descriptors
            .iter()
            .filter(|d| d.owner_mode == "oil-mode")
            .collect();
        assert_eq!(oil_descriptors.len(), 1);
        assert_eq!(oil_descriptors[0].name, "oil-mode.dir");

        let _ = std::fs::remove_dir_all(&tmp);
    }

}
