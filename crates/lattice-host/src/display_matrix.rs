//! Per-line display cache — the substrate that retires the
//! per-character [`lattice_cells::CellMatrix`].
//!
//! See `docs/dev/architecture/display-line.md` (design) and
//! `docs/dev/operations/slice-plans/display-line.md` (slices).
//!
//! ## What this is
//!
//! A [`DisplayLine`] is the renderer-agnostic, fully-resolved display
//! form of one source line: the final display `text` (inlay hints
//! spliced in, tabs expanded to display width, whitespace markers
//! substituted), the style `runs` over it ([`RowRun`], style *tags*
//! resolved to colour by each renderer at paint), a `col_map` from
//! source bytes to inserted display columns (cursor / selection /
//! overlay coordinate translation), the display `col_count` (for
//! soft-wrap segment geometry), and an optional [`FoldHead`] when the
//! line heads a closed fold.
//!
//! [`DisplayMatrix`] is the chunked, viewport-windowed,
//! incrementally-rebuilt cache of `DisplayLine`s — the exact machinery
//! of `CellMatrix` (chunking, windowing, `MatrixVersion`, row reuse via
//! `Arc`) with the payload swapped from `Vec<Cell>` to `DisplayLine`.
//! Both renderers consume it directly: TUI maps `text` + `runs` to
//! ratatui cells; GPU shapes `text` once (`shape_line`, LineLayoutCache)
//! with per-run colours — no per-char intermediate, no un-bake.
//!
//! ## B1 scope
//!
//! Types + machinery only (`empty` / `whole_doc` / `chunked`,
//! `row_at_source_line`, coverage, `segment_count`, `shifted_by`).
//! The worker build path, the shared incremental-reuse, the
//! always-current synchronous rebuild, and the renderer cutovers land
//! in B2–B4. Not consumed by any renderer yet.

use std::sync::Arc;

use lattice_cells::{CHUNK_SIZE_WHOLE_DOC, MatrixVersion, wrap_segments};

use crate::render_state::RowRun;

/// Closed-fold head marker carried by the first visible line of a
/// folded region. `folded_lines` is how many source lines the fold
/// collapses (for the ` ┄ N lines folded` gutter / inline suffix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldHead {
    pub folded_lines: u32,
}

/// The fully-resolved display form of one source line. Fields are
/// `Arc`-shared so [`Self::with_source_line`] (the incremental-reuse
/// shift) is a refcount bump, not a copy — mirroring `CellRow`.
#[derive(Clone, Debug)]
pub struct DisplayLine {
    /// Logical (pre-fold) source line this row renders.
    pub source_line: u32,
    /// Final display string: inlays spliced, tabs expanded to display
    /// width, whitespace markers substituted.
    pub text: Arc<str>,
    /// Style-tagged byte runs partitioning `text` left-to-right.
    /// `RowRun::Source` carries the tree-sitter style tag (resolved to
    /// colour by the renderer); `RowRun::Inlay` marks spliced virtual
    /// text. Run lengths sum to `text.len()`.
    pub runs: Arc<[RowRun]>,
    /// `(source_byte, extra_display_cols)` breakpoints: at each source
    /// byte, how many extra display columns were inserted ahead of it
    /// (inlay text + tab expansion). Drives source-byte ↔ display-col
    /// translation. Same shape as `CellRow::inlay_offsets`.
    pub col_map: Arc<[(u32, u32)]>,
    /// Display width in columns (char count of `text`). Soft-wrap
    /// geometry reads this via [`DisplayMatrix::segment_count`].
    pub col_count: u32,
    /// `Some` when this line heads a closed fold.
    pub fold: Option<FoldHead>,
}

impl DisplayLine {
    /// Clone with a new `source_line`; all payload `Arc`s are shared
    /// (refcount bump only). Used by the incremental-rebuild shift for
    /// lines past an edit whose content is unchanged.
    pub fn with_source_line(&self, source_line: u32) -> Self {
        Self {
            source_line,
            text: self.text.clone(),
            runs: self.runs.clone(),
            col_map: self.col_map.clone(),
            col_count: self.col_count,
            fold: self.fold,
        }
    }
}

/// Contiguous range of display rows covering a slice of the buffer.
/// Same invariants as `CellChunk`: `rows` sorted ascending by
/// `source_line`, folded lines absent (row count is post-fold),
/// `start_source_line` is the first source line the chunk's
/// `[start, start + chunk_size)` logical range *could* contain.
#[derive(Clone, Debug)]
pub struct DisplayChunk {
    pub start_source_line: u32,
    pub rows: Arc<[DisplayLine]>,
    pub version: MatrixVersion,
}

impl DisplayChunk {
    pub fn new(
        start_source_line: u32,
        rows: impl Into<Arc<[DisplayLine]>>,
        version: MatrixVersion,
    ) -> Self {
        Self {
            start_source_line,
            rows: rows.into(),
            version,
        }
    }

    pub fn empty(start_source_line: u32, version: MatrixVersion) -> Self {
        Self::new(start_source_line, Arc::from([] as [DisplayLine; 0]), version)
    }

    pub fn row_count(&self) -> u32 {
        self.rows.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row whose `source_line == target`, or `None` if folded /
    /// outside the chunk. Binary search (rows are sorted).
    pub fn row_at_source_line(&self, target: u32) -> Option<&DisplayLine> {
        match self.rows.binary_search_by_key(&target, |r| r.source_line) {
            Ok(idx) => self.rows.get(idx),
            Err(_) => None,
        }
    }

    /// Clone-with-shifted-line: `start_source_line` and every row's
    /// `source_line` shift by `line_delta` (saturating at 0); payload
    /// `Arc`s shared. `new_version` stamps the result.
    pub fn shifted_by(&self, line_delta: i32, new_version: MatrixVersion) -> Self {
        let shifted_rows: Vec<DisplayLine> = self
            .rows
            .iter()
            .map(|r| {
                let new_line = (r.source_line as i64 + line_delta as i64).max(0) as u32;
                r.with_source_line(new_line)
            })
            .collect();
        let new_start = (self.start_source_line as i64 + line_delta as i64).max(0) as u32;
        Self {
            start_source_line: new_start,
            rows: Arc::from(shifted_rows.into_boxed_slice()),
            version: new_version,
        }
    }
}

/// Chunked, viewport-windowed cache of [`DisplayLine`]s. Mirrors
/// `CellMatrix` exactly; only the row payload differs.
#[derive(Clone, Debug)]
pub struct DisplayMatrix {
    /// Chunks ordered by `start_source_line`. Whole-doc mode has one.
    pub chunks: Arc<[Arc<DisplayChunk>]>,
    /// Logical lines per chunk, or [`CHUNK_SIZE_WHOLE_DOC`] for
    /// whole-doc mode.
    pub chunk_size: u32,
    /// Total logical lines in the source buffer (pre-fold).
    pub source_line_count: u32,
    /// Total display rows across all chunks (post-fold).
    pub visible_line_count: u32,
    /// Component-wise version captured at build time.
    pub version: MatrixVersion,
    /// Soft-wrap column width, or `0` when wrapping is off (one display
    /// row per source line). Stamped by the worker from the pane width.
    pub wrap_width: u32,
}

impl Default for DisplayMatrix {
    fn default() -> Self {
        Self::empty()
    }
}

impl DisplayMatrix {
    pub fn empty() -> Self {
        Self {
            chunks: Arc::from([] as [Arc<DisplayChunk>; 0]),
            chunk_size: CHUNK_SIZE_WHOLE_DOC,
            source_line_count: 0,
            visible_line_count: 0,
            version: MatrixVersion::ZERO,
            wrap_width: 0,
        }
    }

    pub fn chunked(
        chunks: impl Into<Arc<[Arc<DisplayChunk>]>>,
        chunk_size: u32,
        source_line_count: u32,
        version: MatrixVersion,
    ) -> Self {
        assert!(chunk_size > 0, "chunked mode requires chunk_size > 0");
        let chunks: Arc<[Arc<DisplayChunk>]> = chunks.into();
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

    pub fn whole_doc(chunk: Arc<DisplayChunk>, source_line_count: u32) -> Self {
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

    pub fn is_whole_doc(&self) -> bool {
        self.chunk_size == CHUNK_SIZE_WHOLE_DOC
    }

    pub fn is_empty(&self) -> bool {
        self.visible_line_count == 0
    }

    /// How many display rows source line `target` occupies under
    /// soft-wrap (`1` when wrapping off / line missing / folded).
    pub fn segment_count(&self, target: u32) -> u32 {
        if self.wrap_width == 0 {
            return 1;
        }
        match self.row_at_source_line(target) {
            Some(row) => wrap_segments(row.col_count, self.wrap_width),
            None => 1,
        }
    }

    /// First source line the chunks were built to cover (`0` for
    /// whole-doc / full-coverage chunked; the window lower bound when
    /// windowed). H.3 coverage semantics, ported.
    pub fn covered_start_line(&self) -> u32 {
        self.chunks
            .first()
            .map(|c| c.start_source_line)
            .unwrap_or(0)
    }

    /// Exclusive upper bound of the covered source-line range.
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

    /// Does the matrix cover all of `[lo, hi)`? Empty matrix covers
    /// nothing. Drives the worker cache-hit / window-extend gate.
    pub fn covers(&self, lo: u32, hi: u32) -> bool {
        if self.chunks.is_empty() {
            return false;
        }
        self.covered_start_line() <= lo && hi <= self.covered_end_line()
    }

    /// Row whose `source_line == target`, walking chunks in order.
    /// `None` when folded or outside coverage (the renderer falls back
    /// to its rope/plain path only transiently, off-window).
    pub fn row_at_source_line(&self, target: u32) -> Option<&DisplayLine> {
        for chunk in self.chunks.iter() {
            let start = chunk.start_source_line;
            let end = if self.chunk_size == CHUNK_SIZE_WHOLE_DOC {
                self.source_line_count
            } else {
                start.saturating_add(self.chunk_size)
            };
            if target < start {
                return None;
            }
            if target < end {
                return chunk.row_at_source_line(target);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(source_line: u32, text: &str) -> DisplayLine {
        let col_count = text.chars().count() as u32;
        DisplayLine {
            source_line,
            text: Arc::from(text),
            runs: Arc::from(
                vec![RowRun::Source {
                    len: text.len() as u32,
                    style: lattice_syntax::Style::Default,
                }]
                .into_boxed_slice(),
            ),
            col_map: Arc::from([] as [(u32, u32); 0]),
            col_count,
            fold: None,
        }
    }

    #[test]
    fn whole_doc_basic_lookup() {
        let chunk = Arc::new(DisplayChunk::new(
            0,
            vec![line(0, "a"), line(1, "bb"), line(2, "ccc")],
            MatrixVersion::ZERO,
        ));
        let m = DisplayMatrix::whole_doc(chunk, 3);
        assert!(m.is_whole_doc());
        assert_eq!(m.visible_line_count, 3);
        assert_eq!(m.row_at_source_line(1).unwrap().text.as_ref(), "bb");
        assert!(m.row_at_source_line(3).is_none());
        assert!(m.covers(0, 3));
        assert_eq!(m.covered_start_line(), 0);
        assert_eq!(m.covered_end_line(), 3);
    }

    #[test]
    fn chunked_lookup_and_coverage() {
        // One chunk of size 16 over a 25-line doc, windowed to [16,25).
        let c1 = Arc::new(DisplayChunk::new(
            16,
            (16u32..25).map(|i| line(i, "x")).collect::<Vec<_>>(),
            MatrixVersion::ZERO,
        ));
        let m = DisplayMatrix::chunked(vec![c1], 16, 25, MatrixVersion::ZERO);
        assert!(!m.is_whole_doc());
        assert_eq!(m.visible_line_count, 9);
        assert!(m.row_at_source_line(20).is_some());
        assert!(m.row_at_source_line(5).is_none(), "off-window below");
        assert_eq!(m.covered_start_line(), 16);
        assert_eq!(m.covered_end_line(), 25, "16+16 clamped to line count");
        assert!(m.covers(18, 22));
        assert!(!m.covers(5, 22), "window does not cover line 5");
    }

    #[test]
    fn empty_covers_nothing() {
        let m = DisplayMatrix::empty();
        assert!(m.is_empty());
        assert!(!m.covers(0, 1));
        assert!(m.row_at_source_line(0).is_none());
    }

    #[test]
    fn segment_count_wraps_on_width() {
        let chunk = Arc::new(DisplayChunk::new(
            0,
            vec![line(0, "0123456789")], // 10 cols
            MatrixVersion::ZERO,
        ));
        let mut m = DisplayMatrix::whole_doc(chunk, 1);
        assert_eq!(m.segment_count(0), 1, "wrap off");
        m.wrap_width = 4;
        assert_eq!(m.segment_count(0), 3, "ceil(10/4) = 3");
        assert_eq!(m.segment_count(99), 1, "missing line → 1");
    }

    #[test]
    fn with_source_line_shares_payload() {
        let l = line(5, "hello");
        let shifted = l.with_source_line(8);
        assert_eq!(shifted.source_line, 8);
        assert_eq!(shifted.text.as_ref(), "hello");
        assert!(Arc::ptr_eq(&l.text, &shifted.text));
        assert!(Arc::ptr_eq(&l.runs, &shifted.runs));
    }

    #[test]
    fn shifted_by_advances_lines_sharing_payload() {
        let chunk = DisplayChunk::new(
            10,
            vec![line(10, "a"), line(12, "c")],
            MatrixVersion::ZERO,
        );
        let s = chunk.shifted_by(3, MatrixVersion::ZERO);
        assert_eq!(s.start_source_line, 13);
        let lines: Vec<u32> = s.rows.iter().map(|r| r.source_line).collect();
        assert_eq!(lines, vec![13, 15]);
        for (o, n) in chunk.rows.iter().zip(s.rows.iter()) {
            assert!(Arc::ptr_eq(&o.text, &n.text));
        }
    }
}
