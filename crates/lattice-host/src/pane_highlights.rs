//! Per-pane syntax-highlight cache + refresh path for *inactive*
//! panes.
//!
//! The active pane's spans live in `editor.visible_highlights` —
//! built by [`Editor::refresh_highlights_window`]. Inactive panes
//! (split views showing a different document, or the same doc at
//! a different scroll) keep their cached spans in
//! `editor.pane_highlights` keyed by pane index.
//!
//! Phase 5.8.R: hoisted from `lattice-ui-tui::app::highlights::
//! App::refresh_pane_highlights` so the GPUI peer can paint
//! highlights on inactive panes too. The mode-state types this
//! reads (`DocumentLastParsedTextVersion`,
//! `DocumentLastSyncedSyntaxVersion`) live in [`crate::modes`];
//! the buffer-locals routing (`document_syntax_for`) is already
//! on `Editor`.

use lattice_core::{BufferId, BufferKind};

use crate::editor::Editor;
use crate::modes::{DocumentLastParsedTextVersion, DocumentLastSyncedSyntaxVersion};

impl Editor {
    /// Mode-owned `last_parsed_text_version` for `id`. Returns 0
    /// if the buffer is unknown or hasn't been parsed yet.
    /// Mirrors the App-side accessor; both peers reach the same
    /// value via this method.
    pub fn document_last_parsed_text_version_for(&self, id: BufferId) -> u64 {
        if id == self.document_buffer_id && matches!(self.active_buffer, BufferKind::Document) {
            return self.last_parsed_text_version;
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<DocumentLastParsedTextVersion>())
            .map(|v| v.0)
            .unwrap_or(0)
    }

    /// Mode-owned `last_synced_syntax_version` for `id`. Returns
    /// 0 if the buffer is unknown.
    pub fn document_last_synced_syntax_version_for(&self, id: BufferId) -> u64 {
        if id == self.document_buffer_id && matches!(self.active_buffer, BufferKind::Document) {
            return self.last_synced_syntax_version;
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<DocumentLastSyncedSyntaxVersion>())
            .map(|v| v.0)
            .unwrap_or(0)
    }

    /// Recompute per-pane highlights for inactive Document panes.
    /// Each inactive pane whose document `text_version` differs
    /// from the buffer-locals-cached version gets a reparse
    /// request fired, then `highlight_lines(scroll, scroll +
    /// viewport_height)` lands the visible-window slice in
    /// `pane_highlights[idx]`. The renderer reads from there to
    /// paint inactive panes.
    ///
    /// Active pane is skipped (it uses `visible_highlights`).
    /// Panes whose document is the SAME as the active document
    /// also fall through (one parse covers both panes' visible
    /// windows from `editor.syntax`).
    ///
    /// Phase 5.8.R: migrated from
    /// `lattice-ui-tui::app::highlights::App::refresh_pane_highlights`.
    /// The TUI peer's wrapper now delegates here; the GPUI peer
    /// calls it from `EditorView::render` to paint inactive-pane
    /// highlights.
    pub fn refresh_pane_highlights(&mut self) {
        self.pane_highlights.clear();
        let active_idx = self.pane_tree.active_index();
        let active_doc_id = if matches!(self.active_buffer, BufferKind::Document) {
            Some(self.document_buffer_id)
        } else {
            None
        };
        // Collect (pane_idx, doc_id, scroll, height) for each
        // inactive Document pane that doesn't share doc with the
        // active pane. Two-step (collect then iterate) avoids
        // borrowing `pane_tree` while we mutate `pane_highlights`.
        let pending: Vec<(usize, BufferId, u32, u32)> = self
            .pane_tree
            .leaves()
            .iter()
            .enumerate()
            .filter_map(|(idx, pane)| {
                if idx == active_idx {
                    return None;
                }
                if !matches!(pane.buffer, BufferKind::Document) {
                    return None;
                }
                if Some(pane.buffer_id) == active_doc_id {
                    return None;
                }
                let h = self.viewport_height;
                Some((idx, pane.buffer_id, pane.scroll, h))
            })
            .collect();
        for (idx, doc_id, scroll, height) in pending {
            let syntax = self.document_syntax_for(doc_id).cloned();
            let Some(syntax) = syntax else {
                continue;
            };
            let last_parsed = self.document_last_parsed_text_version_for(doc_id);
            let last_synced = self.document_last_synced_syntax_version_for(doc_id);
            let Some(handle) = self.buffers.document_handle(doc_id) else {
                continue;
            };
            let snap = handle.snapshot();
            let tv = snap.version;
            if tv != last_parsed {
                // Slice B.2 part 2: inactive-pane path doesn't
                // yet accumulate per-document edit deltas (only
                // the active-pane path does). Empty edits → the
                // worker does a full reparse. Inactive-pane
                // path is rare (only fires when a pane shows a
                // different document) so the perf cost is bounded.
                syntax.request_reparse(last_synced, tv, snap.buffer.clone(), Vec::new());
                // M.3.2.c.5: write the new baseline back into
                // buffer_locals so subsequent reads see it.
                let locals = self.buffer_locals.entry(doc_id).or_default();
                locals.insert(DocumentLastParsedTextVersion(tv));
                locals.insert(DocumentLastSyncedSyntaxVersion(tv));
            }
            let end = scroll.saturating_add(height);
            let spans = syntax
                .snapshot()
                .highlight_lines(scroll, end)
                .unwrap_or_default();
            self.pane_highlights.insert(idx, spans);
        }
    }
}
