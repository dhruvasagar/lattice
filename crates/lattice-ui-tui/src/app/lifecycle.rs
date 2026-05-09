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

use super::App;
use crate::buffers::BufferKind;
use crate::pane::PaneDirection;

impl App {
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
