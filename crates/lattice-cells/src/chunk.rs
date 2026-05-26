//! Chunk of contiguous matrix rows.
//!
//! A `CellChunk` is the unit of cache + rebuild for the cell-grid
//! renderer. Edits invalidate one or two chunks (the ones
//! intersecting the change range); downstream chunks (lines past
//! the edit) have their `start_source_line` shifted by `Δ`
//! without rebuild.
//!
//! See `docs/dev/architecture/cell-grid-renderer.md` § Chunking
//! policy for sizing rules (`chunk_size = 2 × viewport_height`,
//! whole-doc mode below `4 × viewport_height`).

use std::sync::Arc;

use crate::row::CellRow;
use crate::version::MatrixVersion;

/// Contiguous range of matrix rows covering a slice of the buffer.
///
/// Invariants (S2 enforces; S1 documents):
/// - `rows` is sorted ascending by `source_line`.
/// - Folded source lines do not appear; row count is post-fold.
/// - `start_source_line` is the FIRST source line that *could*
///   appear in this chunk's range. The chunk's range is
///   `[start_source_line, start_source_line + chunk_size)` in
///   *logical* (pre-fold) source-line space. Some of those
///   source lines may be folded and therefore absent from `rows`.
/// - `version` is the `MatrixVersion` snapshot captured at build
///   time. Cell-builder compares against current RenderState
///   version to decide if this chunk needs rebuild.
#[derive(Clone, Debug)]
pub struct CellChunk {
    pub start_source_line: u32,
    pub rows: Arc<[CellRow]>,
    pub version: MatrixVersion,
}

impl CellChunk {
    /// Construct a chunk. S2 is the production caller.
    pub fn new(
        start_source_line: u32,
        rows: impl Into<Arc<[CellRow]>>,
        version: MatrixVersion,
    ) -> Self {
        Self {
            start_source_line,
            rows: rows.into(),
            version,
        }
    }

    /// Empty chunk anchored at `start_source_line`. Used when the
    /// covered source-line range is entirely folded (no visible
    /// rows) or as a placeholder during incremental build.
    pub fn empty(start_source_line: u32, version: MatrixVersion) -> Self {
        Self::new(start_source_line, Arc::from([] as [CellRow; 0]), version)
    }

    /// Number of matrix rows this chunk contributes (post-fold).
    pub fn row_count(&self) -> u32 {
        self.rows.len() as u32
    }

    /// `true` when no rows are present (fully-folded range or
    /// freshly-allocated empty chunk).
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns the row whose `source_line` equals `target`, if it
    /// is present in this chunk (i.e. not folded).
    ///
    /// Binary search on `source_line`; O(log N) for the typical
    /// 128-row chunk. Returns `None` if `target` is folded or
    /// outside the chunk's range.
    pub fn row_at_source_line(&self, target: u32) -> Option<&CellRow> {
        match self
            .rows
            .binary_search_by_key(&target, |r| r.source_line)
        {
            Ok(idx) => self.rows.get(idx),
            Err(_) => None,
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

    #[test]
    fn empty_chunk_reports_empty() {
        let c = CellChunk::empty(10, MatrixVersion::ZERO);
        assert_eq!(c.start_source_line, 10);
        assert!(c.is_empty());
        assert_eq!(c.row_count(), 0);
        assert!(c.row_at_source_line(10).is_none());
    }

    #[test]
    fn row_at_source_line_finds_present_row() {
        let c = CellChunk::new(
            0,
            vec![row(0, b'a'), row(1, b'b'), row(2, b'c')],
            MatrixVersion::ZERO,
        );
        assert_eq!(c.row_count(), 3);
        assert_eq!(c.row_at_source_line(0).unwrap().source_line, 0);
        assert_eq!(c.row_at_source_line(1).unwrap().source_line, 1);
        assert_eq!(c.row_at_source_line(2).unwrap().source_line, 2);
    }

    /// Fold elision: source line 1 is folded so the chunk's
    /// rows array skips it. `row_at_source_line(1)` returns None.
    #[test]
    fn row_at_source_line_returns_none_for_folded() {
        let c = CellChunk::new(
            0,
            vec![row(0, b'a'), row(2, b'c'), row(4, b'e')],
            MatrixVersion::ZERO,
        );
        assert_eq!(c.row_count(), 3);
        assert!(c.row_at_source_line(0).is_some());
        assert!(c.row_at_source_line(1).is_none()); // folded
        assert!(c.row_at_source_line(2).is_some());
        assert!(c.row_at_source_line(3).is_none()); // folded
        assert!(c.row_at_source_line(4).is_some());
        assert!(c.row_at_source_line(5).is_none()); // out of range
    }

    #[test]
    fn row_at_source_line_works_on_single_row() {
        let c = CellChunk::new(7, vec![row(7, b'x')], MatrixVersion::ZERO);
        assert_eq!(c.row_count(), 1);
        assert_eq!(c.row_at_source_line(7).unwrap().source_line, 7);
        assert!(c.row_at_source_line(6).is_none());
        assert!(c.row_at_source_line(8).is_none());
    }

    #[test]
    fn version_round_trip() {
        let v = MatrixVersion {
            text: 5,
            syntax: 2,
            inlay_hints: 1,
            folds: 0,
            theme: 0,
        };
        let c = CellChunk::empty(0, v);
        assert_eq!(c.version, v);
    }
}
