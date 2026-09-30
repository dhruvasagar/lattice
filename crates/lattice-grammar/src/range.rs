//! The vim grammar `Range` -- the dispatcher's range arg.
//!
//! Distinct from `lattice_protocol::position::Range` (which is a structural
//! `[start, end)` byte range used by edits and decorations). The grammar
//! `Range` carries vim's ex-syntax range forms: `:1,5`, `:%`, `:'<,'>`,
//! `:.,+10`, `Selection` (active visual region), plugin-supplied custom
//! ranges.

use serde::{Deserialize, Serialize};

use crate::registry::RangeId;

/// A vim range argument: *which lines* an ex-command or operator covers,
/// before it is resolved against a document.
///
/// Symbolic on purpose: `%` or `'<,'>` means different lines in different
/// buffers and at different times, so the value is carried unresolved (in a
/// [`crate::CommandInvocation`], a recorded macro, a plugin call) and each
/// consumer resolves it at apply time.
///
/// Resolution coverage is uneven today. The dispatcher's operator path
/// resolves `CurrentLine`, `Whole` and `Selection` and rejects `Span` /
/// `Custom` with [`crate::CommandError::InvalidArgs`]; `:narrow` resolves
/// every form (patterns fall back to the cursor line, `Custom` to the
/// cursor line). No parser in the tree produces `Span` or `Custom` yet.
///
/// # Examples
///
/// ```
/// use lattice_grammar::{Range, RangeBound};
///
/// // `:.,+10` -- from the cursor line to ten lines below it.
/// let r = Range::Span {
///     start: RangeBound::CurrentLine,
///     end: RangeBound::Offset {
///         base: Box::new(RangeBound::CurrentLine),
///         delta: 10,
///     },
/// };
/// assert!(matches!(r, Range::Span { .. }));
///
/// // `:'<,'>` -- the last Visual selection, via its marks.
/// let visual = Range::Span {
///     start: RangeBound::Mark('<'),
///     end: RangeBound::Mark('>'),
/// };
/// assert_ne!(visual, Range::Selection); // same lines, different form
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Range {
    /// `:1,5`, `:'<,'>`, `:.,+10`, etc. Both ends inclusive; a consumer
    /// that resolves `start` below `end` swaps them (vim asks, lattice
    /// swaps silently).
    Span {
        /// First line of the range.
        start: RangeBound,
        /// Last line of the range (inclusive).
        end: RangeBound,
    },
    /// `:.` -- the cursor's line.
    CurrentLine,
    /// `:%` -- every line of the buffer.
    Whole,
    /// The current Visual / active region (the default range while Visual
    /// is active). Unlike the other forms it keeps Visual's shape: the
    /// dispatcher resolves it charwise (head-inclusive), linewise or
    /// blockwise according to the selection's mode.
    Selection,
    /// Plugin-registered custom range (e.g., a git-hunk-range plugin),
    /// identified by its registry id.
    Custom(RangeId),
}

/// One end of a [`Range::Span`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeBound {
    /// Absolute line number, **0-based** (the user's `:3` is `Line(2)`);
    /// clamped to the last line on resolution.
    Line(u32),
    /// A mark's line (`'a`, `'<`, `'>`, etc.). `<` / `>` resolve to the last
    /// Visual selection's first / last line; an unset mark resolves to the
    /// cursor line.
    Mark(char),
    /// `.` -- the cursor's line.
    CurrentLine,
    /// `$` -- the buffer's last line.
    LastLine,
    /// Pattern-relative (`/foo/`, `?bar?`): the pattern text, without
    /// delimiters. Not searched yet -- resolves to the cursor line.
    Pattern(String),
    /// Offset from another bound (`+1`, `-3`, `.+5`); the result is
    /// clamped to the buffer.
    Offset {
        /// The bound the offset is relative to.
        base: Box<RangeBound>,
        /// Signed line delta added to `base`'s line.
        delta: i32,
    },
}

/// The inclusive whole lines an operator's byte span covers, given its start
/// and end `(line, byte)` in either order.
///
/// A span ending at byte 0 of a later line is half-open: nothing on that line
/// is covered, so the last covered line is the one before. That's the shape a
/// forward exclusive motion leaves (`}`, `G`), and vim agrees:
/// `:h exclusive-linewise`, "the end is moved to the end of the previous line".
///
/// Known gap: a BACKWARD exclusive motion (`k` from column 0) also ends at byte
/// 0 of the cursor's own line, and from the span alone that can't be told apart,
/// so the cursor's line is dropped. Lattice has no linewise operator targets
/// yet (`dk` is charwise too); threading the cursor through is the fix when it
/// matters.
///
/// Shared by the narrow operator (`zn`) and the fold operator (`zf`), which is
/// why it lives here rather than in either.
///
/// Lines are 0-based; the returned `(first, last)` is inclusive and ordered.
///
/// # Examples
///
/// ```
/// use lattice_grammar::range::span_to_whole_lines;
///
/// // Ends mid-line on line 3: lines 0..=3 are covered.
/// assert_eq!(span_to_whole_lines(0, 0, 3, 5), (0, 3));
/// // Ends at byte 0 of line 3 (a forward exclusive motion like `}`):
/// // line 3 is not covered.
/// assert_eq!(span_to_whole_lines(0, 0, 3, 0), (0, 2));
/// // Reversed input is ordered.
/// assert_eq!(span_to_whole_lines(4, 2, 1, 7), (1, 4));
/// ```
pub fn span_to_whole_lines(
    start_line: u32,
    start_byte: u32,
    end_line: u32,
    end_byte: u32,
) -> (u32, u32) {
    let ((lo_line, _lo_byte), (hi_line, hi_byte)) = if start_line <= end_line {
        ((start_line, start_byte), (end_line, end_byte))
    } else {
        ((end_line, end_byte), (start_line, start_byte))
    };
    let mut end = hi_line;
    if hi_byte == 0 && end > lo_line {
        end -= 1;
    }
    (lo_line, end)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn whole_range_renders_distinct_variant() {
        assert_ne!(Range::Whole, Range::CurrentLine);
        assert_ne!(Range::Whole, Range::Selection);
    }

    #[test]
    fn span_constructed_from_bounds() {
        let r = Range::Span {
            start: RangeBound::Line(0),
            end: RangeBound::Line(4),
        };
        match r {
            Range::Span { start, end } => {
                assert_eq!(start, RangeBound::Line(0));
                assert_eq!(end, RangeBound::Line(4));
            }
            _ => panic!("expected Span"),
        }
    }

    #[test]
    fn offset_bounds_compose() {
        let off = RangeBound::Offset {
            base: Box::new(RangeBound::CurrentLine),
            delta: 5,
        };
        match off {
            RangeBound::Offset { base, delta } => {
                assert_eq!(*base, RangeBound::CurrentLine);
                assert_eq!(delta, 5);
            }
            _ => panic!("expected Offset"),
        }
    }

    #[test]
    fn span_to_whole_lines_mid_line_end_is_inclusive() {
        // `j`-like: next line, end mid-line → both lines covered.
        assert_eq!(span_to_whole_lines(0, 0, 3, 5), (0, 3));
    }

    #[test]
    fn span_to_whole_lines_half_open_end_at_col0_drops_trailing_line() {
        // Forward exclusive motions end at column 0 of the line AFTER the
        // last content line → the last covered line is the previous one.
        assert_eq!(span_to_whole_lines(0, 0, 3, 0), (0, 2));
    }

    #[test]
    fn span_to_whole_lines_single_line() {
        assert_eq!(span_to_whole_lines(2, 0, 2, 4), (2, 2));
    }

    #[test]
    fn span_to_whole_lines_reversed_is_ordered() {
        // A backward span (end before start) is ordered first. This also pins
        // the documented `k`-from-column-0 gap: line 5 is dropped.
        assert_eq!(span_to_whole_lines(5, 0, 2, 0), (2, 4));
    }
}
