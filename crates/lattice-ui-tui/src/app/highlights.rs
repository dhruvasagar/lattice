//! Tree-sitter syntax-highlight cache + per-frame refresh.
//!
//! `App` owns two caches that drive the renderer's coloured
//! spans:
//! - `visible_highlights` -- the active pane's spans for
//!   `[scroll, scroll + viewport_height)`. Keyed by
//!   `VisibleHighlightsKey` so a steady-state cursor blink
//!   never re-walks the QueryCursor.
//! - `pane_highlights` -- inactive Document panes' spans,
//!   keyed by pane index. Recomputed only when the pane's
//!   document text version drifts from its entry.
//!
//! Methods that live here:
//! - `refresh_highlights` (active-pane cache build /
//!   stale-snapshot HOLD).
//! - `refresh_pane_highlights` (inactive Document panes).
//! - `highlights_for_viewport_row`,
//!   `highlights_for_buffer_line` (read accessors the
//!   renderer calls per row).
//! - `shift_highlights_for_edit` /
//!   `shift_spans_within_line` -- the post-edit pre-publish
//!   cache shifters that keep spans line-aligned and
//!   byte-aligned during the brief window before the
//!   syntax worker publishes a fresh snapshot. Pure
//!   ns-fast Vec splices.
//! - `visible_buffer_line_extent` (helper that stretches
//!   the highlight window past closed folds).
//!
//! What does NOT live here: the syntax actor itself
//! (`lattice-syntax`), the QueryCursor walk
//! (`Snapshot::highlight_lines`), the folds the extent
//! helper consults (those live in `app/folds.rs`).

use lattice_protocol::edit::EditDelta;
use lattice_syntax::StyledSpan;

use super::{App, BufferId, BufferKind, folds};

/// Cache key for `visible_highlights`. The renderer paints
/// the spans every frame; the actual `highlight_lines`
/// QueryCursor walk only fires when this key changes. So
/// the cursor blinks but nothing else changes, so the key stays
/// equal across frames and we never re-run the QueryCursor walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleHighlightsKey {
    pub(super) snapshot_ptr: usize,
    /// Syntax snapshot's `text_version` (== version the worker
    /// has parsed up to). Cache invalidates when the worker
    /// publishes a fresh tree -- that's the right trigger for
    /// re-highlighting. The document's own `text_version` is
    /// deliberately NOT in the key: between an edit and the
    /// worker's publish, the latest snapshot has no new
    /// information, so re-highlighting against it would just
    /// produce the same (slightly stale) result as the previous
    /// frame at the cost of a ~178µs walk. Letting the cache
    /// hold across that window keeps unchanged lines'
    /// highlighting continuous; only the just-edited line's
    /// spans are briefly at stale byte positions until the
    /// worker publishes.
    pub(super) syntax_text_version: u64,
    pub(super) scroll: u32,
    pub(super) viewport_height: u32,
    pub(super) fold_hash: u64,
}

impl App {
    /// Slice C.3: keep `visible_highlights` line-aligned with the
    /// current document immediately after an edit, before the
    /// syntax worker publishes a fresh snapshot.
    ///
    /// `visible_highlights` is indexed by viewport row =
    /// `buffer_line - scroll`. When an edit changes the line
    /// count (line-delete, line-insert, multi-line replace), the
    /// content at row N now corresponds to a different buffer
    /// line than before, but the cached span entries don't shift
    /// automatically. The renderer would paint pre-edit spans
    /// onto post-edit content, producing the user-reported "old
    /// span gaps appear as white characters on the new line"
    /// flicker.
    ///
    /// Fix: derive the line-shift from the delta's positions and
    /// apply it to `visible_highlights` as a Vec splice.
    /// Lines above the edit are untouched. Lines at and below
    /// the edit's start line are drained (delete) or padded with
    /// empty placeholders (insert) -- but unchanged lines further
    /// below still have correct spans at their NEW indices.
    ///
    /// Pure ns-fast: a Vec drain or insert of a few elements.
    /// Only mutates the cache; doesn't touch the snapshot.
    pub(super) fn shift_highlights_for_edit(&mut self, delta: &EditDelta) {
        let edit_start = delta.start_position.line;
        let scroll = self.scroll;
        if edit_start < scroll {
            // Edit started above the visible viewport. Bail and
            // let the worker's publish drive a normal recompute.
            return;
        }
        let viewport_idx = (edit_start - scroll) as usize;
        if viewport_idx >= self.visible_highlights.len() {
            // Edit started below the visible viewport. Nothing
            // visible changes.
            return;
        }
        let old_end = delta.old_end_position.line;
        let new_end = delta.new_end_position.line;
        let old_lines = old_end.saturating_sub(edit_start) as usize;
        let new_lines = new_end.saturating_sub(edit_start) as usize;
        if old_lines == new_lines {
            // In-line edit (line count unchanged). Shift spans
            // on the affected line by the byte delta within the
            // line so the held spans stay byte-aligned with the
            // new content. Without this, e.g. `>>` (insert "    "
            // at byte 0) leaves spans pointing at OLD byte
            // positions: the renderer paints "Keyword" color on
            // the new whitespace bytes 0..3 and leaves the
            // shifted "let" bytes 4..7 unstyled. When the worker
            // publishes the corrected spans on the next frame,
            // the bytes transition from "Keyword color on
            // whitespace" to "default color on whitespace" --
            // the "default color" reads as the visible flicker
            // the user reported.
            //
            // Slice C.4: shift each span by the byte delta:
            // - Entirely before the edit: unchanged.
            // - Entirely after the edit: both endpoints shifted.
            // - Crossing the edit point: extend (or contract) the
            //   end by the byte delta to keep the span covering
            //   the (now-resized) content. The start stays
            //   because the prefix bytes are preserved.
            self.shift_spans_within_line(viewport_idx, delta);
            return;
        }
        // Decide where to apply the shift. If the edit starts at
        // the very beginning of `start.line` (byte 0), then
        // `start.line`'s pre-edit content has moved -- it's now
        // located further down (for inserts) or has been
        // consumed (for deletes). The shift point IS
        // `viewport_idx`. If the edit starts mid-line or at
        // line-end (byte > 0), then `start.line`'s content (or
        // prefix) is preserved at `viewport_idx`; the shift
        // applies to the line AFTER it.
        //
        // Concrete impact:
        // - `O` (newline at line start, byte 0): insert at
        //   viewport_idx; original line spans move down.
        // - `o` (newline at line end, byte > 0): insert at
        //   viewport_idx + 1; original line spans preserved.
        // - `dd` (delete whole line, start byte 0):
        //   drain at viewport_idx; the deleted line's spans go.
        // - Backspace joining lines (delete \n at line end,
        //   start byte > 0): drain at viewport_idx + 1; the
        //   joined-into line's spans preserved.
        let action_idx = if delta.start_position.byte == 0 {
            viewport_idx
        } else {
            (viewport_idx + 1).min(self.visible_highlights.len())
        };
        if old_lines > new_lines {
            let to_remove = old_lines - new_lines;
            let drain_end = (action_idx + to_remove).min(self.visible_highlights.len());
            if action_idx < drain_end {
                self.visible_highlights.drain(action_idx..drain_end);
            }
        } else {
            let to_insert = new_lines - old_lines;
            for _ in 0..to_insert {
                self.visible_highlights.insert(action_idx, Vec::new());
            }
        }
    }

    /// Slice C.4: shift the spans on a single visible-line entry
    /// by the byte-delta of an in-line edit, so the held spans
    /// stay byte-aligned with the post-edit content during the
    /// brief window before the syntax worker publishes corrected
    /// spans. Eliminates the "spans paint on shifted bytes →
    /// recompute → bytes transition to default color" flicker
    /// that `>>` indents and other in-line edits produced.
    ///
    /// Three cases per span:
    /// 1. Entirely before the edit (`span.end <= edit_byte`):
    ///    unchanged.
    /// 2. Entirely after the edit (`span.start >= old_end_byte`):
    ///    both endpoints shift by `byte_delta`.
    /// 3. Crossing the edit (overlaps the edited range): the
    ///    prefix bytes [`span.start`, `edit_byte`) are unchanged,
    ///    so the span's start stays put. The end extends (or
    ///    contracts) by `byte_delta` to keep the span covering
    ///    its now-resized content. If the span collapses to
    ///    empty (delete consumed all of it), drop it.
    fn shift_spans_within_line(
        &mut self,
        viewport_idx: usize,
        delta: &EditDelta,
    ) {
        let edit_byte = delta.start_position.byte as usize;
        let old_end_byte = delta.old_end_position.byte as usize;
        let new_end_byte = delta.new_end_position.byte as usize;
        let byte_delta: i64 = new_end_byte as i64 - old_end_byte as i64;
        if edit_byte == old_end_byte && byte_delta == 0 {
            // No-op edit: empty range replaced with empty text.
            return;
        }
        let Some(line_spans) = self.visible_highlights.get_mut(viewport_idx) else {
            return;
        };
        line_spans.retain_mut(|span| {
            if span.end <= edit_byte {
                // Entirely before the edit; unchanged.
                true
            } else if span.start >= old_end_byte {
                // Entirely after the edit; shift both endpoints.
                let new_start = (span.start as i64) + byte_delta;
                let new_end = (span.end as i64) + byte_delta;
                span.start = new_start.max(0) as usize;
                span.end = new_end.max(0) as usize;
                true
            } else {
                // Span crosses the edit. Extend / contract end by
                // byte_delta to track the resized content; start
                // stays put (the prefix is preserved bytes).
                let extended_end = (span.end as i64) + byte_delta;
                if extended_end <= span.start as i64 {
                    // Span collapsed entirely (e.g. a multi-byte
                    // delete consumed the whole span). Drop.
                    false
                } else {
                    span.end = extended_end as usize;
                    true
                }
            }
        });
    }

    /// Refresh the active-pane visible-spans cache. Wraps the
    /// `Snapshot::highlight_lines` walk with a content-keyed
    /// short-circuit so steady-state frames pay roughly nothing.
    ///
    /// Cache key includes:
    /// - `Arc::as_ptr` of the syntax snapshot (changes when the
    ///   worker publishes a fresh tree).
    /// - The snapshot's `text_version` (so post-publish edits
    ///   that haven't reparsed yet still skip the walk -- see
    ///   the HOLD path below).
    /// - The viewport state (`scroll`, `viewport_height`).
    /// - A fold hash (closed-fold changes alter the visible
    ///   line set).
    ///
    /// On cache hit, returns immediately. On cache miss, decides
    /// between recompute and HOLD: when the document has
    /// advanced past the worker's published snapshot, the cached
    /// spans (kept aligned by `shift_highlights_for_edit`) stay
    /// in place until the worker publishes -- never paint
    /// through an empty intermediate. Recomputing in that
    /// window would walk against pre-edit data.
    ///
    /// Slice B.3 baseline: cache hit cost is one key compare +
    /// fold hash (~50ns). Cache miss cost dominated by
    /// `highlight_lines` (~178µs at 80 lines, rust grammar).
    /// The cache hits ~100% in steady state (cursor blink, no
    /// edits, no scroll), dropping per-frame cost from ~178µs
    /// to noise floor.
    ///
    /// Implementation note: when the caller hasn't attached a
    /// syntax handle (e.g. tests, boot before lang resolution),
    /// the active pane's spans clear to empty and the key
    /// resets, so the next reattach repopulates the cache.
    /// Without this reset, an attach-then-detach-then-attach
    /// pattern would compute against the new handle but key the
    /// result against the old one, producing a stale cache hit.
    /// Steady state (cursor blinking, no edit) → ~100% hit rate, dropping
    /// per-frame cost from ~178µs to noise floor (key compare +
    /// fold hash, ~50ns).
    pub fn refresh_highlights(&mut self) {
        let Some(syntax) = self.syntax.as_ref() else {
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
            fold_hash: folds::compute_fold_hash(&self.folds),
        };
        if self.visible_highlights_key == Some(key) {
            // Cache hit -- existing visible_highlights is valid.
            return;
        }
        // Cache miss. Decide between recompute and HOLD based on
        // whether the snapshot is current enough to give correct
        // spans.
        //
        // Slice C.3 stale-snapshot hold: if the document has
        // advanced past the worker's published snapshot, any
        // spans we compute would be against pre-edit data --
        // possibly producing wrong colors or wrong line counts
        // for the brief window before the worker publishes.
        // Instead, hold the existing visible_highlights (kept
        // line-aligned by `shift_highlights_for_edit` on edit)
        // and just update the key. The renderer paints the held
        // spans, which are byte-correct for unchanged content
        // and line-aligned even after line-deletes / inserts.
        //
        // When the worker publishes (snapshot_ptr changes),
        // we'll re-enter this path with a fresh snapshot and
        // recompute correctly. The spans only ever transition
        // from one CORRECT set to another -- never through an
        // empty/wrong intermediate that would visibly flicker.
        if snap.text_version() < self.document.text_version() {
            self.visible_highlights_key = Some(key);
            return;
        }
        // Snapshot is current with the document. Recompute.
        // The window stretches via `visible_buffer_line_extent`
        // to cover lines under closed folds (see method
        // docstring).
        let start = self.scroll;
        let end = self
            .visible_buffer_line_extent(start, self.viewport_height)
            .saturating_add(1);
        self.visible_highlights = snap
            .highlight_lines(start, end)
            .unwrap_or_default();
        self.visible_highlights_key = Some(key);
    }

    /// Last buffer-line index that ends up rendered when the
    /// viewport draws `height` rows starting at `scroll`,
    /// accounting for closed folds collapsing multiple buffer
    /// lines onto one row. Returns `scroll` itself when the
    /// viewport has zero height or the buffer is empty -- the
    /// caller's `+1` then yields a non-empty range so
    /// `highlight_lines` doesn't short-circuit.
    fn visible_buffer_line_extent(&self, scroll: u32, height: u32) -> u32 {
        let total_lines = self.document.snapshot().buffer.line_count();
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
    /// Each inactive pane's [`DocumentEntry::syntax`] gets reparsed
    /// when the document's `text_version` differs from the entry's
    /// cached version (cheap: one parse per inactive pane per
    /// changed document); the visible-window slice lands in
    /// [`Self::pane_highlights`] keyed by pane index. The renderer
    /// reads from there via `&App`.
    ///
    /// Active pane is skipped (it uses [`Self::visible_highlights`]
    /// directly). Panes whose document is the same as the active
    /// document also fall through to `visible_highlights` -- a
    /// single parse covers both panes.
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
        // active pane.
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
                // Use the pane's own viewport slice (the per-pane
                // status line eats one row, so subtract; for v1
                // we approximate using app.viewport_height).
                let h = self.viewport_height;
                Some((idx, pane.buffer_id, pane.scroll, h))
            })
            .collect();
        for (idx, doc_id, scroll, height) in pending {
            let Some(entry) = self.buffers.document_mut(doc_id) else {
                continue;
            };
            let snap = entry.handle.snapshot();
            let tv = snap.version;
            if entry.syntax.is_none() {
                continue;
            }
            if let Some(syntax) = entry.syntax.as_ref() {
                if tv != entry.last_parsed_text_version {
                    // Slice B.2 part 2: inactive-pane path
                    // doesn't yet accumulate per-document edit
                    // deltas (the active-pane path does, on
                    // App.pending_syntax_edits). For now we send
                    // empty edits which routes the worker to
                    // full reparse. Per-DocumentEntry edit
                    // accumulation is its own follow-up; the
                    // inactive-pane path is rare (only fires
                    // when pane shows a different document) so
                    // the perf cost stays bounded.
                    // Slice B.5: pass Buffer (O(1) clone) instead
                    // of pre-materializing the String here.
                    syntax.request_reparse(
                        entry.last_synced_syntax_version,
                        tv,
                        snap.buffer.clone(),
                        Vec::new(),
                    );
                    entry.last_parsed_text_version = tv;
                    entry.last_synced_syntax_version = tv;
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

    /// Spans for the line at `viewport_row` (0-based, relative to the top of
    /// the viewport). Empty slice if no syntax or the row is past EOF.
    ///
    /// Prefer [`Self::highlights_for_buffer_line`] when the renderer
    /// is iterating the visible-line list under closed folds, since
    /// `viewport_row` no longer maps to `scroll + row` once folds
    /// hide interior lines.
    pub fn highlights_for_viewport_row(&self, viewport_row: u32) -> &[StyledSpan] {
        self.visible_highlights
            .get(viewport_row as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Spans for a specific buffer line. `refresh_highlights` populates
    /// `visible_highlights` for the contiguous window
    /// `[scroll, scroll + viewport_height)`; lines outside that window
    /// (or far enough that the slot is missing) return an empty slice.
    /// The renderer uses this for the active pane so closed folds
    /// don't desync syntax styling -- viewport row 5 might be buffer
    /// line 12 once a fold collapses lines 5..=11.
    pub fn highlights_for_buffer_line(&self, line: u32) -> &[StyledSpan] {
        if line < self.scroll {
            return &[];
        }
        let offset = (line - self.scroll) as usize;
        self.visible_highlights
            .get(offset)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
