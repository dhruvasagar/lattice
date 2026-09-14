//! S4.final.c (2026-05-27): hit-testing on the cell grid.
//!
//! Pixel ↔ column ↔ source-byte conversions for the cell-grid
//! paint path. Designed so a future mouse-select / drag-select
//! handler in `window.rs` can map a mouse position to a buffer
//! coordinate without going through `ShapedLine`.
//!
//! The three primitives:
//! - [`x_to_combined_col`] — mouse x → cell column.
//! - [`col_to_x`] — cell column → x origin (inverse of above).
//! - [`combined_col_to_byte`] — cell column → source-byte
//!   position in the original line. Inverse of the existing
//!   [`crate::editor_element::byte_to_combined_col`].
//!
//! ## Where ShapedLine used to be
//!
//! GPUI's `ShapedLine::closest_index_for_x` walks shaped glyph
//! positions to map an x-coordinate to a character index. On
//! the cell grid, every cell has a uniform `advance` width, so
//! the x → col walk collapses to `(x / advance) as u32`. The
//! col → byte walk still needs to know the line's inlay
//! offsets to skip over inlay-spliced columns that have no
//! corresponding source byte.
//!
//! MO.2 is the consumer S4.final.c anticipated:
//! `EditorElement::paint` registers `window.on_mouse_event`
//! handlers that map a window position through these three
//! primitives to a buffer coordinate, and dispatch
//! `Action::MouseGoto`.

#![cfg(feature = "window")]

use crate::cells_paint::RowCoords;
use gpui::Pixels;

/// Map an x-coordinate within a line's text area to a
/// combined-column index. `x` is the offset from the line's
/// text origin (i.e. `mouse_x - text_origin_x`). Negative x
/// clamps to column 0; `advance <= 0` returns 0 (defensive
/// against pathological fonts).
///
/// For monospace single-font cells, every column has the same
/// pixel width, so this is `(x / advance).floor() as u32`.
/// Trailing-edge clicks land on the column the mouse is
/// *inside*, not the next one — matches the conventional
/// terminal cursor placement under a mouse click.
pub fn x_to_combined_col(advance: Pixels, x: Pixels) -> u32 {
    if x <= Pixels::ZERO || advance <= Pixels::ZERO {
        0
    } else {
        // `Div<Pixels> for Pixels` yields raw `f32`; floor and
        // saturating-cast to `u32` (negative would already be
        // caught above; the cast saturates on overflow).
        (x / advance).floor() as u32
    }
}

/// The x-origin of column `col` relative to the line's text
/// origin. Inverse of [`x_to_combined_col`] modulo the
/// integer-floor in that direction.
///
/// Cursor positioning in `EditorElement::paint` is currently
/// `text_origin_x + glyph_advance * (char_col as f32)` — this
/// helper formalises that calculation so future call sites can
/// reuse it without inlining the multiply.
pub fn col_to_x(advance: Pixels, col: u32) -> Pixels {
    advance * (col as f32)
}

/// Map a combined-column position back to the source-byte offset in
/// the original line — the inverse of
/// [`crate::editor_element::byte_to_combined_col`], and what a click
/// needs.
///
/// # Derived from the forward map, not re-derived
///
/// This binary-searches the forward map for the largest char boundary
/// whose column is still `<= col`, rather than walking the splices
/// backwards. Two reasons, and the second is the load-bearing one:
///
/// 1. It cannot disagree with where the caret is drawn, because it *is*
///    the function the caret is placed with. The peer primitive in
///    `lattice_cells::display_col_to_source_byte` is built the same way
///    for the same reason.
/// 2. The hand-written walk this replaces knew about inlays and **not
///    about conceal**, which the forward map has subtracted since H.3.
///    In a buffer with conceal rules — an org file's links, a markdown
///    heading — every column past the first hidden range resolved to
///    the wrong byte. Inverting the forward map picks conceal up for
///    free, and picks up whatever the forward map learns next.
///
/// Sound because the forward map is monotonic non-decreasing in `byte`:
/// each char adds one column plus any inlay spliced ahead of it, and a
/// char inside a hidden range adds zero.
///
/// # Where a click inside virtual or hidden text lands
///
/// **On inlay text, the source byte the inlay is anchored at** — the
/// position the annotation is about, and where the caret already sits.
/// **On a concealed span, the first visible byte at that column**,
/// since every byte in the range shares the range's start column and
/// the largest of them is the one actually drawn there.
///
/// `col` past the end of the line returns `line.len()`, which is the
/// ordinary case rather than a guard: a window reports a position for
/// every pixel in the pane, including the blank ones right of the text.
pub fn combined_col_to_byte(line: &str, col: u32, coords: &RowCoords) -> usize {
    // Char boundaries plus the end, which is a valid cursor position
    // and the answer for every click past the last glyph.
    let mut stops: Vec<usize> = line.char_indices().map(|(b, _)| b).collect();
    stops.push(line.len());

    let mut lo = 0usize;
    let mut hi = stops.len() - 1;
    while lo < hi {
        // Bias up so `lo = mid` always advances; the usual
        // rounding-down midpoint fails to terminate on `hi == lo + 1`.
        let mid = lo + (hi - lo).div_ceil(2);
        if crate::editor_element::byte_to_combined_col(line, stops[mid], coords) <= col as usize {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    stops[lo]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    // ----- x ↔ col -----

    /// Click at x=0 → column 0. Defensive baseline.
    #[test]
    fn x_zero_maps_to_col_zero() {
        assert_eq!(x_to_combined_col(px(8.0), px(0.0)), 0);
    }

    /// Negative x (mouse left of text area) clamps to 0.
    #[test]
    fn negative_x_clamps_to_zero() {
        assert_eq!(x_to_combined_col(px(8.0), px(-100.0)), 0);
    }

    /// Click on the right edge of column N lands on N
    /// (`floor`). Conventional terminal cursor placement.
    #[test]
    fn x_within_cell_floors_to_that_col() {
        let advance = px(8.0);
        assert_eq!(x_to_combined_col(advance, px(0.0)), 0);
        assert_eq!(x_to_combined_col(advance, px(7.9)), 0);
        assert_eq!(x_to_combined_col(advance, px(8.0)), 1);
        assert_eq!(x_to_combined_col(advance, px(15.9)), 1);
        assert_eq!(x_to_combined_col(advance, px(80.0)), 10);
    }

    /// Pathological advance (≤ 0) returns 0 instead of
    /// dividing by zero / negative.
    #[test]
    fn nonpositive_advance_returns_zero() {
        assert_eq!(x_to_combined_col(px(0.0), px(100.0)), 0);
        assert_eq!(x_to_combined_col(px(-1.0), px(100.0)), 0);
    }

    /// `col_to_x` inverts `x_to_combined_col` at column origins.
    #[test]
    fn col_to_x_returns_column_origin() {
        let advance = px(8.0);
        assert_eq!(col_to_x(advance, 0), px(0.0));
        assert_eq!(col_to_x(advance, 1), px(8.0));
        assert_eq!(col_to_x(advance, 10), px(80.0));
    }

    /// Round-trip: `x_to_combined_col(col_to_x(c))` = `c`.
    #[test]
    fn x_col_round_trip_at_origins() {
        let advance = px(8.0);
        for c in [0u32, 1, 5, 10, 80] {
            let x = col_to_x(advance, c);
            assert_eq!(
                x_to_combined_col(advance, x),
                c,
                "round-trip failed for col {c}"
            );
        }
    }

    // ----- col → byte -----

    fn coords(inlays: &[(u32, u32)], conceals: &[(u32, u32)]) -> RowCoords {
        RowCoords {
            inlays: inlays.to_vec(),
            conceals: conceals.to_vec(),
        }
    }

    /// The round-trip property, over a line carrying both an inlay
    /// splice and a conceal — the two move the column in opposite
    /// directions, so a sign error survives either one alone.
    ///
    /// Positions inside a hidden range are skipped: they have no column
    /// of their own, sharing the range's start column with the first
    /// visible byte after it, which is the one the inverse is
    /// documented to pick.
    #[test]
    fn every_visible_byte_round_trips_through_its_own_column() {
        let line = "let x = foo(bar);";
        let c = coords(&[(8, 4)], &[(4, 7)]);
        for (byte, _) in line
            .char_indices()
            .chain(std::iter::once((line.len(), ' ')))
        {
            // Hidden bytes are `[start, end)` — including the start,
            // which is hidden text even though the forward map gives it
            // a column of its own. None of them own a column: all share
            // the range's start column with the first visible byte
            // after it, which is the one the inverse picks.
            if (4..7).contains(&byte) {
                continue;
            }
            let col = crate::editor_element::byte_to_combined_col(line, byte, &c) as u32;
            assert_eq!(
                combined_col_to_byte(line, col, &c),
                byte,
                "byte {byte} sits at column {col} and must come back"
            );
        }
    }

    /// No inlays: col 0 → byte 0.
    #[test]
    fn no_inlays_col_zero_maps_to_byte_zero() {
        assert_eq!(combined_col_to_byte("hello", 0, &coords(&[], &[])), 0);
    }

    /// No inlays: col N → byte position of the N-th char.
    #[test]
    fn no_inlays_col_n_maps_to_byte_of_nth_char() {
        assert_eq!(combined_col_to_byte("hello", 3, &coords(&[], &[])), 3);
        assert_eq!(combined_col_to_byte("hello", 5, &coords(&[], &[])), 5);
    }

    /// Multi-byte chars (é = 2 utf-8 bytes): col → byte
    /// accounts for char_indices, not 1-byte-per-col.
    #[test]
    fn no_inlays_multibyte_char_byte_offsets() {
        // "café" = c(0) a(1) f(2) é(3-4); 4 chars, 5 bytes
        assert_eq!(combined_col_to_byte("café", 0, &coords(&[], &[])), 0);
        assert_eq!(combined_col_to_byte("café", 3, &coords(&[], &[])), 3); // start of é
        assert_eq!(combined_col_to_byte("café", 4, &coords(&[], &[])), 5); // after é
    }

    /// Col past end of line clamps to `line.len()`.
    #[test]
    fn col_past_end_clamps_to_line_len() {
        assert_eq!(combined_col_to_byte("hi", 10, &coords(&[], &[])), 2);
    }

    /// **A click on inlay text lands on the source byte BEFORE the
    /// splice**, and this is a deliberate change from the hand-written
    /// walk this function replaced.
    ///
    /// That walk snapped *forward* to the inlay's anchor byte. But the
    /// forward map draws the anchor byte's caret on the far side of the
    /// inlay — anchor byte 5 with a 3-column hint renders at column 8 —
    /// so clicking column 5 and getting byte 5 put the caret three
    /// columns to the RIGHT of the click. Inverting the forward map
    /// instead yields byte 4, whose caret renders at column 4: one
    /// column left of the click, which reads as ordinary rounding.
    ///
    /// Layout: `hello[HINT───] world`, hint anchored at byte 5, 3 wide.
    /// Columns 5–7 are the hint's own; no source byte lives under them,
    /// and the honest answer for all three is the byte before.
    #[test]
    fn a_click_on_inlay_text_lands_before_the_splice() {
        let line = "hello world";
        let c = coords(&[(5, 3)], &[]);

        assert_eq!(combined_col_to_byte(line, 0, &c), 0);
        assert_eq!(combined_col_to_byte(line, 4, &c), 4);
        // The hint's three columns.
        for hint_col in 5..8 {
            assert_eq!(
                combined_col_to_byte(line, hint_col, &c),
                4,
                "column {hint_col} is virtual text, so it belongs to the \
                 byte before it"
            );
        }
        // Column 8 is where the forward map puts byte 5 — the first
        // real character past the hint.
        assert_eq!(combined_col_to_byte(line, 8, &c), 5);
        assert_eq!(combined_col_to_byte(line, 9, &c), 6);
    }

    /// Two hints on one line compose, and every real column still
    /// resolves to the byte the forward map draws there.
    ///
    /// Layout: `[H2]abc[H1]de` — a 2-wide hint anchored at byte 0 and a
    /// 1-wide hint at byte 3. The forward map puts byte 0 at column 2,
    /// byte 3 at column 6.
    #[test]
    fn multiple_inlays_compose() {
        let line = "abcde";
        let c = coords(&[(0, 2), (3, 1)], &[]);

        // The leading hint occupies columns 0-1; nothing precedes it,
        // so a click there clamps to the start of the line.
        assert_eq!(combined_col_to_byte(line, 0, &c), 0);
        assert_eq!(combined_col_to_byte(line, 1, &c), 0);
        // Real columns.
        assert_eq!(combined_col_to_byte(line, 2, &c), 0, "'a'");
        assert_eq!(combined_col_to_byte(line, 3, &c), 1, "'b'");
        assert_eq!(combined_col_to_byte(line, 4, &c), 2, "'c'");
        // Column 5 is the second hint's own column → the byte before it.
        assert_eq!(combined_col_to_byte(line, 5, &c), 2);
        assert_eq!(combined_col_to_byte(line, 6, &c), 3, "'d'");
        assert_eq!(combined_col_to_byte(line, 7, &c), 4, "'e'");
    }

    /// **Conceal, which the walk this replaced could not see at all.**
    /// The forward map has subtracted hidden ranges since H.3, so every
    /// column past the first one resolved to the wrong byte. A click on
    /// a concealed span lands on the first visible byte at that column
    /// — the character actually drawn there.
    #[test]
    fn a_click_on_a_concealed_span_lands_past_the_hidden_text() {
        // `[[id:...][label]]` shaped: bytes 3..9 hidden.
        let line = "abcHIDDENxyz";
        let c = coords(&[], &[(3, 9)]);

        assert_eq!(combined_col_to_byte(line, 0, &c), 0);
        assert_eq!(
            combined_col_to_byte(line, 3, &c),
            9,
            "column 3 shows what follows the hidden span"
        );
        assert_eq!(combined_col_to_byte(line, 4, &c), 10);
        assert_eq!(combined_col_to_byte(line, 5, &c), 11);
    }

    /// A hint anchored at EOL owns the columns past the text, so a
    /// click in them belongs to the last real byte — and the column the
    /// forward map puts EOL at still resolves to EOL.
    #[test]
    fn trailing_inlay_columns_belong_to_the_last_byte() {
        let line = "abc";
        let c = coords(&[(3, 2)], &[]);

        assert_eq!(combined_col_to_byte(line, 0, &c), 0);
        assert_eq!(combined_col_to_byte(line, 2, &c), 2);
        // Columns 3-4 are the hint's; byte 3 (EOL) is drawn at column 5
        // because the hint precedes it.
        assert_eq!(combined_col_to_byte(line, 3, &c), 2);
        assert_eq!(combined_col_to_byte(line, 4, &c), 2);
        assert_eq!(combined_col_to_byte(line, 5, &c), 3, "EOL");
        assert_eq!(combined_col_to_byte(line, 99, &c), 3, "and past it");
    }
}
