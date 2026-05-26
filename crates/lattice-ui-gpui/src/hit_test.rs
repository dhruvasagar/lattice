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
//! No mouse handler consumes these primitives today — the
//! editor body in `window.rs` has no `on_mouse_down` listener
//! yet. S4.final.c lands the infrastructure so the eventual
//! handler is a one-line `x_to_combined_col(...)` call.

#![cfg(feature = "window")]

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

/// Map a combined-column position back to the source-byte
/// offset in the original (pre-inlay-splice) line. Inverse of
/// [`crate::editor_element::byte_to_combined_col`].
///
/// `inlay_offsets` is the row's `(orig_byte, char_width)`
/// list, sorted by `orig_byte` — the same shape stored on
/// [`lattice_cells::CellRow::inlay_offsets`] and on the
/// element's `inlay_offsets_per_row`. The walk:
///
/// 1. Iterate the source chars by `char_indices`.
/// 2. Before consuming each char, splice in any inlays whose
///    `orig_byte` equals the current source-byte position —
///    each inlay's `char_width` consumes that many combined
///    columns without advancing the source byte.
/// 3. If `col` is exhausted inside an inlay, return the
///    source byte where the inlay was spliced (the char
///    immediately following it). This matches the user's
///    expectation that a click on an inlay snaps to the
///    code position the inlay is annotating.
/// 4. If `col` is exhausted on a source char, return that
///    char's start byte.
/// 5. If `col` outruns the line, return `line.len()`.
pub fn combined_col_to_byte(
    line: &str,
    col: u32,
    inlay_offsets: &[(u32, u32)],
) -> usize {
    let mut remaining = col;
    let mut inlay_idx = 0;
    for (b, ch) in line.char_indices() {
        // Splice any inlays anchored at this byte before
        // consuming the source char.
        while inlay_idx < inlay_offsets.len()
            && inlay_offsets[inlay_idx].0 as usize == b
        {
            let width = inlay_offsets[inlay_idx].1;
            if width > remaining {
                // Click landed inside the inlay → snap to the
                // source byte the inlay is anchored at.
                return b;
            }
            remaining -= width;
            inlay_idx += 1;
        }
        if remaining == 0 {
            return b;
        }
        remaining -= 1;
        if remaining == 0 {
            return b + ch.len_utf8();
        }
    }
    // EOL inlays (anchored at or past `line.len()`).
    while inlay_idx < inlay_offsets.len() {
        let width = inlay_offsets[inlay_idx].1;
        if width > remaining {
            return line.len();
        }
        remaining -= width;
        inlay_idx += 1;
    }
    line.len()
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

    /// No inlays: col 0 → byte 0.
    #[test]
    fn no_inlays_col_zero_maps_to_byte_zero() {
        assert_eq!(combined_col_to_byte("hello", 0, &[]), 0);
    }

    /// No inlays: col N → byte position of the N-th char.
    #[test]
    fn no_inlays_col_n_maps_to_byte_of_nth_char() {
        assert_eq!(combined_col_to_byte("hello", 3, &[]), 3);
        assert_eq!(combined_col_to_byte("hello", 5, &[]), 5);
    }

    /// Multi-byte chars (é = 2 utf-8 bytes): col → byte
    /// accounts for char_indices, not 1-byte-per-col.
    #[test]
    fn no_inlays_multibyte_char_byte_offsets() {
        // "café" = c(0) a(1) f(2) é(3-4); 4 chars, 5 bytes
        assert_eq!(combined_col_to_byte("café", 0, &[]), 0);
        assert_eq!(combined_col_to_byte("café", 3, &[]), 3); // start of é
        assert_eq!(combined_col_to_byte("café", 4, &[]), 5); // after é
    }

    /// Col past end of line clamps to `line.len()`.
    #[test]
    fn col_past_end_clamps_to_line_len() {
        assert_eq!(combined_col_to_byte("hi", 10, &[]), 2);
    }

    /// Inlay anchored at byte 5 with width 3 columns. Returns
    /// reflect a cursor placed *before* the named source byte
    /// (the standard "insertion point" interpretation).
    ///
    /// Layout: `hello[INLAY-3-cols] world`
    /// - cols 0-4 cover `hello` (bytes 0-4 sequentially)
    /// - col 5 lands at the inlay anchor (between 'o' and ' '
    ///   in source bytes; byte 5)
    /// - cols 6-7 are *inside* the 3-col inlay → snap to byte 5
    /// - col 8 is the first column after the inlay, which is
    ///   ' ' (byte 5; the inlay was anchored at byte 5 so the
    ///   space hasn't been consumed yet)
    /// - col 9 is past the space → byte 6 ('w')
    #[test]
    fn inlay_columns_snap_to_anchor_byte() {
        let line = "hello world";
        let inlays = [(5u32, 3u32)]; // 3-col inlay after "hello"
        // pre-inlay
        assert_eq!(combined_col_to_byte(line, 0, &inlays), 0);
        assert_eq!(combined_col_to_byte(line, 4, &inlays), 4);
        // at the inlay anchor (before splicing it)
        assert_eq!(combined_col_to_byte(line, 5, &inlays), 5);
        // inside the inlay (cols 6, 7) → snap to anchor byte 5
        assert_eq!(combined_col_to_byte(line, 6, &inlays), 5);
        assert_eq!(combined_col_to_byte(line, 7, &inlays), 5);
        // one column past the inlay → still byte 5 (the
        // space hasn't been consumed yet; cursor sits before it)
        assert_eq!(combined_col_to_byte(line, 8, &inlays), 5);
        // col 9 → byte 6 (after consuming the space)
        assert_eq!(combined_col_to_byte(line, 9, &inlays), 6);
    }

    /// Two inlays on the same line: each contributes width.
    /// Layout: `[INLAY-2-cols]abc[INLAY-1-col]de`
    /// Combined columns:
    /// - cols 0-1 = inlay-1 (anchored at byte 0)
    /// - col 2 = 'a' (byte 0; right after leading inlay)
    /// - col 3 = 'b' (byte 1)
    /// - col 4 = 'c' (byte 2)
    /// - col 5 = inlay-2 (anchored at byte 3)
    /// - col 6 = 'd' (byte 3)
    /// - col 7 = 'e' (byte 4)
    ///
    /// Returns reflect cursor-before-byte: col 2 sits at the
    /// start of 'a' → byte 0; col 3 sits at the start of 'b'
    /// → byte 1; etc.
    #[test]
    fn multiple_inlays_compose() {
        let line = "abcde";
        let inlays = [(0u32, 2u32), (3u32, 1u32)];
        // col 0 → byte 0 (inside leading inlay; snaps)
        assert_eq!(combined_col_to_byte(line, 0, &inlays), 0);
        // col 1 → byte 0 (still inside leading inlay)
        assert_eq!(combined_col_to_byte(line, 1, &inlays), 0);
        // col 2 → byte 0 (right after leading inlay, before 'a')
        assert_eq!(combined_col_to_byte(line, 2, &inlays), 0);
        // col 3 → byte 1 (after 'a')
        assert_eq!(combined_col_to_byte(line, 3, &inlays), 1);
        // col 4 → byte 2 (after 'b')
        assert_eq!(combined_col_to_byte(line, 4, &inlays), 2);
        // col 5 → byte 3 (after 'c'; also the inlay-2 anchor)
        assert_eq!(combined_col_to_byte(line, 5, &inlays), 3);
        // col 6 → byte 3 (just past inlay-2; 'd' not consumed yet)
        assert_eq!(combined_col_to_byte(line, 6, &inlays), 3);
        // col 7 → byte 4 (after 'd')
        assert_eq!(combined_col_to_byte(line, 7, &inlays), 4);
    }

    /// Trailing inlay (anchored at EOL): a click in its
    /// columns clamps to EOL.
    #[test]
    fn trailing_inlay_clamps_to_eol() {
        let line = "abc";
        // 2-col inlay at the line's tail.
        let inlays = [(3u32, 2u32)];
        assert_eq!(combined_col_to_byte(line, 0, &inlays), 0);
        assert_eq!(combined_col_to_byte(line, 3, &inlays), 3);
        // col 4 is inside the trailing inlay → clamps to EOL.
        assert_eq!(combined_col_to_byte(line, 4, &inlays), 3);
        assert_eq!(combined_col_to_byte(line, 5, &inlays), 3);
    }
}
