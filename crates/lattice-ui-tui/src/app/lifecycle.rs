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

use lattice_protocol::position::Position;

use super::{App, BufferId, EchoLevel, PositionSource, PrevPaneState};
use crate::buffers::BufferKind;
use crate::pane::PaneDirection;

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
