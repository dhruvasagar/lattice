//! Inactive-pane highlight-window helper.
//!
//! Phase 5.8.AF.5 / Slice X2.6 retirement: the active-pane
//! visible-spans cache (`Editor::visible_highlights` + its
//! `VisibleHighlightsKey`) was deleted in favour of the worker-
//! published span cell. display-line B4.2: that worker span cell
//! was itself deleted (the dead span/row prepaint cache); syntax
//! colour now flows through the cells / `DisplayMatrix` substrate,
//! and overlay backgrounds through `lattice_host::overlay_worker`.
//! The App-side span wrappers (`refresh_highlights`,
//! `highlights_for_viewport_row`, `highlights_for_buffer_line`,
//! `shift_highlights_for_edit`, `shift_spans_within_line`,
//! `refresh_pane_highlights`) were all removed with their backing
//! cells.
//!
//! What survives here:
//! - `visible_buffer_line_extent` -- computes the last buffer line
//!   rendered by a viewport, accounting for closed folds.

use super::App;

impl App {
    /// Last buffer-line index that ends up rendered when the
    /// viewport draws `height` rows starting at `scroll`,
    /// accounting for closed folds collapsing multiple buffer
    /// lines onto one row. Returns `scroll` itself when the
    /// viewport has zero height or the buffer is empty -- the
    /// caller's `+1` then yields a non-empty range so
    /// `highlight_lines` doesn't short-circuit.
    pub(crate) fn visible_buffer_line_extent(&self, scroll: u32, height: u32) -> u32 {
        // Slice 3c.final.E.1: read snapshot via the published
        // `ActiveDocumentRenderState` instead of
        // `self.editor.document.snapshot()`. Same Arc — `ad.snapshot`
        // is captured at publish time from the same handle.
        // CV.3: content space — this bounds a walk over painted lines.
        let total_lines = self.ad().snapshot.buffer.content_line_count();
        if total_lines == 0 {
            return scroll;
        }
        let mut buf_line = scroll;
        let mut row: u32 = 0;
        let mut last = scroll;
        while row < height && buf_line < total_lines {
            // Hidden interior of a closed fold -- still part of the
            // window the user is looking at (its content gets shown
            // via the fold heading), so include it in the highlight
            // range.
            if self.line_inside_closed_fold(buf_line) {
                last = buf_line;
                buf_line += 1;
                continue;
            }
            last = buf_line;
            if let Some(fold) = self.fold_start_at(buf_line) {
                last = fold.end_line;
                buf_line = fold.end_line + 1;
            } else {
                buf_line += 1;
            }
            row += 1;
        }
        last
    }

    // DR.2 (decoration-retention): the `refresh_pane_highlights` App
    // wrapper was retired with the `pane_highlights` producer. Inactive
    // panes render from their per-pane `DisplayMatrix` (built by the
    // cells worker for every visible pane); there is no per-frame
    // pane-highlight recompute to trigger.
}
