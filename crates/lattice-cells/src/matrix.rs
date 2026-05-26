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

/// Sentinel value of [`CellMatrix::chunk_size`] meaning whole-doc
/// mode: the matrix has at most one chunk covering the entire
/// document. Below `4 × viewport_height` lines the cell-builder
/// uses whole-doc mode (see chunking policy in the design doc).
pub const CHUNK_SIZE_WHOLE_DOC: u32 = 0;

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
}
