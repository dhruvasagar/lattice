//! Indentation guides — the per-pane layer both renderers paint from.
//!
//! See `docs/dev/architecture/indent-guides.md` (design) and
//! `docs/dev/operations/slice-plans/indent-guides.md` (slices).
//!
//! ## What this is
//!
//! [`IndentGuides`] is the resolved answer to "which columns carry a guide on
//! which rows, and which block is the cursor in". It is built by
//! `cells_worker` in the pass that builds the pane's
//! [`DisplayMatrix`](crate::display_matrix::DisplayMatrix), from the same
//! snapshot and stamped with the same [`MatrixVersion`] — so the two cannot
//! disagree and guides need no staleness axis of their own.
//!
//! ## Why the paint predicate is resolved here
//!
//! [`IndentBlock::paints_on`] decides whether a guide may occupy a column, and
//! getting it wrong means painting over text. Applying it in the producer
//! rather than in each renderer means there is exactly one implementation: a
//! bug surfaces as a failing test here rather than as corrupted text in one
//! peer and not the other. What each renderer still owns is the *mechanism* —
//! the TUI substitutes a glyph into a cell, the GPU peer paints a hairline
//! quad — because a terminal cell cannot hold a one-pixel rule.
//!
//! ## Why blocks are published alongside the per-row marks
//!
//! The *active* guide is the innermost block containing the cursor, and the
//! cursor moves at keystroke rate. Publishing extents rather than a
//! precomputed "is active" flag lets each renderer pick the active block
//! per frame from the cursor row it already holds — an integer scan over the
//! blocks in the window — so cursor motion costs the worker nothing and the
//! highlight has zero lag.

use std::sync::Arc;

use lattice_cells::MatrixVersion;
use lattice_core::indent::IndentUnit;
use lattice_core::indent_blocks::{IndentBlock, LineIndent, indent_blocks, is_closer_line};

/// One guide occupying one column of one row.
///
/// `block` indexes [`IndentGuides::blocks`], which is what lets a renderer
/// style the active guide differently without a second lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuideMark {
    /// Display column the guide occupies.
    pub col: u16,
    /// Index into [`IndentGuides::blocks`].
    pub block: u16,
}

/// How far above the covered window the block walk starts looking for the
/// blocks that enclose it.
///
/// A block cannot span a non-blank line at column 0, so starting the walk at
/// the nearest such line above the window is *exact*, not approximate — it
/// finds every block that reaches into the window. The cap bounds the scan on
/// a file that is indented for thousands of consecutive lines (minified JSON,
/// generated code); past it the outermost guides may be missing from a
/// scrolled-into window, which is a missing hairline rather than a wrong one.
const MAX_LOOKBACK: u32 = 2000;

/// The per-buffer inputs a guide layer is built from, resolved once per
/// publish and carried on [`crate::render_state::PaneCellsInputs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndentGuideInputs {
    /// The buffer's resolved indent level. `step()` is the guide spacing;
    /// `tab_width()` is what makes a tab-indented line's depth comparable
    /// to a space-indented one's.
    pub unit: IndentUnit,
    /// `display.indent-guides`.
    pub enabled: bool,
    /// The `MatrixVersion::indent` stamp these inputs produce.
    pub version: u64,
}

/// Hash the inputs that change the guides' geometry.
///
/// Only `shiftwidth` and `enabled`: `expandtab` never affects an existing
/// line's rendered indentation, and `tabstop` already bumps the `whitespace`
/// axis, which invalidates the same matrix. Carrying an input on two axes is
/// carrying an input that can drift.
pub fn indent_axis_version(unit: &IndentUnit, enabled: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    unit.step().hash(&mut h);
    enabled.hash(&mut h);
    h.finish()
}

/// The per-pane guide layer.
#[derive(Clone, Debug)]
pub struct IndentGuides {
    /// Every block reaching into the covered window, ordered by opener then
    /// column. Retained alongside [`Self::rows`] because the active-block pick
    /// needs extents, not just painted columns.
    pub blocks: Arc<[IndentBlock]>,
    /// Painted marks per covered source line: `rows[line - covered_start]`.
    /// Already filtered through [`IndentBlock::paints_on`], so every mark
    /// lands on a blank cell.
    pub rows: Arc<[Arc<[GuideMark]>]>,
    /// Source line `rows[0]` describes.
    pub covered_start: u32,
    /// The stamp of the display matrix built alongside this layer.
    pub version: MatrixVersion,
}

impl Default for IndentGuides {
    fn default() -> Self {
        Self::empty()
    }
}

impl IndentGuides {
    pub fn empty() -> Self {
        Self {
            blocks: Arc::from([] as [IndentBlock; 0]),
            rows: Arc::from([] as [Arc<[GuideMark]>; 0]),
            covered_start: 0,
            version: MatrixVersion::ZERO,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Marks painted on `source_line`, or `&[]` when the line is outside the
    /// covered window. Outside-the-window is the normal answer during a
    /// scroll that has outrun the worker, not an error.
    pub fn marks_for_line(&self, source_line: u32) -> &[GuideMark] {
        let Some(idx) = source_line.checked_sub(self.covered_start) else {
            return &[];
        };
        self.rows.get(idx as usize).map(|r| &r[..]).unwrap_or(&[])
    }

    /// Index of the innermost block containing `cursor_line`, or `None` when
    /// the cursor is not inside any block (top-level code).
    ///
    /// Innermost is the greatest column: nesting deeper always means a guide
    /// further right, whatever the indent widths involved.
    pub fn active_block(&self, cursor_line: u32) -> Option<u16> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.contains(cursor_line))
            .max_by_key(|(_, b)| b.col)
            .map(|(i, _)| i as u16)
    }
}

/// Build the guide layer for the source lines `[lo, hi)`.
///
/// `lines(i)` yields source line `i`; the caller supplies it so this stays
/// testable without a rope and so the worker can reuse whatever line access it
/// already holds. `hi` is clamped by the caller to the buffer's content line
/// count.
///
/// The walk starts above `lo` (see [`MAX_LOOKBACK`]) so a block opened off the
/// top of the window still paints inside it. It does *not* extend below `hi`:
/// a block still open at the end of the walk is closed at the last content
/// line, which is at or below the last visible row, so painting and the
/// cursor-membership test are both unaffected.
pub fn build_indent_guides<F>(
    mut lines: F,
    line_count: u32,
    unit: &IndentUnit,
    lo: u32,
    hi: u32,
    version: MatrixVersion,
) -> IndentGuides
where
    F: FnMut(u32) -> Option<String>,
{
    let hi = hi.min(line_count);
    let lo = lo.min(hi);
    if lo == hi {
        return IndentGuides::empty();
    }

    let walk_start = scan_back_to_top_level(&mut lines, lo);
    let indents: Vec<LineIndent> = (walk_start..hi)
        .map(|i| {
            let line = lines(i).unwrap_or_default();
            LineIndent {
                depth: if IndentUnit::is_blank(&line) {
                    None
                } else {
                    Some(unit.columns_of(&line))
                },
                closer: is_closer_line(&line),
            }
        })
        .collect();

    let blocks: Vec<IndentBlock> = indent_blocks(&indents, unit.step())
        .into_iter()
        .map(|b| IndentBlock {
            col: b.col,
            start_line: b.start_line + walk_start,
            end_line: b.end_line + walk_start,
        })
        .collect();

    let rows: Vec<Arc<[GuideMark]>> = (lo..hi)
        .map(|line| {
            let depth = indents[(line - walk_start) as usize].depth;
            let marks: Vec<GuideMark> = blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| b.paints_on(line, depth))
                .map(|(i, b)| GuideMark {
                    col: b.col,
                    block: i as u16,
                })
                .collect();
            Arc::from(marks.into_boxed_slice())
        })
        .collect();

    IndentGuides {
        blocks: Arc::from(blocks.into_boxed_slice()),
        rows: Arc::from(rows.into_boxed_slice()),
        covered_start: lo,
        version,
    }
}

/// Nearest non-blank line at column 0 at or above `lo`, bounded by
/// [`MAX_LOOKBACK`]. No block can span such a line, so the walk starting there
/// sees every block that reaches into the window.
fn scan_back_to_top_level<F>(lines: &mut F, lo: u32) -> u32
where
    F: FnMut(u32) -> Option<String>,
{
    let floor = lo.saturating_sub(MAX_LOOKBACK);
    let mut i = lo;
    while i > floor {
        i -= 1;
        let line = lines(i).unwrap_or_default();
        if !IndentUnit::is_blank(&line) && !line.starts_with([' ', '\t']) {
            return i;
        }
    }
    floor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> IndentUnit {
        IndentUnit::new(4, true, 4)
    }

    fn build(src: &str) -> IndentGuides {
        let lines: Vec<String> = src.split('\n').map(|s| s.to_string()).collect();
        let n = lines.len() as u32;
        build_indent_guides(
            |i| lines.get(i as usize).cloned(),
            n,
            &unit(),
            0,
            n,
            MatrixVersion::ZERO,
        )
    }

    fn cols(g: &IndentGuides, line: u32) -> Vec<u16> {
        g.marks_for_line(line).iter().map(|m| m.col).collect()
    }

    const NESTED: &str = "fn f() {\n\n    if c {\n        work();\n\n    }\n}\n\nfn g() {";

    #[test]
    fn marks_match_the_paint_predicate() {
        let g = build(NESTED);
        assert_eq!(cols(&g, 0), Vec::<u16>::new(), "opener column holds text");
        assert_eq!(cols(&g, 1), vec![0], "blank inside the outer block");
        assert_eq!(cols(&g, 2), vec![0]);
        assert_eq!(cols(&g, 3), vec![0, 4]);
        assert_eq!(cols(&g, 4), vec![0, 4], "blank inside both blocks");
        assert_eq!(cols(&g, 5), vec![0], "inner closer column holds a brace");
        assert_eq!(cols(&g, 6), Vec::<u16>::new());
        assert_eq!(cols(&g, 7), Vec::<u16>::new(), "between blocks");
    }

    #[test]
    fn active_block_is_the_innermost_containing_the_cursor() {
        let g = build(NESTED);
        let col_of = |line: u32| g.active_block(line).map(|i| g.blocks[i as usize].col);
        assert_eq!(col_of(3), Some(4), "inside the inner block");
        assert_eq!(col_of(2), Some(4), "on the inner block's opener");
        assert_eq!(col_of(5), Some(4), "on the inner block's closer");
        assert_eq!(col_of(1), Some(0), "outer block only");
        assert_eq!(col_of(6), Some(0), "on the outer closer");
        assert_eq!(col_of(7), None, "between blocks");
        assert_eq!(col_of(8), None, "top level");
    }

    #[test]
    fn every_mark_lands_on_a_blank_column() {
        // The invariant both renderers rely on: no mark may occupy a column
        // that holds a character.
        let src = "fn f() {\n    if c {\n        work();\n    }\n}\n\tmixed\n";
        let lines: Vec<&str> = src.split('\n').collect();
        let g = build(src);
        for (i, line) in lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            for mark in g.marks_for_line(i as u32) {
                let at = chars.get(mark.col as usize).copied();
                assert!(
                    matches!(at, None | Some(' ') | Some('\t')),
                    "line {i} col {} holds {at:?}",
                    mark.col
                );
            }
        }
    }

    #[test]
    fn windowed_build_indexes_from_covered_start() {
        let src = "fn f() {\n    a\n    b\n    c\n    d\n}";
        let lines: Vec<String> = src.split('\n').map(|s| s.to_string()).collect();
        let n = lines.len() as u32;
        let g = build_indent_guides(
            |i| lines.get(i as usize).cloned(),
            n,
            &unit(),
            2,
            5,
            MatrixVersion::ZERO,
        );
        assert_eq!(g.covered_start, 2);
        assert_eq!(g.rows.len(), 3);
        // The block opened at line 0 — above the window — still paints inside
        // it, which is the whole point of the look-back.
        assert_eq!(cols(&g, 2), vec![0]);
        assert_eq!(cols(&g, 4), vec![0]);
        assert_eq!(cols(&g, 1), Vec::<u16>::new(), "below covered_start");
        assert_eq!(cols(&g, 9), Vec::<u16>::new(), "past the window");
    }

    #[test]
    fn look_back_stops_at_the_nearest_top_level_line() {
        // Two sibling blocks; a window inside the second must not inherit the
        // first one's extent.
        let src = "fn f() {\n    a\n}\nfn g() {\n    b\n}";
        let lines: Vec<String> = src.split('\n').map(|s| s.to_string()).collect();
        let g = build_indent_guides(
            |i| lines.get(i as usize).cloned(),
            6,
            &unit(),
            4,
            6,
            MatrixVersion::ZERO,
        );
        assert_eq!(cols(&g, 4), vec![0]);
        assert_eq!(g.blocks.len(), 1, "only the enclosing block is walked");
        assert_eq!(g.blocks[0].start_line, 3);
    }

    #[test]
    fn empty_window_yields_the_empty_layer() {
        let g = build_indent_guides(|_| None, 0, &unit(), 0, 0, MatrixVersion::ZERO);
        assert!(g.is_empty());
        assert_eq!(g.marks_for_line(0), &[]);
        assert_eq!(g.active_block(0), None);
    }

    #[test]
    fn tab_indented_file_matches_its_space_indented_twin() {
        let tabbed = build("fn f() {\n\tif c {\n\t\twork();\n\t}\n}");
        let spaced = build("fn f() {\n    if c {\n        work();\n    }\n}");
        for line in 0..5 {
            assert_eq!(cols(&tabbed, line), cols(&spaced, line), "line {line}");
        }
    }

    #[test]
    fn version_is_carried_through_unchanged() {
        let v = MatrixVersion {
            text: 7,
            indent: 3,
            ..MatrixVersion::ZERO
        };
        let lines = ["a:", "    b"];
        let g = build_indent_guides(
            |i| lines.get(i as usize).map(|s| s.to_string()),
            2,
            &unit(),
            0,
            2,
            v,
        );
        assert_eq!(g.version, v);
    }
}
