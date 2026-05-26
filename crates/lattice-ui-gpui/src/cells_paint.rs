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
//! ## Modifier coverage (S4.0)
//!
//! Cells carry five modifier bits (S3.a): `BOLD`, `ITALIC`,
//! `UNDERLINE`, `DIM`, `REVERSE`. This converter currently
//! propagates only the fg colour — modifier bits are dropped.
//! That matches the visual behaviour of the legacy
//! `build_line_with_inlays` path (which read `syntax_color(...)`
//! for fg only and reused the same font for every cell). S4.0
//! preserves visual parity with the legacy path; richer modifier
//! rendering (font-weight variants for BOLD / ITALIC, fg
//! blending for DIM, fg↔bg swap for REVERSE, underline geometry
//! for UNDERLINE) is a follow-up under S4.3 / S4.final once the
//! cell-grid path is the canonical source.
//!
//! S4.0 is the converter + tests; S4.1 wires it into
//! `EditorElement::prepaint`'s body branch with a fallback to
//! the legacy `build_line_with_inlays` path during validation.

use gpui::{Font, TextRun};
use lattice_cells::CellRow;

use crate::editor_element::make_run_with_color;

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
    let mut current_fg: u32 = 0;
    let mut current_len: usize = 0;
    let mut started = false;

    for cell in row.cells.iter() {
        let ch = char::from_u32(cell.codepoint).unwrap_or('?');
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        let ch_len = encoded.len();
        if !started {
            current_fg = cell.fg;
            current_len = ch_len;
            started = true;
        } else if cell.fg == current_fg {
            current_len += ch_len;
        } else {
            runs.push(make_run_with_color(current_fg, current_len, font));
            current_fg = cell.fg;
            current_len = ch_len;
        }
        combined.push_str(encoded);
    }
    if started && current_len > 0 {
        runs.push(make_run_with_color(current_fg, current_len, font));
    }

    let inlay_offsets: Vec<(u32, u32)> = row.inlay_offsets.iter().copied().collect();
    (combined, runs, inlay_offsets)
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
    /// runs.
    #[test]
    fn keyword_identifier_paren_row_yields_four_runs() {
        let kw_fg = 0xcba6f7;
        let id_fg = 0xcdd6f4;
        let fn_fg = 0x89b4fa;
        let punct_fg = 0x9399b2;
        let cells = vec![
            Cell::new(b'f' as u32, kw_fg, 0, lattice_cells::cell_flags::BOLD),
            Cell::new(b'n' as u32, kw_fg, 0, lattice_cells::cell_flags::BOLD),
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
}
