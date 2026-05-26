//! S4.0 (2026-05-26): cell-grid → GPUI TextRun conversion.
//!
//! `EditorElement::prepaint` shapes the visible viewport via
//! `WindowTextSystem::shape_line(combined_text, &runs)` per line.
//! Pre-cell-grid, those `(combined_text, Vec<TextRun>)` triples
//! came from `build_line_with_inlays` walking a syntax-span set
//! plus inlay-hint metadata.
//!
//! This module is the substrate→GPUI translation layer: given a
//! [`lattice_cells::CellRow`] published by the cell-builder
//! worker (S2), produce the same `(combined_text, Vec<TextRun>,
//! inlay_offsets)` triple. The cells already carry every input
//! the legacy walk reconstructed:
//! - per-cell codepoint → combined text (verbatim).
//! - per-cell fg → TextRun color (cells with the same fg merge
//!   into one run, matching `build_line_with_inlays`'s collapse).
//! - `inlay_offsets` field → returned directly (already
//!   `(orig_byte, char_width)` per S2.3.b).
//!
//! ## Modifier coverage (S4.2)
//!
//! Cells carry five modifier bits (S3.a): `BOLD`, `ITALIC`,
//! `UNDERLINE`, `DIM`, `REVERSE`. S4.2 propagates all of them
//! into the [`TextRun`] so the GPUI path renders the same
//! styling the TUI converter delivers:
//!
//! - `BOLD` → `font.weight = FontWeight::BOLD` (700).
//! - `ITALIC` → `font.style = FontStyle::Italic`.
//! - `UNDERLINE` → `underline = Some(UnderlineStyle { … })`
//!   with default thickness + theme fg + flat (non-wavy)
//!   geometry. Diagnostic squigglies stay as overlay quads
//!   computed in `prepaint`; the underline field here is for
//!   the syntax-style "this token is underlined" decoration.
//! - `DIM` → fg (and bg, when present) RGB channels multiplied
//!   by `0.6` before packing back into the run colour. Matches
//!   the visual feel of ratatui's DIM modifier in truecolor
//!   terminals.
//! - `REVERSE` → swap `cell.fg` ↔ `cell.bg` before any other
//!   processing. When `cell.bg == 0` (transparent), the swap
//!   leaves `fg = 0` (renderer default) and `bg = cell.fg` —
//!   documented limitation, matches the conventional reverse
//!   meaning ("paint the source fg as background, let the
//!   renderer pick the text colour") without needing access to
//!   the theme bg here.
//!
//! Cell background colour (`cell.bg`) also passes through as
//! `TextRun.background_color`. Together with the grouping
//! change below, the converter now produces the same set of
//! distinct runs the TUI converter produces for
//! `cell_row_to_combined_spans` — one run per consecutive
//! cells sharing `(fg, bg, style_bits)`.
//!
//! `style_bits` includes only `BOLD | ITALIC | UNDERLINE | DIM
//! | REVERSE`. `INLAY` and `WS_MARKER` flags do not influence
//! grouping: an INLAY cell with the same visual style as an
//! adjacent syntax cell merges into the same run (the inlay's
//! position is recorded separately on
//! [`CellRow::inlay_offsets`]).
//!
//! S4.0 was the converter + tests; S4.1 wired it into
//! `EditorElement::prepaint`'s body branch with a fallback to
//! the prepaint and legacy paths; S4.2 (this slice) closes the
//! visual-parity gap with the TUI cells path.

use gpui::{Font, FontStyle, FontWeight, TextRun, UnderlineStyle, px, rgb};
use lattice_cells::{Cell, CellRow, cell_flags};

/// Bits in [`Cell::flags`] that drive run grouping + styling.
/// `INLAY` and `WS_MARKER` are intentionally excluded — they
/// are provenance / classification flags, not visual style, so
/// they should not break runs.
const STYLE_FLAGS_MASK: u16 = cell_flags::BOLD
    | cell_flags::ITALIC
    | cell_flags::UNDERLINE
    | cell_flags::DIM
    | cell_flags::REVERSE;

/// Multiplier applied to each RGB channel of fg + bg when the
/// `DIM` flag is set. `0.6` matches the perceptual feel of
/// ratatui's `Modifier::DIM` in truecolor terminals.
const DIM_FACTOR: f32 = 0.6;

/// Convert a [`CellRow`] into the
/// `(combined_text, Vec<TextRun>, inlay_offsets)` triple that
/// `EditorElement::prepaint` feeds into
/// `WindowTextSystem::shape_line`.
///
/// Returns:
/// - `combined_text`: every cell's codepoint, in order, including
///   inlay-spliced cells. This is exactly the `combined` text
///   the cell-builder produced; passing it through `shape_line`
///   yields the same layout the legacy path produced.
/// - `runs`: one [`TextRun`] per consecutive group of cells with
///   matching fg. Adjacent same-fg cells (including
///   syntax-resolved + inlay cells if they happen to share a
///   colour) merge.
/// - `inlay_offsets`: `(orig_byte, char_width)` pairs from
///   [`CellRow::inlay_offsets`] verbatim. The cell-builder
///   maintains the sorted-by-orig-byte invariant; callers can
///   index into this without re-sorting.
///
/// `font` is cloned into each [`TextRun`]; callers should pass
/// the same font they would feed to `build_line_with_inlays`.
pub fn cell_row_to_text_runs(
    row: &CellRow,
    font: &Font,
) -> (String, Vec<TextRun>, Vec<(u32, u32)>) {
    let mut combined = String::with_capacity(row.cells.len());
    let mut runs: Vec<TextRun> = Vec::new();
    // (sample cell carrying the style key, accumulated utf-8
    // byte length for the in-progress run). The sample's flags
    // / fg / bg supply every input `cell_to_text_run` needs to
    // build the TextRun on flush.
    let mut current: Option<(Cell, usize)> = None;

    for cell in row.cells.iter() {
        let ch = char::from_u32(cell.codepoint).unwrap_or('?');
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        let ch_len = encoded.len();

        match &mut current {
            Some((sample, len)) if style_key(sample) == style_key(cell) => {
                *len += ch_len;
            }
            _ => {
                if let Some((sample, len)) = current.take() {
                    runs.push(cell_to_text_run(&sample, len, font));
                }
                current = Some((*cell, ch_len));
            }
        }
        combined.push_str(encoded);
    }
    if let Some((sample, len)) = current {
        runs.push(cell_to_text_run(&sample, len, font));
    }

    let inlay_offsets: Vec<(u32, u32)> = row.inlay_offsets.iter().copied().collect();
    (combined, runs, inlay_offsets)
}

/// Run-grouping key. Consecutive cells with the same key merge
/// into one [`TextRun`]; a change in any component flushes the
/// in-progress run. See [`STYLE_FLAGS_MASK`] for which flag bits
/// are considered style-significant.
fn style_key(cell: &Cell) -> (u32, u32, u16) {
    (cell.fg, cell.bg, cell.flags & STYLE_FLAGS_MASK)
}

/// Build a fully-styled [`TextRun`] for `len` utf-8 bytes worth
/// of `cell`'s visual style. `font_base` supplies family,
/// features, fallbacks, and the default size; `cell_to_text_run`
/// overrides `weight` / `style` per modifier bits.
fn cell_to_text_run(cell: &Cell, len: usize, font_base: &Font) -> TextRun {
    let mut fg = cell.fg;
    let mut bg = cell.bg;
    if cell.is_reverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.is_dim() {
        fg = dim_channel(fg);
        if bg != 0 {
            bg = dim_channel(bg);
        }
    }

    let mut font = font_base.clone();
    if cell.is_bold() {
        font.weight = FontWeight::BOLD;
    }
    if cell.is_italic() {
        font.style = FontStyle::Italic;
    }

    let underline = if cell.is_underline() {
        Some(UnderlineStyle {
            thickness: px(1.0),
            color: None,
            wavy: false,
        })
    } else {
        None
    };
    let background_color = if bg != 0 { Some(rgb(bg).into()) } else { None };

    TextRun {
        len,
        font,
        color: rgb(fg).into(),
        background_color,
        underline,
        strikethrough: None,
    }
}

/// Multiply each RGB channel by [`DIM_FACTOR`]. Saturates to 0
/// on underflow (the `f32 → u32` `as` cast already clamps
/// negatives to 0 and overflow is impossible because every
/// channel is at most 255 and the multiplier is < 1).
fn dim_channel(packed: u32) -> u32 {
    let r = ((packed >> 16) & 0xff) as f32;
    let g = ((packed >> 8) & 0xff) as f32;
    let b = (packed & 0xff) as f32;
    let r = (r * DIM_FACTOR) as u32;
    let g = (g * DIM_FACTOR) as u32;
    let b = (b * DIM_FACTOR) as u32;
    (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use gpui::font;
    use lattice_cells::{Cell, CellRow};

    fn row(cells: Vec<Cell>) -> CellRow {
        CellRow::new(cells, 0, Vec::<lattice_cells::row::InlayOffset>::new())
    }

    fn row_with_inlays(cells: Vec<Cell>, inlays: Vec<(u32, u32)>) -> CellRow {
        CellRow::new(cells, 0, inlays)
    }

    /// Empty row → empty triple. Defensive baseline.
    #[test]
    fn empty_row_yields_empty_triple() {
        let r = row(Vec::new());
        let (text, runs, offsets) = cell_row_to_text_runs(&r, &font("monospace"));
        assert!(text.is_empty());
        assert!(runs.is_empty());
        assert!(offsets.is_empty());
    }

    /// Single cell → one run of length 1, combined text = that
    /// codepoint.
    #[test]
    fn single_cell_yields_one_run() {
        let c = Cell::new(b'x' as u32, 0xcdd6f4, 0, 0);
        let r = row(vec![c]);
        let (text, runs, offsets) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "x");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 1);
        assert!(offsets.is_empty());
    }

    /// Adjacent same-fg cells merge into one run — the collapse
    /// invariant that keeps `shape_line` work proportional to
    /// styled span count, not character count.
    #[test]
    fn adjacent_same_fg_cells_merge_into_one_run() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, 0),
            Cell::new(b'b' as u32, fg, 0, 0),
            Cell::new(b'c' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "abc");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 3);
    }

    /// Cells with different fg break the run.
    #[test]
    fn different_fg_breaks_runs() {
        let cells = vec![
            Cell::new(b'a' as u32, 0xff0000, 0, 0),
            Cell::new(b'b' as u32, 0x00ff00, 0, 0),
            Cell::new(b'c' as u32, 0x0000ff, 0, 0),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "abc");
        assert_eq!(runs.len(), 3);
        for run in &runs {
            assert_eq!(run.len, 1);
        }
    }

    /// `inlay_offsets` from the row pass through verbatim. The
    /// cell-builder maintains the sorted-by-orig-byte invariant;
    /// callers can index into the result directly.
    #[test]
    fn inlay_offsets_pass_through() {
        let cells = vec![
            Cell::new(b'a' as u32, 0xcdd6f4, 0, 0),
            Cell::new(b':' as u32, 0x7f7f7f, 0, lattice_cells::cell_flags::INLAY),
            Cell::new(b'b' as u32, 0xcdd6f4, 0, 0),
        ];
        let inlays = vec![(1u32, 1u32)];
        let r = row_with_inlays(cells, inlays.clone());
        let (_, _, offsets) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(offsets, inlays);
    }

    /// Inlay cells with their own fg (DarkGray per S2.3.b) form
    /// their own run, separated from the surrounding source-fg
    /// cells.
    #[test]
    fn inlay_cells_form_separate_run() {
        let src_fg = 0xcdd6f4;
        let inlay_fg = 0x7f7f7f;
        let cells = vec![
            Cell::new(b'a' as u32, src_fg, 0, 0),
            Cell::new(b':' as u32, inlay_fg, 0, lattice_cells::cell_flags::INLAY),
            Cell::new(b' ' as u32, inlay_fg, 0, lattice_cells::cell_flags::INLAY),
            Cell::new(b'b' as u32, src_fg, 0, 0),
        ];
        let r = row_with_inlays(cells, vec![(1u32, 2u32)]);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "a: b");
        // Three runs: "a" (src) / ": " (inlay) / "b" (src).
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].len, 1);
        assert_eq!(runs[1].len, 2);
        assert_eq!(runs[2].len, 1);
    }

    /// Realistic row: `fn main(` with keyword-bold-purple, default
    /// fg space, function-yellow `main`, punct-grey `(`. Four
    /// runs; S4.2 propagates the BOLD bit into the run's font
    /// weight.
    #[test]
    fn keyword_identifier_paren_row_yields_four_runs() {
        let kw_fg = 0xcba6f7;
        let id_fg = 0xcdd6f4;
        let fn_fg = 0x89b4fa;
        let punct_fg = 0x9399b2;
        let cells = vec![
            Cell::new(b'f' as u32, kw_fg, 0, cell_flags::BOLD),
            Cell::new(b'n' as u32, kw_fg, 0, cell_flags::BOLD),
            Cell::new(b' ' as u32, id_fg, 0, 0),
            Cell::new(b'm' as u32, fn_fg, 0, 0),
            Cell::new(b'a' as u32, fn_fg, 0, 0),
            Cell::new(b'i' as u32, fn_fg, 0, 0),
            Cell::new(b'n' as u32, fn_fg, 0, 0),
            Cell::new(b'(' as u32, punct_fg, 0, 0),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "fn main(");
        // 4 runs: `fn` (2 bytes) / ` ` (1) / `main` (4) / `(` (1).
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[1].len, 1);
        assert_eq!(runs[2].len, 4);
        assert_eq!(runs[3].len, 1);
        // S4.2: keyword run carries bold weight; the rest stay
        // at the default normal weight.
        assert_eq!(runs[0].font.weight, FontWeight::BOLD);
        assert_eq!(runs[1].font.weight, FontWeight::NORMAL);
        assert_eq!(runs[2].font.weight, FontWeight::NORMAL);
        assert_eq!(runs[3].font.weight, FontWeight::NORMAL);
    }

    /// Non-ASCII codepoints round-trip: each char's utf-8 byte
    /// length contributes to its run length, not 1-byte-per-char.
    #[test]
    fn non_ascii_codepoints_run_length_is_utf8_bytes() {
        let fg = 0xcdd6f4;
        let cells = vec![
            // 'é' = 2 utf-8 bytes; '→' = 3 utf-8 bytes.
            Cell::new('é' as u32, fg, 0, 0),
            Cell::new('→' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "é→");
        assert_eq!(runs.len(), 1);
        // 2 + 3 = 5 utf-8 bytes total — TextRun.len is byte-based.
        assert_eq!(runs[0].len, 5);
    }

    // -------- S4.2 modifier propagation --------

    /// BOLD bit → `font.weight = FontWeight::BOLD`. Cells with
    /// BOLD set merge among themselves; cells without break the
    /// run even when fg matches.
    #[test]
    fn bold_bit_sets_font_weight_and_breaks_runs() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, cell_flags::BOLD),
            Cell::new(b'b' as u32, fg, 0, cell_flags::BOLD),
            Cell::new(b'c' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "abc");
        // Two runs: BOLD `ab` / non-bold `c`.
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[0].font.weight, FontWeight::BOLD);
        assert_eq!(runs[1].len, 1);
        assert_eq!(runs[1].font.weight, FontWeight::NORMAL);
    }

    /// ITALIC bit → `font.style = FontStyle::Italic`.
    #[test]
    fn italic_bit_sets_font_style() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, cell_flags::ITALIC),
            Cell::new(b'b' as u32, fg, 0, cell_flags::ITALIC),
            Cell::new(b'c' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].font.style, FontStyle::Italic);
        assert_eq!(runs[1].font.style, FontStyle::Normal);
    }

    /// UNDERLINE bit → `underline = Some(UnderlineStyle { … })`
    /// with flat (non-wavy) geometry and default colour. Other
    /// cells get `None`.
    #[test]
    fn underline_bit_emits_flat_underline_style() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, cell_flags::UNDERLINE),
            Cell::new(b'b' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(runs.len(), 2);
        let underline = runs[0].underline.as_ref().expect("UNDERLINE cell run");
        assert!(!underline.wavy, "syntax-style underline is flat");
        assert!(runs[1].underline.is_none());
    }

    /// `cell.bg != 0` → `TextRun.background_color = Some(rgb(bg))`.
    /// Adjacent cells with matching fg + flags but different bg
    /// break the run (matches the TUI grouping key).
    #[test]
    fn bg_passes_through_and_breaks_runs() {
        let fg = 0xcdd6f4;
        let bg_a = 0x313244;
        let cells = vec![
            Cell::new(b'a' as u32, fg, bg_a, 0),
            Cell::new(b'b' as u32, fg, bg_a, 0),
            Cell::new(b'c' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len, 2);
        assert!(runs[0].background_color.is_some());
        assert!(runs[1].background_color.is_none());
    }

    /// DIM bit → fg (and bg, when present) RGB channels
    /// multiplied by `DIM_FACTOR` (0.6). The output run's
    /// `color` differs from a non-DIM cell with the same fg.
    #[test]
    fn dim_bit_attenuates_fg() {
        let fg = 0xff0000; // pure red, easy to eyeball
        let cells = vec![Cell::new(b'a' as u32, fg, 0, cell_flags::DIM)];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        let dim_run = &runs[0];

        let cells2 = vec![Cell::new(b'a' as u32, fg, 0, 0)];
        let r2 = row(cells2);
        let (_, runs2, _) = cell_row_to_text_runs(&r2, &font("monospace"));
        let bright_run = &runs2[0];

        // DIM lowers the run colour. The Hsla wrapping makes
        // direct byte comparison fragile, so just assert the
        // two are not equal (the contract is "DIM produces a
        // different colour", not a specific Hsla constant).
        assert_ne!(dim_run.color, bright_run.color);
    }

    /// DIM channel arithmetic — `dim_channel(0xff_ff_ff)` clamps
    /// each channel to `floor(255 * 0.6) = 153 = 0x99`. Locks
    /// the formula so future tweaks to `DIM_FACTOR` show up here.
    #[test]
    fn dim_channel_attenuation_table() {
        assert_eq!(dim_channel(0x000000), 0x000000);
        assert_eq!(dim_channel(0xffffff), 0x999999);
        assert_eq!(dim_channel(0x808080), (0x80 as f32 * 0.6) as u32 * 0x010101);
    }

    /// REVERSE bit → `cell.fg` and `cell.bg` swap before colour
    /// resolution. When `cell.bg != 0`, the output run paints
    /// the source-fg as background and the source-bg as
    /// foreground.
    #[test]
    fn reverse_bit_swaps_fg_and_bg() {
        let fg = 0xcdd6f4;
        let bg = 0x313244;
        let cells = vec![Cell::new(b'a' as u32, fg, bg, cell_flags::REVERSE)];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        let run = &runs[0];

        // Build the colour values the same way the converter
        // does, then compare. The post-swap fg should equal
        // what `rgb(bg)` produces.
        let expected_fg: gpui::Hsla = rgb(bg).into();
        let expected_bg: gpui::Hsla = rgb(fg).into();
        assert_eq!(run.color, expected_fg);
        assert_eq!(run.background_color, Some(expected_bg));
    }

    /// REVERSE with `cell.bg == 0`. Documented limitation: the
    /// swap leaves `fg = 0` (renderer default) and
    /// `bg = cell.fg`. This test pins that behaviour so future
    /// changes are deliberate.
    #[test]
    fn reverse_with_transparent_bg_promotes_fg_to_bg() {
        let fg = 0xcdd6f4;
        let cells = vec![Cell::new(b'a' as u32, fg, 0, cell_flags::REVERSE)];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        let run = &runs[0];

        let expected_fg: gpui::Hsla = rgb(0).into();
        let expected_bg: gpui::Hsla = rgb(fg).into();
        assert_eq!(run.color, expected_fg);
        assert_eq!(run.background_color, Some(expected_bg));
    }

    /// Composition: BOLD + ITALIC + UNDERLINE all apply
    /// simultaneously. The test exercises the modifier
    /// interactions the TUI converter's
    /// `modifier_flags_compose_independently` test already
    /// covers on the ratatui side.
    #[test]
    fn bold_italic_underline_compose() {
        let fg = 0xcdd6f4;
        let cells = vec![Cell::new(
            b'x' as u32,
            fg,
            0,
            cell_flags::BOLD | cell_flags::ITALIC | cell_flags::UNDERLINE,
        )];
        let r = row(cells);
        let (_, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.font.weight, FontWeight::BOLD);
        assert_eq!(run.font.style, FontStyle::Italic);
        assert!(run.underline.is_some());
    }

    /// INLAY and WS_MARKER are excluded from the style mask, so
    /// two cells with the same fg / bg / style-bits but
    /// different INLAY / WS_MARKER flags still merge. Locks the
    /// `STYLE_FLAGS_MASK` contract: provenance bits don't break
    /// runs, only visual-style bits do.
    #[test]
    fn inlay_and_ws_marker_do_not_break_runs() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, 0),
            Cell::new(b'b' as u32, fg, 0, cell_flags::INLAY),
            Cell::new(b'c' as u32, fg, 0, cell_flags::WS_MARKER),
            Cell::new(b'd' as u32, fg, 0, cell_flags::INLAY | cell_flags::WS_MARKER),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "abcd");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 4);
    }
}
