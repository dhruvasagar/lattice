//! Computed folds (DESIGN.md §5.1, §15:18).
//!
//! Folds derived automatically from the buffer's structure. v1
//! status (C.2): indent-based fallback only -- a fold spans any
//! line whose successor indents deeper, ending at the last line
//! whose indent is strictly greater than the start. Tree-sitter-
//! driven folds (function bodies, classes, blocks via `folds.scm`)
//! are queued as a follow-up; the data type ([`crate::app::Fold`])
//! is shared so a tree-sitter pass and the indent fallback both
//! produce the same shape.
//!
//! Manual folds (created via `zf` from a Visual selection) and
//! computed folds coexist in [`crate::app::App::folds`] with no
//! distinction at the storage layer; the `:set foldmethod` option
//! decides which side feeds in.

use lattice_core::Buffer;

use crate::app::Fold;

/// Run the indent-based fold algorithm against `buffer` and return
/// every fold range it discovers. All produced folds are open
/// (`closed = false`) by default -- vim's `foldlevelstart` would
/// override that, but v1 doesn't model the level option yet.
///
/// Algorithm:
///
/// 1. For each non-blank line, compute its indent (count of
///    leading ASCII whitespace, treating tabs as one cell).
/// 2. Walk lines top-down; whenever line `i` has a non-blank
///    successor `j` with strictly greater indent, open a fold
///    starting at `i`. Walk forward to find the last line whose
///    indent is greater than `i`'s; that's the fold end.
/// 3. Skip blank lines when locating the fold end (they don't
///    break a fold -- vim's behaviour).
///
/// The output is sorted by start_line; nested folds appear
/// inside their parent's range.
pub fn compute_indent_folds(buffer: &Buffer) -> Vec<Fold> {
    let text = buffer.as_string();
    let lines: Vec<&str> = text.split('\n').collect();
    let line_count = lines.len();
    if line_count <= 1 {
        return Vec::new();
    }
    let indents: Vec<Option<usize>> = lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                None
            } else {
                Some(leading_indent(l))
            }
        })
        .collect();

    let mut folds: Vec<Fold> = Vec::new();
    for i in 0..line_count {
        let Some(start_indent) = indents[i] else {
            continue;
        };
        // Look for the next non-blank line.
        let next_non_blank = ((i + 1)..line_count).find(|j| indents[*j].is_some());
        let Some(j) = next_non_blank else {
            continue;
        };
        let Some(next_indent) = indents[j] else {
            continue;
        };
        if next_indent <= start_indent {
            continue;
        }
        // Walk forward to find the last line with indent > start_indent.
        let mut end = j;
        for (k, ind) in indents.iter().enumerate().skip(j + 1) {
            match ind {
                Some(i) if *i > start_indent => end = k,
                Some(_) => break,
                None => {
                    // Blank line: keep looking but don't extend
                    // the end past it unless a deeper line follows.
                    continue;
                }
            }
        }
        folds.push(Fold {
            start_line: i as u32,
            end_line: end as u32,
            closed: false,
        });
    }
    folds
}

/// Count leading whitespace cells. Tabs and spaces both count as
/// one cell -- vim's `foldmethod=indent` uses 'shiftwidth' instead
/// of raw whitespace count; v1 keeps this simple and treats every
/// leading whitespace byte as one indent unit. (Refinement when
/// we honour `tabstop` / `shiftwidth` lands with the typed-options
/// follow-up.)
fn leading_indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::Position;

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        if !text.is_empty() {
            b.apply_edit(&Edit::insert(Position::ZERO, text.to_string()))
                .unwrap();
        }
        b
    }

    #[test]
    fn empty_buffer_yields_no_folds() {
        let b = buf("");
        assert!(compute_indent_folds(&b).is_empty());
    }

    #[test]
    fn single_line_yields_no_folds() {
        let b = buf("hello");
        assert!(compute_indent_folds(&b).is_empty());
    }

    #[test]
    fn flat_lines_yield_no_folds() {
        let b = buf("a\nb\nc\nd\n");
        assert!(compute_indent_folds(&b).is_empty());
    }

    #[test]
    fn one_block_produces_a_fold() {
        let b = buf("def f():\n    pass\n");
        let folds = compute_indent_folds(&b);
        assert_eq!(folds.len(), 1);
        let f = &folds[0];
        assert_eq!(f.start_line, 0);
        assert_eq!(f.end_line, 1);
        assert!(!f.closed);
    }

    #[test]
    fn nested_blocks_produce_nested_folds() {
        let b = buf("outer:\n    inner:\n        deep\n        deeper\n    after-inner\n");
        let folds = compute_indent_folds(&b);
        // outer (0..4) and inner (1..3).
        assert!(folds.iter().any(|f| f.start_line == 0 && f.end_line == 4));
        assert!(folds.iter().any(|f| f.start_line == 1 && f.end_line == 3));
    }

    #[test]
    fn blank_lines_inside_a_block_dont_break_it() {
        let b = buf("def f():\n    line1\n\n    line2\n");
        let folds = compute_indent_folds(&b);
        assert_eq!(folds.len(), 1);
        // Fold extends to line 3 (last indented row); the blank
        // line on row 2 is skipped.
        assert_eq!(folds[0].end_line, 3);
    }

    #[test]
    fn blank_lines_at_top_dont_start_a_fold() {
        let b = buf("\n    indented\nfollowing\n");
        // Line 0 is blank; no fold should start there.
        let folds = compute_indent_folds(&b);
        assert!(folds.iter().all(|f| f.start_line != 0));
    }

    #[test]
    fn computed_folds_are_open_by_default() {
        let b = buf("a:\n    b\n");
        let folds = compute_indent_folds(&b);
        assert!(!folds[0].closed);
    }
}
