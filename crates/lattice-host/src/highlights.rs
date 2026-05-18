//! Syntax highlight cache keys and helpers.
//! Renderer-agnostic; used by host-side Editor and renderers.

use lattice_syntax::SyntaxHandle;

use crate::editor::Editor;

/// Cache key for visible-highlights. The renderer paints spans every
/// frame; the expensive `highlight_lines` walk only runs when this key
/// changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleHighlightsKey {
    pub snapshot_ptr: usize,
    pub syntax_text_version: u64,
    pub scroll: u32,
    pub viewport_height: u32,
    pub fold_hash: u64,
}

impl Editor {
    /// Refresh the active-document visible-spans cache, gated on a
    /// content key. Caller provides `end_line` (exclusive upper
    /// bound of the highlight window) and `fold_hash` (caller-
    /// tracked closed-fold signature). The host walks the cache
    /// key, applies the stale-snapshot HOLD, and on a miss runs
    /// the `highlight_lines(scroll, end_line)` walk into
    /// [`Self::visible_highlights`].
    ///
    /// `syntax_handle` is the syntax to walk. Callers routing
    /// through buffer locals (TUI peer's `document_syntax_for`)
    /// resolve their per-buffer handle and pass it here; the GPUI
    /// peer (no buffer-locals routing yet) passes
    /// `self.editor.syntax.as_ref()`. When `None`, clears the
    /// cache + key so the next refresh recomputes against a fresh
    /// attach.
    ///
    /// Phase 5.8.G migration: the body used to live in
    /// `lattice-ui-tui::app::highlights::App::refresh_highlights`
    /// with App-specific fold + buffer-locals dependencies inlined.
    /// Hoisted host-side so the GPUI peer can reuse the cache
    /// without dragging fold machinery / locals routing into
    /// the renderer.
    ///
    /// Cost: cache-hit path is one key compare (~50ns). Cache-miss
    /// path dominated by `highlight_lines` (~178µs at 80 lines,
    /// Rust grammar). Steady state (cursor blink, no edits) → ~100%
    /// hit rate, so per-frame cost drops to the key compare.
    pub fn refresh_highlights_window(
        &mut self,
        syntax_handle: Option<&SyntaxHandle>,
        end_line: u32,
        fold_hash: u64,
    ) {
        let Some(syntax) = syntax_handle else {
            self.visible_highlights = Vec::new();
            self.visible_highlights_key = None;
            return;
        };
        let snap = syntax.snapshot();
        let key = VisibleHighlightsKey {
            snapshot_ptr: std::sync::Arc::as_ptr(&snap) as usize,
            syntax_text_version: snap.text_version(),
            scroll: self.scroll,
            viewport_height: self.viewport_height,
            fold_hash,
        };
        if self.visible_highlights_key == Some(key) {
            // Cache hit: existing visible_highlights is valid.
            return;
        }
        // Cache miss. Stale-snapshot HOLD: if the document has
        // advanced past the worker's published snapshot, hold the
        // existing (line-shifted) spans rather than recompute
        // against pre-edit data. The shifter (`shift_highlights_
        // for_edit`) keeps indices line-aligned during the worker
        // window, so unchanged-content lines stay correctly
        // colored continuously.
        if snap.text_version() < self.document.text_version() {
            self.visible_highlights_key = Some(key);
            return;
        }
        // Snapshot is current; recompute.
        let start = self.scroll;
        self.visible_highlights = snap.highlight_lines(start, end_line).unwrap_or_default();
        self.visible_highlights_key = Some(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use lattice_core::Document;

    /// `refresh_highlights_window(None, ...)` clears spans + key
    /// so a later attach repopulates the cache. Mirrors the
    /// "no syntax handle → clear cache" path from the original
    /// `App::refresh_highlights`.
    #[test]
    fn refresh_with_none_clears_cache() {
        let mut editor = Editor::boot(Document::empty());
        editor.visible_highlights = vec![Vec::new()];
        editor.visible_highlights_key = Some(VisibleHighlightsKey {
            snapshot_ptr: 0,
            syntax_text_version: 0,
            scroll: 0,
            viewport_height: 0,
            fold_hash: 0,
        });
        editor.refresh_highlights_window(None, 0, 0);
        assert!(editor.visible_highlights.is_empty());
        assert!(editor.visible_highlights_key.is_none());
    }
}
