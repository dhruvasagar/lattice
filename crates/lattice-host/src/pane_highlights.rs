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

        // DR.1 (decoration-retention): don't clear + re-slice every
        // frame. Decide what actually changed FIRST (immutable reads
        // only), then take `&mut self.pane_highlights` solely when a
        // pane must be recomputed or a stale entry pruned — so a
        // no-op refresh leaves the `Versioned` map untouched (no
        // version bump, published Arc reused) and an unchanged
        // inactive pane keeps the spans it already had. The retention
        // key is `(buffer_id, scroll, syntax_snapshot.text_version)`:
        // it changes when the pane switches buffer, scrolls, or the
        // buffer's parsed tree advances (incl. an async reparse
        // landing after the buffer went inactive), and only then is
        // the slice recomputed.
        let qualifying: std::collections::HashSet<usize> =
            pending.iter().map(|(idx, ..)| *idx).collect();
        // (idx, doc_id, scroll, height, needs_reparse, key)
        let mut recompute: Vec<(usize, BufferId, u32, u32, bool, (BufferId, u32, u64))> =
            Vec::new();
        for &(idx, doc_id, scroll, height) in &pending {
            let Some(syntax) = self.document_syntax_for(doc_id) else {
                continue;
            };
            let snap_version = syntax.snapshot().text_version();
            let key = (doc_id, scroll, snap_version);
            if self.pane_highlight_keys.get(&idx).copied() == Some(key)
                && self.pane_highlights.contains_key(&idx)
            {
                // Unchanged — retain the cached spans untouched.
                continue;
            }
            let last_parsed = self.document_last_parsed_text_version_for(doc_id);
            let tv = self
                .buffers
                .document_handle(doc_id)
                .map(|h| h.snapshot().version)
                .unwrap_or(last_parsed);
            recompute.push((idx, doc_id, scroll, height, tv != last_parsed, key));
        }
        // Pane indices we hold spans for that no longer qualify
        // (became active, closed, or switched to a non-Document
        // buffer) must be dropped so a reused index never serves a
        // stale buffer's spans.
        let stale: Vec<usize> = self
            .pane_highlight_keys
            .keys()
            .copied()
            .filter(|idx| !qualifying.contains(idx))
            .collect();

        if recompute.is_empty() && stale.is_empty() {
            // Nothing changed: retain everything, no `&mut` to the
            // Versioned map, no version bump.
            return;
        }

        for idx in stale {
            self.pane_highlights.remove(&idx);
            self.pane_highlight_keys.remove(&idx);
        }
        for (idx, doc_id, scroll, height, needs_reparse, key) in recompute {
            let Some(syntax) = self.document_syntax_for(doc_id).cloned() else {
                continue;
            };
            if needs_reparse {
                // Slice B.2 part 2: inactive-pane path doesn't yet
                // accumulate per-document edit deltas (only the
                // active-pane path does). Empty edits → the worker
                // does a full reparse. Inactive-pane reparse is rare
                // (the buffer can't be edited while inactive; this
                // only fires for edits made just before it lost
                // focus) so the perf cost is bounded.
                let last_synced = self.document_last_synced_syntax_version_for(doc_id);
                if let Some(handle) = self.buffers.document_handle(doc_id) {
                    let snap = handle.snapshot();
                    let tv = snap.version;
                    syntax.request_reparse(last_synced, tv, snap.buffer.clone(), Vec::new());
                    // M.3.2.c.5: write the new baseline back into
                    // buffer_locals so subsequent reads see it.
                    let locals = self.buffer_locals.entry(doc_id).or_default();
                    locals.insert(DocumentLastParsedTextVersion(tv));
                    locals.insert(DocumentLastSyncedSyntaxVersion(tv));
                }
            }
            let end = scroll.saturating_add(height);
            let spans = syntax
                .snapshot()
                .highlight_lines(scroll, end)
                .unwrap_or_default();
            self.pane_highlights.insert(idx, spans);
            self.pane_highlight_keys.insert(idx, key);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use lattice_core::Document;

    /// DR.1 (decoration-retention): a no-op refresh — no qualifying
    /// inactive panes, nothing changed — must NOT touch the
    /// `Versioned` `pane_highlights` map. The pre-DR.1 code called
    /// `pane_highlights.clear()` unconditionally at the top of every
    /// refresh, bumping the version on every frame (and defeating the
    /// B.4 Arc-reuse) even when nothing changed. This locks the
    /// teardown removal: repeated refreshes are idempotent on the
    /// version counter.
    #[test]
    fn refresh_pane_highlights_no_op_does_not_bump_version() {
        let mut editor = Editor::boot(Document::empty());
        // First refresh settles any boot-time state.
        editor.refresh_pane_highlights();
        let v0 = editor.pane_highlights.version();
        // A second refresh with nothing changed must be a true no-op.
        editor.refresh_pane_highlights();
        let v1 = editor.pane_highlights.version();
        assert_eq!(
            v0, v1,
            "no-op refresh_pane_highlights must not bump the pane_highlights version \
             (pre-DR.1 it cleared + rebuilt every call)"
        );
        // And again, to be sure it's stable, not just slow to bump.
        editor.refresh_pane_highlights();
        assert_eq!(v1, editor.pane_highlights.version());
    }
}
