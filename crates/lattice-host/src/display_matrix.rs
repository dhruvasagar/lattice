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
use lattice_syntax::Style;

/// A style-tagged run within a [`DisplayLine`]'s `text` — the per-line
/// analogue of a `Cell`, one per contiguous run instead of per char.
/// The renderer resolves `style` → foreground colour + modifiers via
/// the per-frame theme; `flags` carries the non-style bits the cell
/// model baked: [`lattice_cells::cell_flags::INLAY`] for spliced inlay
/// text, `WS_MARKER` for a whitespace-marker glyph. Run lengths
/// (`len`, utf-8 bytes) sum to `text.len()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayRun {
    pub len: u32,
    pub style: Style,
    pub flags: u16,
    /// DR.2 (2026-08-12): intra-line diff refinement — when `Some`,
    /// this run's **background** overrides its row's diff tint.
    ///
    /// The second axis of `span-layering.md`, narrowed from per-row to
    /// per-range. Runs already split wherever appearance changes, so
    /// carrying it here costs one field and no new splitting concept.
    /// Foreground is untouched, which is what keeps the syntax colour
    /// DS.1–DS.5 added visible under the refinement.
    pub refine: Option<lattice_cells::RefineKind>,
}

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
    /// Run lengths sum to `text.len()`. See [`DisplayRun`].
    pub runs: Arc<[DisplayRun]>,
    /// `(source_byte, extra_display_cols)` breakpoints: at each source
    /// byte, how many extra display columns were inserted ahead of it
    /// (inlay text + tab expansion). Drives source-byte ↔ display-col
    /// translation. Same shape as `CellRow::inlay_offsets`.
    pub col_map: Arc<[(u32, u32)]>,
    /// H.1: source-byte ranges this line hides — `[start, end)`,
    /// sorted ascending and non-overlapping (the builder coalesces
    /// before storing; two overlapping ranges would have their
    /// shared width subtracted twice and every column past them
    /// would be wrong).
    ///
    /// A hidden range occupies zero display columns and its bytes
    /// are absent from [`Self::text`], so this is what lets a
    /// source position still be located: see
    /// [`lattice_cells::source_byte_to_display_col`]. Empty for
    /// every line of a buffer whose language declares no conceal
    /// rules, which is the path that must stay free.
    ///
    /// Deliberately NOT folded into [`Self::col_map`] as a signed
    /// delta. `col_map`'s columns are already char-resolved, so a
    /// hidden range removes exactly `end - start` of them — the
    /// width is derivable from the range and a second encoding of
    /// it could only ever disagree with the first.
    pub conceals: Arc<[lattice_cells::ConcealRange]>,
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
            conceals: self.conceals.clone(),
            col_count: self.col_count,
            fold: self.fold,
        }
    }

    /// Map a source byte (already char-resolved) → combined display
    /// column for this line. Returns the column *after* any inlay /
    /// tab-expansion columns inserted at or before `byte`. The
    /// `DisplayLine` analogue of `CellRow::byte_to_combined_col`; both
    /// walk the same `(orig_byte, extra_cols)` breakpoint list
    /// (`col_map` here, `inlay_offsets` there), so overlay / cursor
    /// positioning is identical across the cell and display substrates.
    /// `col_map` is sorted ascending by `orig_byte` (build invariant),
    /// so the walk can stop at the first breakpoint past `byte`.
    ///
    /// H.1: also subtracts [`Self::conceals`], and a byte falling
    /// *inside* a hidden range resolves to that range's start column.
    /// The arithmetic lives in [`lattice_cells::source_byte_to_display_col`]
    /// rather than here because three carriers ask this question and an
    /// elision the cursor agrees with and the search highlight does not
    /// is a caret sitting off its own match.
    pub fn byte_to_combined_col(&self, byte: u32) -> u32 {
        lattice_cells::source_byte_to_display_col(byte, &self.col_map, &self.conceals)
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
        Self::new(
            start_source_line,
            Arc::from([] as [DisplayLine; 0]),
            version,
        )
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
    /// CL.1: the line this matrix was built with its conceals suppressed on.
    ///
    /// Carried on the matrix rather than folded into `MatrixVersion` on
    /// purpose. The version is the cache-hit key, and the reveal line moves
    /// with the CURSOR — folding it in would invalidate the whole matrix on
    /// every `j`, turning a 47 ns cache hit into a ~1.5 ms window rebuild.
    /// Kept beside the version instead, so the worker can see the reveal moved
    /// and rebuild exactly the two rows that changed.
    pub reveal_line: Option<u32>,
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
            reveal_line: None,
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
            reveal_line: None,
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
            reveal_line: None,
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
                vec![DisplayRun {
                    len: text.len() as u32,
                    style: Style::Default,
                    flags: 0,
                    refine: None,
                }]
                .into_boxed_slice(),
            ),
            col_map: Arc::from([] as [(u32, u32); 0]),
            conceals: Arc::from([] as [lattice_cells::ConcealRange; 0]),
            col_count,
            fold: None,
        }
    }

    /// A line carrying both tables, for the H.1 coordinate tests.
    fn line_with(
        text: &str,
        col_map: &[(u32, u32)],
        conceals: &[lattice_cells::ConcealRange],
    ) -> DisplayLine {
        let mut l = line(0, text);
        l.col_map = Arc::from(col_map.to_vec().into_boxed_slice());
        l.conceals = Arc::from(conceals.to_vec().into_boxed_slice());
        l
    }

    #[test]
    fn h1_no_conceals_is_the_pre_h1_behaviour() {
        // The regression guard for every buffer in the editor: with an
        // empty conceal list the delegating implementation must agree
        // with the inlay-only walk it replaced, entry for entry.
        let l = line_with("hello world", &[(1, 2), (3, 1)], &[]);
        assert_eq!(l.byte_to_combined_col(0), 0);
        assert_eq!(l.byte_to_combined_col(1), 3);
        assert_eq!(l.byte_to_combined_col(2), 4);
        assert_eq!(l.byte_to_combined_col(3), 6);
        assert_eq!(l.byte_to_combined_col(5), 8);
    }

    #[test]
    fn h1_a_byte_inside_a_concealed_range_clamps_to_its_start() {
        // `[[a][hi]]` in miniature: hide [0,4), show 4..6, hide [6,9).
        let l = line_with("[[a][hi]]", &[], &[(0, 4), (6, 9)]);
        assert_eq!(l.byte_to_combined_col(0), 0, "before anything visible");
        assert_eq!(l.byte_to_combined_col(2), 0, "inside the first hidden run");
        assert_eq!(l.byte_to_combined_col(4), 0, "first visible byte");
        assert_eq!(l.byte_to_combined_col(5), 1);
        assert_eq!(l.byte_to_combined_col(7), 2, "inside the second hidden run");
        assert_eq!(l.byte_to_combined_col(9), 2, "past both");
    }

    #[test]
    fn h1_with_source_line_shares_the_conceal_arc() {
        // The incremental-rebuild shift reuses payload Arcs so unedited
        // lines stay byte-identical and therefore pixel-stable. A new
        // field that got cloned instead of shared would not fail any
        // behaviour test — only this one.
        let l = line_with("x", &[], &[(0, 1)]);
        let shifted = l.with_source_line(7);
        assert_eq!(shifted.source_line, 7);
        assert!(
            Arc::ptr_eq(&l.conceals, &shifted.conceals),
            "conceals must be shared, not re-allocated"
        );
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
    fn byte_to_combined_col_shifts_by_colmap_widths_at_or_before_byte() {
        // Two breakpoints: an inlay of width 3 at byte 2, a tab
        // expansion of +3 cols at byte 5. Mirrors `CellRow`'s test.
        let mut l = line(0, "ignored");
        l.col_map = Arc::from(vec![(2u32, 3u32), (5u32, 3u32)].into_boxed_slice());
        // No breakpoint at/before byte 1 → col == byte.
        assert_eq!(l.byte_to_combined_col(1), 1);
        // Breakpoint at byte 2 (orig_byte <= byte) shifts by +3.
        assert_eq!(l.byte_to_combined_col(2), 5);
        // Both breakpoints (2 and 5) apply at byte 6 → +6.
        assert_eq!(l.byte_to_combined_col(6), 12);
        // Empty col_map → identity.
        let plain = line(0, "abc");
        assert_eq!(plain.byte_to_combined_col(3), 3);
    }

    #[test]
    fn shifted_by_advances_lines_sharing_payload() {
        let chunk = DisplayChunk::new(10, vec![line(10, "a"), line(12, "c")], MatrixVersion::ZERO);
        let s = chunk.shifted_by(3, MatrixVersion::ZERO);
        assert_eq!(s.start_source_line, 13);
        let lines: Vec<u32> = s.rows.iter().map(|r| r.source_line).collect();
        assert_eq!(lines, vec![13, 15]);
        for (o, n) in chunk.rows.iter().zip(s.rows.iter()) {
            assert!(Arc::ptr_eq(&o.text, &n.text));
        }
    }
}
