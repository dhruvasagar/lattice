//! The matrix: an ordered sequence of chunks the renderer slices.
//!
//! `CellMatrix` is the contract between the cell-builder worker
//! (S2 producer, in `lattice-host`) and the renderers (S3 TUI, S4
//! GPU consumers). Published wait-free via ArcSwap inside
//! `RenderState`; the paint loop reads it once per frame.
//!
//! See `docs/dev/architecture/cell-grid-renderer.md` § Paint loop
//! for the per-frame contract.

use std::sync::Arc;

use crate::chunk::CellChunk;
use crate::row::CellRow;
use crate::version::MatrixVersion;
use crate::virtual_rows::{AnchorPosition, VirtualRow, VirtualRowMatrix};

/// Sentinel value of [`CellMatrix::chunk_size`] meaning whole-doc
/// mode: the matrix has at most one chunk covering the entire
/// document. Below `4 × viewport_height` lines the cell-builder
/// uses whole-doc mode (see chunking policy in the design doc).
pub const CHUNK_SIZE_WHOLE_DOC: u32 = 0;

/// Soft-wrap (W.2, A2): number of display rows a row of `col_count`
/// columns occupies when wrapped at `wrap_width` columns. Floored at
/// `1` (an empty row still occupies one display row) and at `1` when
/// `wrap_width == 0` (wrapping off). Shared by the host scroll model
/// and both renderers so segment arithmetic is defined in exactly one
/// place.
pub fn wrap_segments(col_count: u32, wrap_width: u32) -> u32 {
    if wrap_width == 0 {
        return 1;
    }
    col_count.div_ceil(wrap_width).max(1)
}

/// The published cell matrix for a single buffer.
///
/// Immutable once built; cell-builder replaces the published Arc
/// when versions change. Cheap to clone (Arc bump).
#[derive(Clone, Debug)]
pub struct CellMatrix {
    /// Chunks ordered by `start_source_line`. Whole-doc mode has
    /// exactly one chunk.
    pub chunks: Arc<[Arc<CellChunk>]>,
    /// Logical lines per chunk, or [`CHUNK_SIZE_WHOLE_DOC`] for
    /// whole-doc mode.
    pub chunk_size: u32,
    /// Total logical lines in the source buffer (pre-fold).
    pub source_line_count: u32,
    /// Total matrix rows across all chunks (post-fold).
    pub visible_line_count: u32,
    /// Component-wise maximum of all chunks' captured versions.
    pub version: MatrixVersion,
    /// Soft-wrap (W.2, A2): the column width each source line is
    /// wrapped at when `:set wrap` is on, or `0` when wrapping is
    /// off (the historical default — every source line is exactly
    /// one display row). The cells worker stamps this from the
    /// pane's `viewport_width`; consumers derive display geometry
    /// via [`Self::segment_count`] without the matrix storing
    /// per-line segment data. One `CellRow` per source line is
    /// preserved either way — see
    /// `docs/dev/architecture/soft-wrap.md` (A2).
    pub wrap_width: u32,
}

impl Default for CellMatrix {
    /// Equivalent to [`Self::empty`]. Provided so containers like
    /// `Arc<ArcSwap<CellMatrix>>` derive `Default` without
    /// explicit-init plumbing at every call site.
    fn default() -> Self {
        Self::empty()
    }
}

impl CellMatrix {
    /// Empty matrix: no chunks, no rows. The initial published
    /// value before the cell-builder finishes its first build.
    pub fn empty() -> Self {
        Self {
            chunks: Arc::from([] as [Arc<CellChunk>; 0]),
            chunk_size: CHUNK_SIZE_WHOLE_DOC,
            source_line_count: 0,
            visible_line_count: 0,
            version: MatrixVersion::ZERO,
            wrap_width: 0,
        }
    }

    /// Construct a matrix in chunked mode. S2 is the production
    /// caller; `chunk_size` must be > 0. Use [`Self::whole_doc`]
    /// for the single-chunk mode.
    ///
    /// `version` is the aggregate stamp the renderer can inspect
    /// to detect "is this matrix newer than the one I painted last
    /// frame?" Comparison is `!=` (see `MatrixVersion::differs_from`)
    /// because some axes are hash-style and don't admit ordering.
    /// Production builds pass the publisher's current
    /// `MatrixVersion` snapshot; defaults to all-zero for empty /
    /// test cases.
    pub fn chunked(
        chunks: impl Into<Arc<[Arc<CellChunk>]>>,
        chunk_size: u32,
        source_line_count: u32,
        version: MatrixVersion,
    ) -> Self {
        assert!(chunk_size > 0, "chunked mode requires chunk_size > 0");
        let chunks: Arc<[Arc<CellChunk>]> = chunks.into();
        let visible_line_count = chunks.iter().map(|c| c.row_count()).sum::<u32>();
        Self {
            chunks,
            chunk_size,
            source_line_count,
            visible_line_count,
            version,
            wrap_width: 0,
        }
    }

    /// Construct a matrix in whole-doc mode (one chunk).
    pub fn whole_doc(chunk: Arc<CellChunk>, source_line_count: u32) -> Self {
        let visible_line_count = chunk.row_count();
        let version = chunk.version;
        Self {
            chunks: Arc::from(vec![chunk]),
            chunk_size: CHUNK_SIZE_WHOLE_DOC,
            source_line_count,
            visible_line_count,
            version,
            wrap_width: 0,
        }
    }

    /// `true` when running in whole-doc mode (one chunk covering
    /// the entire document).
    pub fn is_whole_doc(&self) -> bool {
        self.chunk_size == CHUNK_SIZE_WHOLE_DOC
    }

    /// `true` when no chunks/rows are present.
    pub fn is_empty(&self) -> bool {
        self.visible_line_count == 0
    }

    /// Soft-wrap (W.2, A2): how many *display* rows the source line
    /// `target` occupies. `1` when wrapping is off (`wrap_width ==
    /// 0`) or the line is missing/folded; otherwise
    /// `⌈col_count / wrap_width⌉`, floored at `1` (an empty line
    /// still occupies one display row).
    ///
    /// This is the published geometry the host scroll model
    /// (`bottom_anchored_scroll`) and both renderers read to expand
    /// a source line into wrap segments — no per-line segment data
    /// is stored on the matrix.
    pub fn segment_count(&self, target: u32) -> u32 {
        if self.wrap_width == 0 {
            return 1;
        }
        match self.row_at_source_line(target) {
            Some(row) => wrap_segments(row.col_count(), self.wrap_width),
            None => 1,
        }
    }

    /// S3.c.0 (2026-05-26): look up the row whose logical source
    /// line equals `target`. Returns `None` when the target line
    /// is folded (no visible row) or past the matrix's coverage.
    ///
    /// Walks chunks linearly; chunks are sorted by
    /// `start_source_line`. Most callers ask for visible lines
    /// (≤ viewport_height per frame); a typical 100K-line buffer
    /// at `chunk_size = 128` has ~780 chunks, so the walk is
    /// sub-µs even without an outer binary search.
    pub fn row_at_source_line(&self, target: u32) -> Option<&CellRow> {
        for chunk in self.chunks.iter() {
            let start = chunk.start_source_line;
            // Whole-doc mode (`chunk_size == 0`) — the single
            // chunk covers the entire source. Chunked mode — the
            // chunk covers `[start, start + chunk_size)`.
            let end = if self.chunk_size == CHUNK_SIZE_WHOLE_DOC {
                self.source_line_count
            } else {
                start.saturating_add(self.chunk_size)
            };
            if target < start {
                // chunks are ordered; the rest are even further
                // away.
                return None;
            }
            if target < end {
                return chunk.row_at_source_line(target);
            }
        }
        None
    }

    /// H.3 (2026-06-04): first source line this matrix's chunks were
    /// built to cover. `0` for whole-doc mode and full-coverage
    /// chunked mode; the window's lower bound for a windowed
    /// large-file matrix. Derived from the first chunk's
    /// `start_source_line` (exact — chunks are ordered).
    pub fn covered_start_line(&self) -> u32 {
        self.chunks
            .first()
            .map(|c| c.start_source_line)
            .unwrap_or(0)
    }

    /// H.3: exclusive upper bound of the source-line range this
    /// matrix's chunks were built to cover. `source_line_count` in
    /// whole-doc mode; otherwise the last chunk's
    /// `start_source_line + chunk_size`, clamped to
    /// `source_line_count`. Robust against fold elision (a fully
    /// folded tail chunk still reports the source span it was built
    /// over) and against a window that ends mid-`chunk_size`
    /// (`build_matrix` aligns the window up to `chunk_size`, so the
    /// last chunk's nominal end equals the window's upper bound).
    pub fn covered_end_line(&self) -> u32 {
        if self.is_whole_doc() {
            return self.source_line_count;
        }
        self.chunks
            .last()
            .map(|c| {
                c.start_source_line
                    .saturating_add(self.chunk_size)
                    .min(self.source_line_count)
            })
            .unwrap_or(0)
    }

    /// H.3: does this matrix cover the entire source-line range
    /// `[lo, hi)`? The cells worker's cache-hit gate uses this to
    /// keep a windowed large-file matrix from serving a viewport
    /// that has scrolled past its covered range — when it returns
    /// `false`, the worker rebuilds the window around the new
    /// scroll. An empty matrix (no chunks) covers nothing.
    pub fn covers(&self, lo: u32, hi: u32) -> bool {
        if self.chunks.is_empty() {
            return false;
        }
        self.covered_start_line() <= lo && hi <= self.covered_end_line()
    }

    /// Borrow the visible rows starting at matrix-row index
    /// `scroll`, up to `height` rows. Returns a [`CellSlice`] that
    /// iterates `&CellRow` references without allocating.
    ///
    /// If `scroll + height` exceeds `visible_line_count`, the
    /// slice is naturally truncated (no panic, no padding). The
    /// renderer must paint blank rows below the slice's end when
    /// the viewport extends past EOF.
    pub fn slice(&self, scroll: u32, height: u32) -> CellSlice<'_> {
        let start = scroll.min(self.visible_line_count);
        let end = scroll.saturating_add(height).min(self.visible_line_count);
        CellSlice {
            chunks: &self.chunks,
            start,
            end,
        }
    }

    /// D.0a: borrow `height` display rows starting at display
    /// row `scroll`, **interleaving** virtual rows from
    /// `virtual_rows` with the document rows in this matrix.
    ///
    /// `scroll` and `height` are in *display-row* space —
    /// they count both document rows and virtual rows.
    /// Returns a [`DisplaySlice`] that iterates
    /// [`DisplayRowEntry::Document`] for document rows and
    /// [`DisplayRowEntry::Virtual`] for virtual rows in
    /// natural top-to-bottom order.
    ///
    /// When `virtual_rows.is_empty()` the iterator degenerates
    /// to the same yield order as [`Self::slice`]; renderers
    /// can call `display_slice` unconditionally without
    /// paying for the interleaver when no provider has
    /// registered virtual rows.
    ///
    /// See `docs/dev/architecture/virtual-rows.md` for the
    /// full ordering contract (Above-before-Cell-before-Below
    /// at each anchor line, folded-line anchors emit at the
    /// next visible line, past-EOF anchors emit at the end).
    pub fn display_slice<'a>(
        &'a self,
        scroll: u32,
        height: u32,
        virtual_rows: &'a VirtualRowMatrix,
    ) -> DisplaySlice<'a> {
        DisplaySlice {
            chunks: &self.chunks,
            cell_total: self.visible_line_count,
            virtual_rows,
            scroll,
            height,
        }
    }
}

/// Borrowed iterator over a slice of matrix rows. Created via
/// [`CellMatrix::slice`].
///
/// The slice does not pre-resolve a flat `Vec<&CellRow>`; it walks
/// chunks lazily during iteration so the renderer pays only for
/// visible rows. Sub-microsecond for any realistic viewport.
#[derive(Debug)]
pub struct CellSlice<'a> {
    chunks: &'a [Arc<CellChunk>],
    /// Start index in *matrix-row* space (post-fold), inclusive.
    start: u32,
    /// End index in *matrix-row* space (post-fold), exclusive.
    end: u32,
}

impl<'a> CellSlice<'a> {
    /// Number of rows the slice will yield.
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// Iterate matrix rows in order.
    pub fn iter(&self) -> CellSliceIter<'a> {
        CellSliceIter::new(self.chunks, self.start, self.end)
    }
}

/// Iterator yielded by [`CellSlice::iter`]. Walks chunks from the
/// one containing `start` and yields `&CellRow` refs through to
/// `end`.
pub struct CellSliceIter<'a> {
    chunks: &'a [Arc<CellChunk>],
    chunk_idx: usize,
    /// Index into `chunks[chunk_idx].rows` for the next yield.
    row_idx_in_chunk: u32,
    /// Matrix-row index of the next yield (pre-increment).
    next_matrix_row: u32,
    /// Stop-at matrix-row index (exclusive).
    end: u32,
}

impl<'a> CellSliceIter<'a> {
    fn new(chunks: &'a [Arc<CellChunk>], start: u32, end: u32) -> Self {
        // Find the chunk + in-chunk offset corresponding to `start`.
        let mut acc: u32 = 0;
        let mut chunk_idx = chunks.len();
        let mut row_idx_in_chunk = 0u32;
        for (i, c) in chunks.iter().enumerate() {
            let next = acc.saturating_add(c.row_count());
            if start < next {
                chunk_idx = i;
                row_idx_in_chunk = start - acc;
                break;
            }
            acc = next;
        }
        Self {
            chunks,
            chunk_idx,
            row_idx_in_chunk,
            next_matrix_row: start,
            end,
        }
    }
}

impl<'a> Iterator for CellSliceIter<'a> {
    type Item = &'a CellRow;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_matrix_row >= self.end {
            return None;
        }
        while self.chunk_idx < self.chunks.len() {
            let chunk = &self.chunks[self.chunk_idx];
            if (self.row_idx_in_chunk as usize) < chunk.rows.len() {
                let row = &chunk.rows[self.row_idx_in_chunk as usize];
                self.row_idx_in_chunk += 1;
                self.next_matrix_row += 1;
                return Some(row);
            }
            // Walk to next non-empty chunk.
            self.chunk_idx += 1;
            self.row_idx_in_chunk = 0;
        }
        None
    }
}

// ============================================================
// D.0a: display rows — interleaver over (CellMatrix,
// VirtualRowMatrix). See
// `docs/dev/architecture/virtual-rows.md`.
// ============================================================

/// One row yielded by [`DisplaySliceIter`].
///
/// `Document` rows reference a [`CellRow`] from the underlying
/// [`CellMatrix`]; `Virtual` rows reference a [`VirtualRow`]
/// from the sibling [`VirtualRowMatrix`]. Renderers paint both
/// the same way (both carry `Arc<[Cell]>`), differing only in
/// the cursor / motion treatment (vim's `j` / `k` step
/// document rows only — virtual rows are visual-only).
#[derive(Debug, Clone, Copy)]
pub enum DisplayRowEntry<'a> {
    Document(&'a CellRow),
    Virtual(&'a VirtualRow),
}

/// Borrowed slice over the interleaved (document, virtual)
/// display rows. Created via [`CellMatrix::display_slice`].
///
/// Holds the parameters; the actual interleaving work happens
/// in [`Self::iter`] / [`DisplaySliceIter`]. Cheap to
/// construct.
#[derive(Debug)]
pub struct DisplaySlice<'a> {
    chunks: &'a [Arc<CellChunk>],
    cell_total: u32,
    virtual_rows: &'a VirtualRowMatrix,
    scroll: u32,
    height: u32,
}

impl<'a> DisplaySlice<'a> {
    /// Iterate `height` display rows starting at `scroll`.
    ///
    /// When `virtual_rows.is_empty()` the iterator walks the
    /// underlying `CellSliceIter` directly without
    /// interleaver overhead.
    pub fn iter(&self) -> DisplaySliceIter<'a> {
        let cells = if self.virtual_rows.is_empty() {
            // Fast path: no virtual rows, scroll counts cell
            // rows 1:1. Reuse CellSliceIter's chunk-walk
            // logic for the start position.
            let start = self.scroll.min(self.cell_total);
            CellSliceIter::new(self.chunks, start, self.cell_total)
        } else {
            CellSliceIter::new(self.chunks, 0, self.cell_total)
        };

        let mut it = DisplaySliceIter {
            cells,
            cell_peek: None,
            virtual_rows: &self.virtual_rows.rows,
            v_idx: 0,
            after_cell_below_for: None,
            remaining: u32::MAX,
        };

        // Skip `scroll` display rows when virtual rows are
        // present. Naive O(scroll); for v1 viewport sizes
        // (sub-frame budget at scroll < ~10k display rows)
        // this is well inside the per-frame budget. Optimised
        // skip via line_index lookup can replace this if a
        // bench surfaces it.
        if !self.virtual_rows.is_empty() {
            for _ in 0..self.scroll {
                if it.next().is_none() {
                    break;
                }
            }
        }
        it.remaining = self.height;
        it
    }
}

/// Iterator yielded by [`DisplaySlice::iter`]. Walks the
/// underlying [`CellSliceIter`] in tandem with the virtual
/// rows in `virtual_rows`, emitting them in the order:
///
/// 1. Virtual rows whose anchor is strictly less than the
///    next document row's source line.
/// 2. Virtual rows whose anchor equals the next document
///    row's source line and `position == Above`.
/// 3. The document row.
/// 4. Virtual rows whose anchor equals that document row's
///    source line and `position == Below`.
/// 5. Repeat from (1) with the next document row.
/// 6. After the last document row, any remaining virtual
///    rows are emitted in their sorted order (covers both
///    past-EOF anchors and anchors on folded-out trailing
///    lines).
pub struct DisplaySliceIter<'a> {
    cells: CellSliceIter<'a>,
    cell_peek: Option<&'a CellRow>,
    virtual_rows: &'a [VirtualRow],
    v_idx: usize,
    /// When `Some(line)`, the iterator just emitted the
    /// document row for `line` and is now draining its
    /// `Below(line)` virtual rows before peeking the next
    /// document row.
    after_cell_below_for: Option<u32>,
    remaining: u32,
}

impl<'a> Iterator for DisplaySliceIter<'a> {
    type Item = DisplayRowEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        // Each `next()` emits exactly one entry on a single straight-line
        // pass: phase A returns a queued `Below` row, else phase B/C/D
        // peeks the next document row and returns either the document row
        // or a virtual row anchored at/before it. No path loops back, so
        // this is straight-line, not a `loop` (clippy::never_loop).

        // Phase A: drain `Below(line)` virtual rows for the
        // most-recently emitted document row.
        if let Some(line) = self.after_cell_below_for {
            if let Some(vrow) = self.virtual_rows.get(self.v_idx) {
                if vrow.anchor_line == line && vrow.position == AnchorPosition::Below {
                    self.v_idx += 1;
                    self.remaining -= 1;
                    return Some(DisplayRowEntry::Virtual(vrow));
                }
            }
            // No more Below(line) entries — exit phase A.
            self.after_cell_below_for = None;
        }

        // Phase B: peek next document row if we haven't.
        if self.cell_peek.is_none() {
            self.cell_peek = self.cells.next();
        }

        match self.cell_peek {
            Some(crow) => {
                let line = crow.source_line;
                // Phase C: emit any virtual row whose anchor sits at or
                // before `line` with `Above`-or-earlier semantics.
                if let Some(vrow) = self.virtual_rows.get(self.v_idx) {
                    let v_anchor = vrow.anchor_line;
                    let emits_before_cell = v_anchor < line
                        || (v_anchor == line && vrow.position == AnchorPosition::Above);
                    if emits_before_cell {
                        self.v_idx += 1;
                        self.remaining -= 1;
                        return Some(DisplayRowEntry::Virtual(vrow));
                    }
                }
                // Phase D: emit the document row; queue `Below(line)` for
                // phase A on the next call.
                self.cell_peek = None;
                self.after_cell_below_for = Some(line);
                self.remaining -= 1;
                Some(DisplayRowEntry::Document(crow))
            }
            None => {
                // No more document rows; emit any remaining virtual rows
                // in sorted order.
                if let Some(vrow) = self.virtual_rows.get(self.v_idx) {
                    self.v_idx += 1;
                    self.remaining -= 1;
                    return Some(DisplayRowEntry::Virtual(vrow));
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    fn row(source_line: u32, ch: u8) -> CellRow {
        CellRow::new(
            vec![Cell::with_codepoint(ch as u32)],
            source_line,
            Vec::<crate::row::InlayOffset>::new(),
        )
    }

    fn chunk(start: u32, rows: Vec<CellRow>) -> Arc<CellChunk> {
        Arc::new(CellChunk::new(start, rows, MatrixVersion::ZERO))
    }

    #[test]
    fn wrap_segments_arithmetic() {
        // Wrap off ⇒ always one display row.
        assert_eq!(wrap_segments(0, 0), 1);
        assert_eq!(wrap_segments(200, 0), 1);
        // Empty / short rows ⇒ one row.
        assert_eq!(wrap_segments(0, 80), 1);
        assert_eq!(wrap_segments(1, 80), 1);
        assert_eq!(wrap_segments(80, 80), 1);
        // Exact multiples + remainders.
        assert_eq!(wrap_segments(81, 80), 2);
        assert_eq!(wrap_segments(160, 80), 2);
        assert_eq!(wrap_segments(161, 80), 3);
    }

    #[test]
    fn segment_count_reads_wrap_width() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let mut m = CellMatrix::whole_doc(c, 2);
        // Wrap off ⇒ 1 per line regardless of content.
        assert_eq!(m.segment_count(0), 1);
        // Turn wrap on at width 1: each 1-cell row is exactly one
        // segment; a missing line is neutral (1).
        m.wrap_width = 1;
        assert_eq!(m.segment_count(0), 1);
        assert_eq!(m.segment_count(99), 1);
    }

    #[test]
    fn empty_matrix_reports_empty() {
        let m = CellMatrix::empty();
        assert!(m.is_empty());
        assert!(m.is_whole_doc());
        assert_eq!(m.visible_line_count, 0);
        let s = m.slice(0, 10);
        assert!(s.is_empty());
        assert_eq!(s.iter().count(), 0);
    }

    #[test]
    fn whole_doc_holds_single_chunk() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b'), row(2, b'c')]);
        let m = CellMatrix::whole_doc(c, 3);
        assert!(m.is_whole_doc());
        assert_eq!(m.visible_line_count, 3);
        assert_eq!(m.source_line_count, 3);
        let s = m.slice(0, 10);
        let chars: Vec<u32> = s
            .iter()
            .map(|r| r.cells.first().map(|c| c.codepoint).unwrap_or(0))
            .collect();
        assert_eq!(chars, vec![b'a' as u32, b'b' as u32, b'c' as u32]);
    }

    #[test]
    fn slice_truncates_past_eof() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let m = CellMatrix::whole_doc(c, 2);
        let s = m.slice(0, 5);
        assert_eq!(s.len(), 2);
        assert_eq!(s.iter().count(), 2);
    }

    #[test]
    fn slice_with_scroll_past_eof_is_empty() {
        let c = chunk(0, vec![row(0, b'a')]);
        let m = CellMatrix::whole_doc(c, 1);
        let s = m.slice(10, 5);
        assert!(s.is_empty());
        assert_eq!(s.iter().count(), 0);
    }

    #[test]
    fn chunked_stores_passed_version() {
        let v = MatrixVersion {
            text: 3,
            syntax: 7,
            inlay_hints: 1,
            folds: 0,
            theme: 0,
            whitespace: 0,
        };
        let c1 = Arc::new(CellChunk::new(0, vec![row(0, b'a')], v));
        let c2 = Arc::new(CellChunk::new(1, vec![row(1, b'b')], v));
        let m = CellMatrix::chunked(vec![c1, c2], 1, 2, v);
        assert_eq!(m.version, v);
        assert_eq!(m.visible_line_count, 2);
    }

    #[test]
    fn slice_walks_across_chunks() {
        // Three chunks of one row each.
        let c1 = chunk(0, vec![row(0, b'a')]);
        let c2 = chunk(1, vec![row(1, b'b')]);
        let c3 = chunk(2, vec![row(2, b'c')]);
        let m = CellMatrix::chunked(vec![c1, c2, c3], 1, 3, MatrixVersion::ZERO);
        let s = m.slice(0, 3);
        let chars: Vec<u32> = s.iter().map(|r| r.cells[0].codepoint).collect();
        assert_eq!(chars, vec![b'a' as u32, b'b' as u32, b'c' as u32]);
    }

    #[test]
    fn slice_starting_mid_chunk() {
        let c1 = chunk(0, vec![row(0, b'a'), row(1, b'b'), row(2, b'c')]);
        let c2 = chunk(3, vec![row(3, b'd'), row(4, b'e')]);
        let m = CellMatrix::chunked(vec![c1, c2], 3, 5, MatrixVersion::ZERO);
        // Scroll past first two rows of chunk1; take 3 rows.
        let s = m.slice(2, 3);
        let chars: Vec<u32> = s.iter().map(|r| r.cells[0].codepoint).collect();
        assert_eq!(chars, vec![b'c' as u32, b'd' as u32, b'e' as u32]);
    }

    #[test]
    fn slice_handles_folded_rows_via_chunk_row_count() {
        // Chunk covers source lines 0..3 but only lines 0, 2 are
        // visible (line 1 is folded).
        let c = chunk(0, vec![row(0, b'a'), row(2, b'c')]);
        let m = CellMatrix::chunked(vec![c], 3, 3, MatrixVersion::ZERO);
        assert_eq!(m.visible_line_count, 2); // post-fold count
        let s = m.slice(0, 5);
        let source_lines: Vec<u32> = s.iter().map(|r| r.source_line).collect();
        assert_eq!(source_lines, vec![0, 2]);
    }

    #[test]
    fn slice_len_matches_iter_count() {
        let c1 = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let c2 = chunk(2, vec![row(2, b'c'), row(3, b'd'), row(4, b'e')]);
        let m = CellMatrix::chunked(vec![c1, c2], 2, 5, MatrixVersion::ZERO);
        for (scroll, height) in [(0, 5), (1, 3), (3, 10), (2, 2), (5, 5)] {
            let s = m.slice(scroll, height);
            assert_eq!(
                s.len() as usize,
                s.iter().count(),
                "scroll={scroll} height={height}"
            );
        }
    }

    #[test]
    fn slice_iter_walks_past_empty_chunks() {
        // Middle chunk is empty (e.g. covers a fully-folded range).
        let c1 = chunk(0, vec![row(0, b'a')]);
        let c2 = Arc::new(CellChunk::empty(1, MatrixVersion::ZERO));
        let c3 = chunk(2, vec![row(2, b'c')]);
        let m = CellMatrix::chunked(vec![c1, c2, c3], 1, 3, MatrixVersion::ZERO);
        let s = m.slice(0, 5);
        let chars: Vec<u32> = s.iter().map(|r| r.cells[0].codepoint).collect();
        assert_eq!(chars, vec![b'a' as u32, b'c' as u32]);
    }

    #[test]
    fn chunk_size_whole_doc_constant() {
        assert_eq!(CHUNK_SIZE_WHOLE_DOC, 0);
    }

    // ---- S3.c.0 — row_at_source_line ----

    /// Whole-doc matrix: every source line in `[0, count)` looks
    /// up to a row. Targets past `count` return `None`. Folded
    /// lines (absent from the chunk's rows vec) return `None`.
    #[test]
    fn row_at_source_line_whole_doc() {
        // Lines 0, 1, 3 visible; line 2 folded.
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b'), row(3, b'd')]);
        let m = CellMatrix::whole_doc(c, 4);
        assert!(m.is_whole_doc());

        assert_eq!(m.row_at_source_line(0).unwrap().source_line, 0);
        assert_eq!(m.row_at_source_line(1).unwrap().source_line, 1);
        // Folded — present in the source range but absent from
        // the chunk's row vector.
        assert!(m.row_at_source_line(2).is_none());
        assert_eq!(m.row_at_source_line(3).unwrap().source_line, 3);
        // Past EOF.
        assert!(m.row_at_source_line(4).is_none());
        assert!(m.row_at_source_line(100).is_none());
    }

    /// Chunked matrix: lookup walks chunks to the right one and
    /// then binary-searches inside. Spans across chunk boundaries.
    #[test]
    fn row_at_source_line_chunked_walks_chunks() {
        // Three chunks of width 2 covering lines 0..6.
        let c1 = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let c2 = chunk(2, vec![row(2, b'c'), row(3, b'd')]);
        let c3 = chunk(4, vec![row(4, b'e'), row(5, b'f')]);
        let m = CellMatrix::chunked(vec![c1, c2, c3], 2, 6, MatrixVersion::ZERO);

        for line in 0u32..6 {
            let r = m
                .row_at_source_line(line)
                .unwrap_or_else(|| panic!("expected row for line {line}"));
            assert_eq!(r.source_line, line);
        }
        // Past EOF.
        assert!(m.row_at_source_line(6).is_none());
    }

    /// Empty matrix never resolves any target.
    #[test]
    fn row_at_source_line_empty_matrix_returns_none() {
        let m = CellMatrix::empty();
        assert!(m.row_at_source_line(0).is_none());
        assert!(m.row_at_source_line(42).is_none());
    }

    // ============================================================
    // D.0a: display_slice / interleaver tests
    // ============================================================

    use crate::virtual_rows::{AnchorPosition, VirtualRow, VirtualRowMatrix, VirtualRowVersion};

    fn vrow(anchor: u32, position: AnchorPosition) -> VirtualRow {
        VirtualRow {
            anchor_line: anchor,
            position,
            cells: Arc::from([] as [Cell; 0]),
            height: 1,
            kind: crate::VirtualRowKind::Generic,
            bg: None,
            scales: None,
        }
    }

    /// Collect a DisplaySlice as a Vec<(kind, anchor_or_line)>
    /// for ergonomic test assertions. `kind` is 'D' for
    /// Document or 'V' for Virtual.
    fn collect(slice: &DisplaySlice<'_>) -> Vec<(char, u32)> {
        slice
            .iter()
            .map(|e| match e {
                DisplayRowEntry::Document(r) => ('D', r.source_line),
                DisplayRowEntry::Virtual(r) => ('V', r.anchor_line),
            })
            .collect()
    }

    #[test]
    fn display_slice_empty_virtual_matches_slice() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b'), row(2, b'c')]);
        let m = CellMatrix::whole_doc(c, 3);
        let v = VirtualRowMatrix::empty();
        let ds = m.display_slice(0, 10, &v);
        assert_eq!(collect(&ds), vec![('D', 0), ('D', 1), ('D', 2)]);
    }

    #[test]
    fn display_slice_above_emits_before_document_row() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let m = CellMatrix::whole_doc(c, 2);
        let v = VirtualRowMatrix::build(
            vec![vrow(1, AnchorPosition::Above)],
            2,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 10, &v);
        assert_eq!(collect(&ds), vec![('D', 0), ('V', 1), ('D', 1)]);
    }

    #[test]
    fn display_slice_below_emits_after_document_row() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let m = CellMatrix::whole_doc(c, 2);
        let v = VirtualRowMatrix::build(
            vec![vrow(0, AnchorPosition::Below)],
            2,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 10, &v);
        assert_eq!(collect(&ds), vec![('D', 0), ('V', 0), ('D', 1)]);
    }

    #[test]
    fn display_slice_multiple_at_same_anchor_sorted_above_then_below() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let m = CellMatrix::whole_doc(c, 2);
        let v = VirtualRowMatrix::build(
            vec![
                vrow(1, AnchorPosition::Below),
                vrow(1, AnchorPosition::Above),
                vrow(1, AnchorPosition::Above),
                vrow(1, AnchorPosition::Below),
            ],
            2,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 20, &v);
        // Expected ordering at anchor=1: two Above, then doc
        // row 1, then two Below.
        assert_eq!(
            collect(&ds),
            vec![
                ('D', 0),
                ('V', 1), // Above
                ('V', 1), // Above
                ('D', 1),
                ('V', 1), // Below
                ('V', 1), // Below
            ]
        );
    }

    #[test]
    fn display_slice_anchor_past_eof_emits_at_end() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let m = CellMatrix::whole_doc(c, 2);
        // VirtualRowMatrix::build clamps past-EOF anchors to
        // source_line_count, which sorts after the last
        // document row.
        let v = VirtualRowMatrix::build(
            vec![vrow(99, AnchorPosition::Above)],
            2,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 20, &v);
        assert_eq!(collect(&ds), vec![('D', 0), ('D', 1), ('V', 2)]);
    }

    #[test]
    fn display_slice_folded_line_emits_at_next_visible_row() {
        // Matrix has source lines [0, 2, 4] (lines 1 and 3
        // folded). Virtual rows anchored at 1 (Above) and 3
        // (Below) must emit at the next visible row -- they
        // can't sit at their original folded source line.
        let c = chunk(0, vec![row(0, b'a'), row(2, b'b'), row(4, b'c')]);
        let m = CellMatrix::whole_doc(c, 5);
        let v = VirtualRowMatrix::build(
            vec![
                vrow(1, AnchorPosition::Above),
                vrow(3, AnchorPosition::Below),
            ],
            5,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 20, &v);
        // (1, Above) emits before the next visible doc row
        // (source 2). (3, Below) emits before the next visible
        // doc row (source 4) because the would-be anchor line
        // 3 is folded out.
        assert_eq!(
            collect(&ds),
            vec![('D', 0), ('V', 1), ('D', 2), ('V', 3), ('D', 4),]
        );
    }

    #[test]
    fn display_slice_scroll_skips_display_rows() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b'), row(2, b'c')]);
        let m = CellMatrix::whole_doc(c, 3);
        let v = VirtualRowMatrix::build(
            vec![vrow(0, AnchorPosition::Below)],
            3,
            VirtualRowVersion(1),
        );
        // Unsliced order: D0, V0, D1, D2.
        let ds_0 = m.display_slice(0, 10, &v);
        assert_eq!(collect(&ds_0), vec![('D', 0), ('V', 0), ('D', 1), ('D', 2)]);

        // Scroll past D0 and V0: 2-row skip starts at D1.
        let ds_2 = m.display_slice(2, 10, &v);
        assert_eq!(collect(&ds_2), vec![('D', 1), ('D', 2)]);
    }

    #[test]
    fn display_slice_height_bounds_yielded_rows() {
        let c = chunk(0, vec![row(0, b'a'), row(1, b'b'), row(2, b'c')]);
        let m = CellMatrix::whole_doc(c, 3);
        let v = VirtualRowMatrix::build(
            vec![vrow(0, AnchorPosition::Below)],
            3,
            VirtualRowVersion(1),
        );
        // Unsliced order: D0, V0, D1, D2. height=2 yields the
        // first two.
        let ds = m.display_slice(0, 2, &v);
        assert_eq!(collect(&ds), vec![('D', 0), ('V', 0)]);
    }

    #[test]
    fn display_slice_empty_matrix_with_virtual_rows_emits_only_virtual() {
        let m = CellMatrix::empty();
        let v = VirtualRowMatrix::build(
            vec![
                vrow(0, AnchorPosition::Above),
                vrow(0, AnchorPosition::Below),
            ],
            0,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 10, &v);
        assert_eq!(collect(&ds), vec![('V', 0), ('V', 0)]);
    }

    #[test]
    fn display_slice_chunked_matrix_interleaves_correctly() {
        // Two chunks of size 2: rows [0, 1] and [2, 3].
        let c1 = chunk(0, vec![row(0, b'a'), row(1, b'b')]);
        let c2 = chunk(2, vec![row(2, b'c'), row(3, b'd')]);
        let m = CellMatrix::chunked(vec![c1, c2], 2, 4, MatrixVersion::ZERO);
        let v = VirtualRowMatrix::build(
            vec![
                vrow(1, AnchorPosition::Below),
                vrow(2, AnchorPosition::Above),
            ],
            4,
            VirtualRowVersion(1),
        );
        let ds = m.display_slice(0, 20, &v);
        assert_eq!(
            collect(&ds),
            vec![
                ('D', 0),
                ('D', 1),
                ('V', 1), // Below(1)
                ('V', 2), // Above(2)
                ('D', 2),
                ('D', 3),
            ]
        );
    }
}
