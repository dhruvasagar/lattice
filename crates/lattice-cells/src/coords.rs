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
    subtract_conceals(col, byte, conceals)
}

/// Map a display column back to the source position under it — the
/// inverse of [`source_byte_to_display_col`], and what a mouse click
/// needs.
///
/// `max_source_byte` is the source line's length in the same
/// already-char-resolved space the forward map uses (see the
/// byte-vs-char note on [`crate::row::CellRow::byte_to_combined_col`]):
/// the caller resolves char ↔ byte, this only undoes the column
/// arithmetic. The result is clamped to `[0, max_source_byte]`, so a
/// click past the end of a line lands on its end rather than failing.
///
/// # Derived from the forward map, not re-derived
///
/// This binary-searches [`source_byte_to_display_col`] for the largest
/// source position whose column is still `<= col`, rather than running
/// the inlay and conceal arithmetic backwards. That costs
/// `O(log line_len)` forward evaluations — nothing, on a path driven by
/// a human's hand — and buys the one property that matters: the inverse
/// cannot disagree with the forward map, because it *is* the forward
/// map. This module exists because three copies of the forward
/// arithmetic drifted; a hand-written inverse would be a fourth copy
/// with the same failure mode and a worse symptom, since a click that
/// lands one column off reads as a shaping bug rather than a missing
/// conceal rule.
///
/// The search is sound because the forward map is monotonic
/// non-decreasing: each source position adds one column plus any inlay
/// spliced ahead of it, and a position inside a hidden range adds zero.
///
/// # Where a click inside hidden or virtual text lands
///
/// **On a concealed range, the first VISIBLE position at that column.**
/// Every position in `[start, end]` maps to the range's start column, so
/// the largest of them wins — which is the position just past the hidden
/// text, i.e. the character actually drawn there. That is the right
/// answer for a click and the mirror image of the forward map's clamp,
/// which sends a hidden position *back* to the range's start column.
///
/// **On inlay text, the source position before the splice.** Inlay
/// columns have no source position of their own; the one before them is
/// the only truthful answer, and it is where the caret would already be
/// drawn.
pub fn display_col_to_source_byte(
    col: u32,
    max_source_byte: u32,
    inlay_offsets: &[(u32, u32)],
    conceals: &[ConcealRange],
) -> u32 {
    let mut lo = 0u32;
    let mut hi = max_source_byte;
    while lo < hi {
        // Bias the midpoint up so `lo = mid` always advances; with the
        // usual rounding-down midpoint this loop fails to terminate on
        // `hi == lo + 1`.
        let mid = lo + (hi - lo).div_ceil(2);
        if source_byte_to_display_col(mid, inlay_offsets, conceals) <= col {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Remove the concealed columns lying before `source_col` from an
/// otherwise-computed display column.
///
/// Split out from [`source_byte_to_display_col`] because the GPU peer
/// computes its inlay term differently — it resolves the source byte
/// to a char column itself and filters inlays by *byte* rather than by
/// column. Reconciling that is a separate question with its own
/// non-ASCII risk; what must not happen meanwhile is two
/// implementations of the conceal clamp, because the symptom of a
/// stale one is a caret sitting off its own match on one renderer and
/// not the other.
///
/// `col` is the display column before conceal; `source_col` is the
/// position in the source line's own column space, which is what
/// conceal ranges are expressed in.
pub fn subtract_conceals(col: u32, source_col: u32, conceals: &[ConcealRange]) -> u32 {
    let mut col = col;
    for (start, end) in conceals {
        if *start >= source_col {
            break;
        }
        // Only the part of this range lying strictly before
        // `source_col` is subtracted. For a position past the range
        // that is its whole width; for one inside it, exactly enough
        // to land on `start` — which is the clamp, falling out of the
        // arithmetic rather than needing a branch.
        col = col.saturating_sub(end.min(&source_col) - start);
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

    // ── display_col_to_source_byte ────────────────────────────────

    /// Plain text: the inverse is the identity, and a click past the
    /// end clamps to the end rather than running away.
    #[test]
    fn inverse_is_the_identity_without_inlays_or_conceals() {
        for col in 0..12u32 {
            assert_eq!(display_col_to_source_byte(col, 10, &[], &[]), col.min(10));
        }
    }

    /// The property that makes this worth having: for every position
    /// that HAS a column of its own, clicking that column selects it
    /// back. Stated over a line carrying both an inlay splice and a
    /// conceal, because the two terms move the column in opposite
    /// directions and a sign error survives either one alone.
    #[test]
    fn every_visible_position_round_trips_through_its_own_column() {
        let inlays = [(4u32, 6u32)];
        let conceals = [(8u32, 12u32)];
        let len = 20;
        for byte in 0..=len {
            // Hidden positions are `[start, end)` — INCLUDING the start,
            // which is hidden text even though the forward map gives it a
            // column of its own. None of them have a column to themselves:
            // all share the range's start column with the first visible
            // position after it, which is the one the inverse is
            // documented to pick.
            if byte >= conceals[0].0 && byte < conceals[0].1 {
                continue;
            }
            let col = source_byte_to_display_col(byte, &inlays, &conceals);
            assert_eq!(
                display_col_to_source_byte(col, len, &inlays, &conceals),
                byte,
                "position {byte} sits at column {col} and must come back"
            );
        }
    }

    /// A click on a concealed span lands on the character actually
    /// drawn there — the first position past the hidden text — not
    /// somewhere in the middle of bytes the user cannot see.
    #[test]
    fn a_click_on_a_concealed_span_lands_past_the_hidden_text() {
        let conceals = [(3u32, 9u32)];
        let col = source_byte_to_display_col(3, &[], &conceals);
        assert_eq!(
            display_col_to_source_byte(col, 20, &[], &conceals),
            9,
            "columns 3.. show the text after the hidden range, so that is \
             what clicking there selects"
        );
    }

    /// A click on inlay text resolves to the source position the inlay
    /// is spliced in front of. Inlay columns have no source position,
    /// and the one before them is where the caret is already drawn.
    #[test]
    fn a_click_on_inlay_text_lands_on_the_position_before_the_splice() {
        // 5 columns of virtual text spliced ahead of position 4.
        let inlays = [(4u32, 5u32)];
        assert_eq!(display_col_to_source_byte(3, 20, &inlays, &[]), 3);
        for inlay_col in 4..9 {
            assert_eq!(
                display_col_to_source_byte(inlay_col, 20, &inlays, &[]),
                3,
                "column {inlay_col} is virtual text, so it belongs to the \
                 position before it"
            );
        }
        assert_eq!(
            display_col_to_source_byte(9, 20, &inlays, &[]),
            4,
            "the first real column past the inlay is position 4"
        );
    }

    /// A click beyond the last column clamps to the end of the line.
    /// Terminals report a column for every cell in the row, including
    /// the blank ones past the text, so this is the common case rather
    /// than a defensive one.
    #[test]
    fn a_click_past_the_end_of_the_line_clamps_to_its_end() {
        assert_eq!(display_col_to_source_byte(999, 7, &[], &[]), 7);
        assert_eq!(
            display_col_to_source_byte(999, 0, &[], &[]),
            0,
            "empty line"
        );
    }

    /// Two hidden ranges on one line: the widths compose, and the
    /// inverse keeps agreeing with the forward map across both.
    #[test]
    fn the_inverse_composes_across_two_hidden_ranges() {
        let conceals = [(2u32, 5u32), (10u32, 14u32)];
        let len = 20;
        for byte in [0, 1, 2, 5, 6, 9, 10, 14, 15, 20] {
            let col = source_byte_to_display_col(byte, &[], &conceals);
            let back = display_col_to_source_byte(col, len, &[], &conceals);
            assert_eq!(
                source_byte_to_display_col(back, &[], &conceals),
                col,
                "position {byte} → column {col} → position {back}, which must \
                 sit at the same column"
            );
        }
    }
}
