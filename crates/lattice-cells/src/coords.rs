//! Source byte → display column, in one place.
//!
//! Three carriers hold the same two tables — `CellRow` (the cell
//! path), `DisplayLine` (the display path), and the GPU peer's
//! per-row arrays — and every one of them has to answer the same
//! question: *given a source position, which column is it under?*
//!
//! Before conceal there was one term in that answer (inlay splices)
//! and three copies of a four-line loop, which was survivable. With
//! a second term the copies stop being survivable: an elision the
//! cursor agrees with and the search highlight does not is a caret
//! sitting off its own match, and the bug lives in whichever copy
//! was not updated. So the arithmetic lands here once and the
//! carriers delegate.
//!
//! Design anchor:
//! [`docs/dev/architecture/conceal.md`](../../../docs/dev/architecture/conceal.md).

/// A concealed source-byte range, `[start, end)`.
///
/// Hidden ranges occupy **zero** display columns. The list must be
/// sorted ascending by `start` and non-overlapping — the builder
/// coalesces before storing, because two overlapping ranges would
/// have their shared width subtracted twice and every column past
/// them on the line would be wrong.
pub type ConcealRange = (u32, u32);

/// Map a source byte to its display column.
///
/// `inlay_offsets` are `(orig_byte, extra_cols)` splices that *add*
/// columns; `conceals` are ranges that *remove* them. Both are in
/// the same already-char-resolved space the rest of the cell
/// substrate uses — see the byte-vs-char note on
/// [`crate::row::CellRow::byte_to_combined_col`]. In that space a
/// hidden range removes exactly `end - start` columns, which is why
/// conceal needs no width table of its own.
///
/// # A byte inside a concealed range
///
/// It has no column of its own, and it resolves to the column of
/// its range's **start** — the first visible position at or before
/// it. That is not a special case in the code below: subtracting
/// only the concealed width that lies strictly before `byte` yields
/// the range's start column on its own.
///
/// Landing there is deliberate. The alternative — letting the
/// subtraction run past `byte` — produces a column *between* the
/// range's endpoints, which is worse than either end precisely
/// because it looks plausible: a caret one column into a hidden
/// span reads as an off-by-one in the shaper rather than as a
/// missing rule.
pub fn source_byte_to_display_col(
    byte: u32,
    inlay_offsets: &[(u32, u32)],
    conceals: &[ConcealRange],
) -> u32 {
    let mut col = byte;
    for (orig_byte, width) in inlay_offsets {
        if *orig_byte <= byte {
            col = col.saturating_add(*width);
        } else {
            break;
        }
    }
    for (start, end) in conceals {
        if *start >= byte {
            break;
        }
        // Only the part of this range lying strictly before `byte`
        // is subtracted. For a `byte` past the range that is its
        // whole width; for a `byte` inside it, exactly enough to
        // land on `start`.
        col = col.saturating_sub(end.min(&byte) - start);
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tables_is_the_identity() {
        for b in 0..8 {
            assert_eq!(source_byte_to_display_col(b, &[], &[]), b);
        }
    }

    #[test]
    fn inlays_alone_behave_exactly_as_before() {
        // Pinned against `CellRow::byte_to_combined_col`'s own
        // cases, so the shared function is a drop-in for the loop
        // it replaces rather than a re-derivation of it.
        let inlays = [(1u32, 2u32), (3u32, 1u32)];
        assert_eq!(source_byte_to_display_col(0, &inlays, &[]), 0);
        assert_eq!(source_byte_to_display_col(1, &inlays, &[]), 3);
        assert_eq!(source_byte_to_display_col(2, &inlays, &[]), 4);
        assert_eq!(source_byte_to_display_col(3, &inlays, &[]), 6);
        assert_eq!(source_byte_to_display_col(5, &inlays, &[]), 8);
    }

    #[test]
    fn a_byte_before_a_concealed_range_is_untouched() {
        let c = [(4u32, 9u32)];
        assert_eq!(source_byte_to_display_col(0, &[], &c), 0);
        assert_eq!(source_byte_to_display_col(3, &[], &c), 3);
    }

    #[test]
    fn a_byte_at_the_start_of_a_concealed_range_is_its_own_column() {
        let c = [(4u32, 9u32)];
        assert_eq!(source_byte_to_display_col(4, &[], &c), 4);
    }

    #[test]
    fn every_byte_inside_a_concealed_range_clamps_to_its_start() {
        let c = [(4u32, 9u32)];
        for b in 4..=9 {
            assert_eq!(
                source_byte_to_display_col(b, &[], &c),
                4,
                "byte {b} inside [4,9) must resolve to the range's start column"
            );
        }
    }

    #[test]
    fn a_byte_after_a_concealed_range_loses_its_whole_width() {
        let c = [(4u32, 9u32)];
        // 5 columns hidden.
        assert_eq!(source_byte_to_display_col(10, &[], &c), 5);
        assert_eq!(source_byte_to_display_col(20, &[], &c), 15);
    }

    #[test]
    fn two_concealed_ranges_accumulate() {
        let c = [(2u32, 4u32), (8u32, 11u32)];
        assert_eq!(source_byte_to_display_col(1, &[], &c), 1);
        assert_eq!(source_byte_to_display_col(6, &[], &c), 4); // -2
        assert_eq!(source_byte_to_display_col(9, &[], &c), 6); // -2, clamped into the second
        assert_eq!(source_byte_to_display_col(15, &[], &c), 10); // -2 -3
    }

    #[test]
    fn an_inlay_and_a_conceal_compose_in_byte_order() {
        // `[[x][hi]]`-shaped: hide [0,4) and [6,9), inlay +3 at 5.
        let inlays = [(5u32, 3u32)];
        let conceals = [(0u32, 4u32), (6u32, 9u32)];
        // Byte 4 — first visible byte. Inlay is past it; 4 hidden before.
        assert_eq!(source_byte_to_display_col(4, &inlays, &conceals), 0);
        // Byte 5 — the inlay anchor: +3 for the inlay, -4 hidden.
        assert_eq!(source_byte_to_display_col(5, &inlays, &conceals), 4);
        // Byte 12 — past everything: +3 inlay, -4 -3 hidden.
        assert_eq!(source_byte_to_display_col(12, &inlays, &conceals), 8);
    }

    #[test]
    fn a_line_concealed_from_its_first_byte_never_goes_negative() {
        // The saturating path: more hidden than there are columns
        // cannot underflow into a huge u32.
        let c = [(0u32, 40u32)];
        assert_eq!(source_byte_to_display_col(0, &[], &c), 0);
        assert_eq!(source_byte_to_display_col(40, &[], &c), 0);
        assert_eq!(source_byte_to_display_col(41, &[], &c), 1);
    }

    #[test]
    fn a_whole_line_hidden_leaves_every_byte_at_column_zero() {
        let c = [(0u32, 12u32)];
        for b in 0..=12 {
            assert_eq!(source_byte_to_display_col(b, &[], &c), 0);
        }
    }
}
