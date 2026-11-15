//! The exchange operator (RF.4d), vim-exchange's `cx`: the first use marks a
//! range, the second swaps the two.
//!
//! **Text only, no tree.** The two ranges are whatever the Visual selection,
//! motion or text object drew, and the swap is one replacement spanning both
//! — so one undo step, and no second range to keep valid while the first is
//! rewritten.
//!
//! **The mark is the host's.** The first range is tracked by the document
//! (`track-range`), so it moves with every edit made between the two `cx`s,
//! whoever makes it, and the host tints it with no guest code on the paint
//! path. This module decides only what two ranges come to.

use crate::span::Span;

/// How two exchanged ranges relate, which decides the edit.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// The same text twice: nothing to swap.
    Same,
    /// They overlap without either containing the other. A swap would have
    /// to put some text in two places, so it is refused.
    Overlap,
    /// One lies inside the other: the outer is replaced by the inner
    /// (vim-exchange's rule — exchanging an expression with a part of it
    /// keeps the part).
    Contains { outer: Span, inner: Span },
    /// Apart: `first` comes before `second` in the buffer.
    Disjoint { first: Span, second: Span },
}

/// What exchanging `marked` with `new` comes to.
pub fn plan(marked: Span, new: Span) -> Plan {
    if marked == new {
        return Plan::Same;
    }
    if marked.covers(new) {
        return Plan::Contains {
            outer: marked,
            inner: new,
        };
    }
    if new.covers(marked) {
        return Plan::Contains {
            outer: new,
            inner: marked,
        };
    }
    let (first, second) = if marked.start <= new.start {
        (marked, new)
    } else {
        (new, marked)
    };
    if first.end > second.start {
        return Plan::Overlap;
    }
    Plan::Disjoint { first, second }
}

/// The replacement text for a disjoint swap over `first.start..second.end`:
/// the second's text, the gap between them unchanged, then the first's.
pub fn swapped(first: &str, gap: &str, second: &str) -> String {
    format!("{second}{gap}{first}")
}

#[cfg(test)]
mod tests {
    use super::{plan, swapped, Plan};
    use crate::span::{Pos, Span};

    fn s(a: (u32, u32), b: (u32, u32)) -> Span {
        Span {
            start: Pos {
                line: a.0,
                byte: a.1,
            },
            end: Pos {
                line: b.0,
                byte: b.1,
            },
        }
    }

    #[test]
    fn apart_ranges_are_ordered_whichever_was_marked_first() {
        let (a, b) = (s((0, 0), (0, 3)), s((0, 5), (0, 8)));
        let want = Plan::Disjoint {
            first: a,
            second: b,
        };
        assert_eq!(plan(a, b), want);
        assert_eq!(plan(b, a), want, "marking the later one first");
    }

    #[test]
    fn touching_ranges_are_apart_not_overlapping() {
        let (a, b) = (s((0, 0), (0, 3)), s((0, 3), (0, 6)));
        assert!(matches!(plan(a, b), Plan::Disjoint { .. }));
    }

    #[test]
    fn a_range_inside_the_other_replaces_the_larger_with_the_smaller() {
        let (outer, inner) = (s((0, 0), (0, 9)), s((0, 2), (0, 4)));
        let want = Plan::Contains { outer, inner };
        assert_eq!(plan(outer, inner), want);
        assert_eq!(plan(inner, outer), want);
    }

    #[test]
    fn a_partial_overlap_is_refused_and_the_same_range_is_a_no_op() {
        assert_eq!(plan(s((0, 0), (0, 5)), s((0, 3), (0, 8))), Plan::Overlap);
        assert_eq!(plan(s((1, 0), (1, 2)), s((1, 0), (1, 2))), Plan::Same);
    }

    #[test]
    fn the_gap_between_the_two_is_kept_verbatim() {
        assert_eq!(swapped("foo", ", ", "bar"), "bar, foo");
        assert_eq!(swapped("a", "\n", "b"), "b\na");
    }
}
