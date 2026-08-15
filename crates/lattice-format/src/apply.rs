//! Turning a formatter's whole-file output into a minimal edit set.
//!
//! A formatter returns a new file, not a patch. Splicing that over the
//! buffer would be the obvious implementation and the wrong one: it
//! destroys cursor position, marks and folds, invalidates every
//! renderer fast path, and shows as a full-viewport repaint — a UX veto
//! even when the visible text is unchanged.
//!
//! `lattice-diff` already computes line-granular hunks for the diff
//! subsystem, and line granularity is the right resolution here: a
//! formatter's unit of change *is* the line.

use lattice_diff::compute::compute_diff;
use lattice_diff::types::DiffAlgorithm;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use ropey::Rope;

/// Edits that transform `old` into `new`, one per changed region.
///
/// Returned **bottom-up**, so applying them in order does not shift the
/// positions of edits not yet applied — the same ordering `>` / `<` /
/// `=` use.
///
/// An empty result means the formatter agreed with the buffer. That is
/// the common case for an already-formatted file and must cost nothing:
/// no edit, no undo entry, no repaint.
pub fn minimal_edits(old: &str, new: &str) -> Vec<Edit> {
    if old == new {
        return Vec::new();
    }
    let old_rope = Rope::from_str(old);
    let new_rope = Rope::from_str(new);
    let Ok(index) = compute_diff(&[old_rope, new_rope], DiffAlgorithm::default()) else {
        // The diff engine declined. Rather than fall back to a
        // whole-buffer replace — the exact thing this module exists to
        // avoid — report no edits and let the caller surface it.
        tracing::debug!("format: diff engine declined; applying no edits");
        return Vec::new();
    };

    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let old_line_count = old.split_inclusive('\n').count();

    let mut edits: Vec<Edit> = Vec::new();
    for hunk in &index.hunks {
        // `ranges[0]` is the old side, `ranges[1]` the new side.
        let (Some(old_range), Some(new_range)) = (hunk.ranges.first(), hunk.ranges.get(1)) else {
            continue;
        };
        let replacement: String = (new_range.start..new_range.end)
            .filter_map(|l| new_lines.get(l as usize).copied())
            .collect();

        let start = Position::new(old_range.start, 0);
        // A hunk's end is exclusive. Deleting through the start of the
        // following line takes the trailing newline with it, which is
        // what makes a pure deletion actually remove the line rather
        // than leave a blank one.
        let end = if old_range.end as usize >= old_line_count {
            Position::new(old_range.end.saturating_sub(1), u32::MAX)
        } else {
            Position::new(old_range.end, 0)
        };
        edits.push(Edit::replace(Range::new(start, end), replacement));
    }
    // Bottom-up.
    edits.sort_by(|a, b| b.range.start.line.cmp(&a.range.start.line));
    edits
}

/// Whether `new` differs from `old` in more than leading whitespace.
///
/// Used to tell an *indent* filter (`equalprg`, which is specified to
/// adjust leading whitespace only) from a *reformatter* that was
/// pointed at the wrong option. A tool that rewrites content is not an
/// indent filter, and running it from `=` would break the operator's
/// contract with motions.
pub fn changes_more_than_indentation(old: &str, new: &str) -> bool {
    let strip = |s: &str| {
        s.lines()
            .map(|l| l.trim_start_matches([' ', '\t']))
            .collect::<Vec<_>>()
            .join("\n")
    };
    strip(old) != strip(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply `edits` to `old` the way the buffer would, so the test
    /// checks the edits rather than restating the diff.
    fn apply(old: &str, edits: &[Edit]) -> String {
        let mut rope = Rope::from_str(old);
        for edit in edits {
            let r = edit.range;
            let start_line = (r.start.line as usize).min(rope.len_lines());
            let start = rope.line_to_char(start_line) + r.start.byte as usize;
            let end = if r.end.byte == u32::MAX {
                rope.len_chars()
            } else {
                let end_line = (r.end.line as usize).min(rope.len_lines());
                rope.line_to_char(end_line) + r.end.byte as usize
            };
            let start = start.min(rope.len_chars());
            let end = end.min(rope.len_chars()).max(start);
            rope.remove(start..end);
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            rope.insert(start, text);
        }
        rope.to_string()
    }

    #[test]
    fn an_already_formatted_buffer_produces_no_edits() {
        // The idempotence guard. If a whole-buffer replace ever sneaks
        // back in, this is what catches it — a replace is never empty.
        assert!(minimal_edits("a\nb\nc\n", "a\nb\nc\n").is_empty());
    }

    #[test]
    fn a_single_changed_line_touches_only_that_line() {
        let edits = minimal_edits("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(edits.len(), 1, "one hunk, one edit");
        assert_eq!(edits[0].range.start.line, 1);
        assert_eq!(apply("a\nb\nc\n", &edits), "a\nB\nc\n");
    }

    #[test]
    fn edits_round_trip_for_a_reindent() {
        let old = "fn f() {\nx();\n        y();\n}\n";
        let new = "fn f() {\n    x();\n    y();\n}\n";
        let edits = minimal_edits(old, new);
        assert!(!edits.is_empty());
        assert_eq!(apply(old, &edits), new);
    }

    #[test]
    fn edits_round_trip_for_an_insertion() {
        let old = "a\nd\n";
        let new = "a\nb\nc\nd\n";
        let edits = minimal_edits(old, new);
        assert_eq!(apply(old, &edits), new);
    }

    #[test]
    fn edits_round_trip_for_a_deletion() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nd\n";
        let edits = minimal_edits(old, new);
        assert_eq!(apply(old, &edits), new);
    }

    #[test]
    fn indent_only_changes_are_recognised_as_such() {
        assert!(!changes_more_than_indentation(
            "fn f() {\nx();\n}\n",
            "fn f() {\n    x();\n}\n"
        ));
        // A reformatter moving a brace is not an indent filter.
        assert!(changes_more_than_indentation(
            "fn f() {\nx();\n}\n",
            "fn f()\n{\n    x();\n}\n"
        ));
    }
}
