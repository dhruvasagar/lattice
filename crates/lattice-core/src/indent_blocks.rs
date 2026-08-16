//! Indent blocks — the shared answer to "what block is this line in".
//!
//! [`IndentUnit`](crate::indent::IndentUnit) answers *how wide is one level*.
//! This module answers *where do the levels begin and end*, and it has two
//! consumers that must not disagree:
//!
//! - **Indentation guides** — a guide is a block's column drawn down the
//!   block's extent (`docs/dev/architecture/indent-guides.md`).
//! - **`foldmethod=indent`** — a fold is a block's extent
//!   (`lattice-host/src/folds.rs`).
//!
//! They are two views of one question, and a user who folds a block and sees a
//! different extent than the one that was just highlighted has been told two
//! different things by the same editor. So the walk lives here, once, and both
//! call it.
//!
//! Nothing here reads config or touches a rope: [`line_indents`] projects lines
//! to [`LineIndent`], and [`indent_blocks`] is a pure function of that
//! projection. That is what lets the subtle cases — blank runs, closer
//! inclusion, multi-level jumps, tabs — be argued against a unit test rather
//! than against a screenshot.

use crate::indent::IndentUnit;

/// One line's contribution to the block walk.
///
/// The two fields travel together because the walk needs both at the same
/// moment: `depth` decides whether a block closes here, and `closer` decides
/// whether the closing line is swallowed into the block it closes. Passing two
/// parallel slices instead would let a caller hand over mismatched lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineIndent {
    /// Leading whitespace in display columns, or `None` when the line is
    /// blank. Blank lines are *transparent* to the walk — they neither break a
    /// block nor extend one past its last content line.
    pub depth: Option<u16>,
    /// The line is nothing but closing brackets. See [`is_closer_line`].
    pub closer: bool,
}

/// One indent guide: a display column, and the inclusive line range it spans.
///
/// `start_line` is the block's **opener** and `end_line` its **closer**, both
/// included. Neither is necessarily painted — [`IndentBlock::paints_on`] tests
/// that — but both belong to the range because the *active* block under a
/// cursor sitting on `if c {` is the block that line opens, and the block under
/// a cursor on the matching `}` is the one it closes. One range serves the
/// extent question and the membership question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndentBlock {
    /// Display column the guide occupies.
    pub col: u16,
    /// Opener line, inclusive.
    pub start_line: u32,
    /// Closer line, inclusive.
    pub end_line: u32,
}

impl IndentBlock {
    /// Whether this block's guide is drawn on `line`.
    ///
    /// The single predicate the whole feature rests on:
    ///
    /// > in range, **and** (the line is blank **or** the guide's column is
    /// > strictly left of where the line's text starts).
    ///
    /// The second arm is what keeps a guide out of a cell that holds text — on
    /// `fn f() {` the column-0 guide fails `0 < 0`, and on the matching `}` it
    /// fails for the same reason. The first arm is what carries a guide through
    /// a blank line inside a block.
    ///
    /// Because the producer applies this before publishing, **a published guide
    /// mark always lands on a blank cell**, and no renderer needs a
    /// don't-overwrite-text guard.
    #[inline]
    pub fn paints_on(&self, line: u32, line_depth: Option<u16>) -> bool {
        if line < self.start_line || line > self.end_line {
            return false;
        }
        match line_depth {
            None => true,
            Some(depth) => self.col < depth,
        }
    }

    /// Whether `line` is inside this block at all, painted or not. The
    /// membership test the active-block pick uses.
    #[inline]
    pub fn contains(&self, line: u32) -> bool {
        self.start_line <= line && line <= self.end_line
    }
}

/// Upper bound on emitted blocks.
///
/// Mirrors `folds::MAX_FOLDS`. The walk itself is linear, so this is not a
/// complexity guard — it bounds *memory* on a pathological file (one whose
/// indentation increases on every line produces one block per line).
const MAX_BLOCKS: usize = 5000;

/// Upper bound on grid columns emitted for a single indent jump.
///
/// An opener at column 0 followed by a body at column 60 000 would otherwise
/// emit thousands of guides from one line pair. Sixty-four levels is deeper
/// than any code a guide would help with; past that the guides are the problem.
const MAX_LEVELS_PER_BLOCK: u16 = 64;

/// Project lines to the walk's input.
///
/// Depth is measured in **display columns** (`IndentUnit::columns_of`, so a tab
/// advances to the next `tabstop`), not in leading whitespace characters. A
/// tab-indented file and its space-indented twin must produce identical blocks;
/// counting characters is what made them differ.
pub fn line_indents<'a>(
    lines: impl Iterator<Item = &'a str>,
    unit: &IndentUnit,
) -> Vec<LineIndent> {
    lines
        .map(|line| LineIndent {
            depth: if IndentUnit::is_blank(line) {
                None
            } else {
                Some(unit.columns_of(line))
            },
            closer: is_closer_line(line),
        })
        .collect()
}

/// Whether `line` is a pure closing-bracket line.
///
/// Most brace languages dedent the closing delimiter back to the parent's
/// indent (`}`, `};`, `})`, `})?;`), which leaves the closer *outside* the
/// block its own body belongs to. Swallowing it keeps a fold's summary line
/// ending on the brace instead of orphaning it, and keeps the cursor "inside"
/// the block while it sits on the closer.
///
/// The heuristic is deliberately narrow: bracket characters plus the
/// punctuation that trails them. `} else {` is not a closer — it opens a new
/// block, and swallowing it would merge two sibling blocks into one.
pub fn is_closer_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed
        .chars()
        .all(|c| matches!(c, ')' | ']' | '}' | ',' | ';' | '?'))
}

/// Walk `lines` and emit every indent block, ordered by opener then column.
///
/// A block opens at line `p` when the next non-blank line is strictly deeper
/// than `p`, and closes at the first non-blank line no deeper than `p` — with
/// that line swallowed when it is a [closer](is_closer_line) at exactly `p`'s
/// depth. Blank lines are transparent throughout.
///
/// An indent jump of more than one level emits one block per intervening grid
/// column over the same range: there is no structure between those levels to
/// give them different extents. Columns are laid on a grid anchored at the
/// **opener's** column rather than at 0, so continuation-line indentation
/// (an opener at column 7) still produces guides that line up with the code.
///
/// Linear in `lines.len()`: a stack of open blocks, popped when a line closes
/// them. The direct transcription of the "walk forward to find the end"
/// formulation is quadratic on deeply nested files, and this runs on every
/// publish.
pub fn indent_blocks(lines: &[LineIndent], step: u16) -> Vec<IndentBlock> {
    let step = step.max(1);
    let mut blocks: Vec<IndentBlock> = Vec::new();
    // (opener line, opener depth, body depth) for each block still open.
    let mut open: Vec<(usize, u16, u16)> = Vec::new();
    // Last non-blank line seen, and its depth. A block that closes at line `k`
    // ends at this line, because every line between it and `k` was blank or
    // deeper.
    let mut prev: Option<(usize, u16)> = None;

    for (k, line) in lines.iter().enumerate() {
        let Some(depth) = line.depth else {
            continue; // blank lines are transparent
        };

        while let Some(&(opener, opener_depth, body_depth)) = open.last() {
            if depth > opener_depth {
                break;
            }
            open.pop();
            // Unwrap-free: a block can only be open if a non-blank line opened
            // it, so `prev` is populated. `k - 1` would be wrong — the line
            // before `k` may be blank, and a block does not extend into the
            // trailing blank run that follows its last content line.
            let mut end = prev.map(|(line, _)| line).unwrap_or(opener);
            if depth == opener_depth && line.closer {
                end = k;
            }
            push_block_columns(&mut blocks, opener, end, opener_depth, body_depth, step);
            if blocks.len() >= MAX_BLOCKS {
                return blocks;
            }
        }

        if let Some((prev_line, prev_depth)) = prev
            && depth > prev_depth
        {
            open.push((prev_line, prev_depth, depth));
        }
        prev = Some((k, depth));
    }

    // End of input closes whatever is still open, at the last content line.
    let end = prev.map(|(line, _)| line).unwrap_or(0);
    while let Some((opener, opener_depth, body_depth)) = open.pop() {
        push_block_columns(&mut blocks, opener, end, opener_depth, body_depth, step);
        if blocks.len() >= MAX_BLOCKS {
            break;
        }
    }

    blocks.sort_by_key(|b| (b.start_line, b.col));
    blocks
}

/// Emit one block per grid column in `opener_depth .. body_depth`.
fn push_block_columns(
    out: &mut Vec<IndentBlock>,
    opener: usize,
    end: usize,
    opener_depth: u16,
    body_depth: u16,
    step: u16,
) {
    let mut col = opener_depth;
    let mut levels = 0u16;
    while col < body_depth && levels < MAX_LEVELS_PER_BLOCK {
        out.push(IndentBlock {
            col,
            start_line: opener as u32,
            end_line: end as u32,
        });
        col = col.saturating_add(step);
        levels += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(width: u8, tabstop: u8) -> IndentUnit {
        IndentUnit::new(width, true, tabstop)
    }

    /// Build blocks from source text laid out as it would appear on screen.
    fn blocks_of(src: &str, width: u8, tabstop: u8) -> Vec<IndentBlock> {
        let unit = unit(width, tabstop);
        let lines = line_indents(src.split('\n'), &unit);
        indent_blocks(&lines, unit.step())
    }

    /// The columns actually painted on each line — the feature's observable
    /// output, and what every behavioural test below asserts against.
    fn painted(src: &str, width: u8, tabstop: u8) -> Vec<Vec<u16>> {
        let unit = unit(width, tabstop);
        let lines = line_indents(src.split('\n'), &unit);
        let blocks = indent_blocks(&lines, unit.step());
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let mut cols: Vec<u16> = blocks
                    .iter()
                    .filter(|b| b.paints_on(i as u32, line.depth))
                    .map(|b| b.col)
                    .collect();
                cols.sort_unstable();
                cols.dedup();
                cols
            })
            .collect()
    }

    #[test]
    fn flat_file_has_no_blocks() {
        assert!(blocks_of("a\nb\nc", 4, 4).is_empty());
    }

    #[test]
    fn empty_and_blank_inputs_are_empty() {
        assert!(blocks_of("", 4, 4).is_empty());
        assert!(blocks_of("\n\n\n", 4, 4).is_empty());
        assert!(indent_blocks(&[], 4).is_empty());
    }

    #[test]
    fn single_block_spans_opener_through_closer() {
        let b = blocks_of("fn f() {\n    body\n}", 4, 4);
        assert_eq!(
            b,
            vec![IndentBlock {
                col: 0,
                start_line: 0,
                end_line: 2
            }],
            "closer at the opener's depth is swallowed"
        );
    }

    #[test]
    fn guide_never_lands_on_a_column_holding_text() {
        // The design fragment's worked picture, asserted column by column.
        let src = "fn f() {\n\n    if c {\n        work();\n\n    }\n}\n\nfn g() {";
        assert_eq!(
            painted(src, 4, 4),
            vec![
                vec![],     // fn f() {      — column 0 holds `f`
                vec![0],    // (blank)       — inside the outer block
                vec![0],    //     if c {    — column 4 holds `i`
                vec![0, 4], //         work();
                vec![0, 4], // (blank)       — inside both blocks
                vec![0],    //     }         — column 4 holds `}`
                vec![],     // }             — column 0 holds `}`
                vec![],     // (blank)       — between blocks
                vec![],     // fn g() {
            ]
        );
    }

    #[test]
    fn blank_between_blocks_carries_nothing() {
        let src = "fn f() {\n    a\n}\n\nfn g() {\n    b\n}";
        let p = painted(src, 4, 4);
        assert_eq!(p[3], Vec::<u16>::new(), "blank between two blocks");
        assert_eq!(p[1], vec![0]);
        assert_eq!(p[5], vec![0]);
    }

    #[test]
    fn trailing_blank_run_does_not_extend_a_block() {
        // No closer: the block ends at its last content line, and the blank
        // run after it belongs to nobody.
        let src = "if x:\n    body\n\n\ny = 1";
        let p = painted(src, 4, 4);
        assert_eq!(p[1], vec![0]);
        assert_eq!(p[2], Vec::<u16>::new());
        assert_eq!(p[3], Vec::<u16>::new());
    }

    #[test]
    fn interior_blanks_keep_the_guide_without_a_closer() {
        // Python: the block ends at its last deeper line, but blanks *inside*
        // it stay covered.
        let src = "def f():\n    a\n\n    b\n\ndef g():";
        let p = painted(src, 4, 4);
        assert_eq!(p[2], vec![0], "blank between two body lines");
        assert_eq!(p[4], Vec::<u16>::new(), "blank after the last body line");
    }

    #[test]
    fn closer_inclusion_requires_a_pure_bracket_line() {
        assert!(is_closer_line("}"));
        assert!(is_closer_line("  });"));
        assert!(is_closer_line("})?;"));
        assert!(!is_closer_line("} else {"));
        assert!(!is_closer_line(""));
        assert!(!is_closer_line("   "));

        // `} else {` closes one block and opens another; swallowing it would
        // merge the two into one guide run.
        let src = "if a {\n    x\n} else {\n    y\n}";
        let b = blocks_of(src, 4, 4);
        assert_eq!(
            b,
            vec![
                IndentBlock {
                    col: 0,
                    start_line: 0,
                    end_line: 1
                },
                IndentBlock {
                    col: 0,
                    start_line: 2,
                    end_line: 4
                },
            ]
        );
    }

    #[test]
    fn nested_blocks_close_independently() {
        let src = "a:\n  b:\n    c\n  d\ne";
        let b = blocks_of(src, 2, 2);
        assert_eq!(
            b,
            vec![
                IndentBlock {
                    col: 0,
                    start_line: 0,
                    end_line: 3
                },
                IndentBlock {
                    col: 2,
                    start_line: 1,
                    end_line: 2
                },
            ]
        );
    }

    #[test]
    fn multi_level_jump_emits_a_guide_per_grid_column() {
        // Opener at 0, body at 8, shiftwidth 4 — two levels, one range.
        let src = "fn f() {\n        deep();\n}";
        let b = blocks_of(src, 4, 4);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].col, 0);
        assert_eq!(b[1].col, 4);
        assert_eq!((b[0].start_line, b[0].end_line), (0, 2));
        assert_eq!((b[1].start_line, b[1].end_line), (0, 2));
        // Both paint on the body line; neither paints on the opener or closer.
        assert_eq!(painted(src, 4, 4)[1], vec![0, 4]);
    }

    #[test]
    fn tabs_and_spaces_produce_identical_blocks() {
        let tabbed = "fn f() {\n\tif c {\n\t\twork();\n\t}\n}";
        let spaced = "fn f() {\n    if c {\n        work();\n    }\n}";
        assert_eq!(
            blocks_of(tabbed, 4, 4),
            blocks_of(spaced, 4, 4),
            "a tab at tabstop=4 is four columns, not one character"
        );
    }

    #[test]
    fn tabstop_changes_the_grid_a_tab_lands_on() {
        let tabbed = "fn f() {\n\tbody\n}";
        // At tabstop=8 the body sits at column 8, so shiftwidth=4 puts two
        // guides under it rather than one.
        assert_eq!(blocks_of(tabbed, 4, 8).len(), 2);
        assert_eq!(blocks_of(tabbed, 4, 4).len(), 1);
    }

    #[test]
    fn step_not_dividing_the_indent_still_anchors_on_the_opener() {
        // Opener at 0, body at 6, step 4 → guides at 0 and 4, both < 6.
        let src = "x:\n      deep\ny";
        let b = blocks_of(src, 4, 4);
        assert_eq!(b.iter().map(|b| b.col).collect::<Vec<_>>(), vec![0, 4]);
    }

    #[test]
    fn continuation_indent_anchors_the_grid_on_the_opener_column() {
        // A block opened by an already-indented line puts its guide at that
        // line's column, not at a multiple of `step` from zero.
        let src = "   odd:\n       body\n   done";
        let b = blocks_of(src, 4, 4);
        assert_eq!(b.iter().map(|b| b.col).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn zero_step_is_clamped_rather_than_looping() {
        let lines = line_indents("a:\n    b\nc".split('\n'), &unit(4, 4));
        let b = indent_blocks(&lines, 0);
        assert_eq!(b.len(), 4, "step clamped to 1 → guides at 0,1,2,3");
    }

    #[test]
    fn unterminated_block_ends_at_the_last_content_line() {
        let src = "fn f() {\n    body";
        assert_eq!(
            blocks_of(src, 4, 4),
            vec![IndentBlock {
                col: 0,
                start_line: 0,
                end_line: 1
            }]
        );
    }

    #[test]
    fn block_count_is_capped() {
        // Every line deeper than the last: one block opens per line and none
        // close until EOF.
        let src: String = (0..8000)
            .map(|i| format!("{}x", " ".repeat(i)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blocks_of(&src, 1, 1).len() <= MAX_BLOCKS);
    }

    #[test]
    fn single_jump_level_count_is_capped() {
        let src = format!("x:\n{}deep\ny", " ".repeat(10_000));
        assert_eq!(blocks_of(&src, 1, 1).len(), MAX_LEVELS_PER_BLOCK as usize);
    }

    #[test]
    fn contains_covers_opener_and_closer() {
        let b = IndentBlock {
            col: 0,
            start_line: 2,
            end_line: 6,
        };
        assert!(b.contains(2));
        assert!(b.contains(6));
        assert!(!b.contains(1));
        assert!(!b.contains(7));
        // Membership is wider than painting: the opener is in the block but
        // its column holds text.
        assert!(!b.paints_on(2, Some(0)));
        assert!(b.paints_on(3, Some(4)));
    }
}
