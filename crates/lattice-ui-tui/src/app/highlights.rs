//! Tree-sitter syntax-highlight cache + per-frame refresh.
//!
//! Phase 5.8.AF.5 / Slice X2.6 retirement: the active-pane
//! visible-spans cache (`Editor::visible_highlights` + its
//! `VisibleHighlightsKey`) was deleted in favour of the worker-
//! published `Editor::syntax_visible_spans_cell`
//! (`lattice_host::highlights_worker`). Renderer paths now read
//! directly from that cell via `render_state.syntax.visible_spans`;
//! the App-side wrappers (`refresh_highlights`,
//! `highlights_for_viewport_row`, `highlights_for_buffer_line`,
//! `shift_highlights_for_edit`, `shift_spans_within_line`) were
//! removed with their host-side equivalents.
//!
//! What survives here:
//! - `refresh_pane_highlights` -- inactive Document panes still
//!   keep their own per-pane cache (the worker only publishes the
//!   active pane's spans); thin wrapper around the host body.
//! - `visible_buffer_line_extent` -- still used by inactive-pane
//!   path to stretch the highlight window past closed folds.

use lattice_syntax::StyledSpan;

use super::App;

impl App {
    /// X2.6 compatibility shim: drives the worker's synchronous
    /// `recompute` against the freshly-published `RenderState`.
    /// Production paint paths go through the async worker (woken
    /// by `Editor::highlight_wake` after every
    /// `publish_render_state`); the sync version preserves the
    /// pre-X2 contract that callers (tests, benches, anywhere off
    /// the per-frame hot path) can force-fill the span cell and
    /// immediately observe the result through
    /// `render_state.syntax.visible_spans.load()`.
    pub fn refresh_highlights(&mut self) {
        // Publish first so the worker reads the latest viewport /
        // scroll / fold-hash / text-version. (Production loop
        // publishes once per dispatch; tests calling this in
        // isolation need the explicit publish.)
        self.editor.publish_render_state();
        lattice_host::highlights_worker::recompute(
            &self.editor.render_state,
            &self.editor.syntax_visible_spans_cell,
        );
    }

    /// X2.6 compatibility shim: returns the worker-published spans
    /// for `line`, mapped through the active pane's `scroll`.
    /// Renderer-internal code reads through `FrameView` instead
    /// (already preloads the same Arc once per frame). This
    /// accessor stays so test helpers + the few non-renderer
    /// callers (e.g. fold-aware integration tests) don't break.
    pub fn highlights_for_buffer_line(&self, line: u32) -> Vec<StyledSpan> {
        let scroll = self.ad().scroll;
        if line < scroll {
            return Vec::new();
        }
        let offset = (line - scroll) as usize;
        let rs = self.editor.render_state.load_full();
        let spans = rs.syntax.visible_spans.load();
        spans
            .spans
            .get(offset)
            .cloned()
            .unwrap_or_default()
    }

    /// Last buffer-line index that ends up rendered when the
    /// viewport draws `height` rows starting at `scroll`,
    /// accounting for closed folds collapsing multiple buffer
    /// lines onto one row. Returns `scroll` itself when the
    /// viewport has zero height or the buffer is empty -- the
    /// caller's `+1` then yields a non-empty range so
    /// `highlight_lines` doesn't short-circuit.
    pub(crate) fn visible_buffer_line_extent(&self, scroll: u32, height: u32) -> u32 {
        let total_lines = self.editor.document.snapshot().buffer.line_count();
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

    /// Recompute per-pane highlights for inactive Document panes.
    /// Each inactive pane's `DocumentEntry::syntax` gets reparsed
    /// when the document's `text_version` differs from the entry's
    /// cached version (cheap: one parse per inactive pane per
    /// changed document); the visible-window slice lands in
    /// `Editor::pane_highlights` keyed by pane index. The renderer
    /// reads from there via `&App`.
    ///
    /// Active pane is skipped (it uses the worker-published
    /// `syntax_visible_spans_cell` directly via FrameView). Panes
    /// whose document is the same as the active document also
    /// fall through to the worker cell -- a single parse covers
    /// both panes.
    pub fn refresh_pane_highlights(&mut self) {
        // 5.8.R: cache-rebuild body migrated to
        // `lattice_host::editor::Editor::refresh_pane_highlights`
        // so the GPUI peer reaches the same path.
        self.editor.refresh_pane_highlights();
    }
}
