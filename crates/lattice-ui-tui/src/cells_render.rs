//! S3.b (2026-05-26): cell-grid → ratatui span conversion.
//!
//! The TUI consumes [`lattice_cells::CellRow`] published by the
//! cell-builder worker (S2) and emits a `Vec<Span<'static>>`
//! ready for ratatui's text widgets. This module is the
//! substrate→TUI translation layer.
//!
//! ## Why a separate module
//!
//! The legacy render path builds spans from `RowPrepaint.runs`
//! produced by `highlights_worker` — it uses
//! `lattice_syntax::Style` enum tags and resolves them to ratatui
//! styles at paint time via the TUI theme. The cell-grid path
//! has *already* resolved the style: each [`lattice_cells::Cell`]
//! carries its 24-bit fg, bg, and modifier flag bits (S3.a). The
//! converter here just walks cells, groups consecutive cells with
//! matching `(fg, bg, modifier_bits)`, and emits one
//! [`ratatui::text::Span`] per group.
//!
//! ## Coordinate spaces
//!
//! - [`cell_row_to_combined_spans`] returns spans over *combined*
//!   columns — every cell in the row, including inlay-spliced
//!   cells. Use this when the consumer treats inlays as part of
//!   the rendered text (the cell-grid model).
//! - [`cell_row_to_source_spans`] returns spans over *source*
//!   bytes — INLAY-flagged cells are skipped. Drop-in compatible
//!   with the existing TUI body-spans shape; the renderer's
//!   overlay pipeline still positions overlays by source-byte
//!   offset.
//!
//! S3.b is the converter + tests; S3.c will wire it into
//! `draw_buffer` and (if needed) introduce `OverlayState` to
//! retire the existing per-frame overlay walk.

use lattice_cells::{Cell, CellRow};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Convert every cell in `row` to ratatui spans. Inlay-spliced
/// cells are included; the output covers *combined* column space.
///
/// Returns an empty `Vec` for an empty row.
pub fn cell_row_to_combined_spans(row: &CellRow) -> Vec<Span<'static>> {
    cells_to_spans(row.cells.iter())
}

/// Convert only the *source* cells in `row` (cells without
/// [`lattice_cells::cell_flags::INLAY`]) to ratatui spans. Drop-
/// in compatible with the existing TUI body-spans shape — the
/// resulting spans cover source-byte positions one-to-one with
/// the rope line, so overlays positioned by source byte work
/// unchanged.
///
/// Returns an empty `Vec` for an empty row or a row containing
/// only inlay cells.
pub fn cell_row_to_source_spans(row: &CellRow) -> Vec<Span<'static>> {
    cells_to_spans(row.cells.iter().filter(|c| !c.is_inlay()))
}

/// Walk a stream of `&Cell` references, group consecutive cells
/// with the same `(fg, bg, modifier_bits)` into one span, and
/// emit. Internal helper for both public converters above.
///
/// The grouping key is the full [`Style`] derived from the cell
/// (via [`cell_to_style`]). Two cells with the same colours +
/// modifiers but one marked INLAY produce the same style and
/// merge — callers that need provenance-aware grouping should
/// filter upstream (as [`cell_row_to_source_spans`] does).
fn cells_to_spans<'a, I>(cells: I) -> Vec<Span<'static>>
where
    I: Iterator<Item = &'a Cell>,
{
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_text = String::new();
    let mut current_style: Option<Style> = None;

    for cell in cells {
        let style = cell_to_style(cell);
        let ch = char::from_u32(cell.codepoint).unwrap_or('?');
        if current_style == Some(style) {
            current_text.push(ch);
        } else {
            if !current_text.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut current_text),
                    current_style.unwrap_or_default(),
                ));
            }
            current_text.push(ch);
            current_style = Some(style);
        }
    }
    if !current_text.is_empty() {
        spans.push(Span::styled(
            current_text,
            current_style.unwrap_or_default(),
        ));
    }
    spans
}

/// Resolve a single [`Cell`] to a ratatui [`Style`]. Maps:
/// - `cell.fg != 0` → `Color::Rgb(r, g, b)` foreground.
/// - `cell.bg != 0` → `Color::Rgb(r, g, b)` background.
/// - `cell.fg == 0` / `cell.bg == 0` → channel left unset (the
///   renderer uses the pane's default for that channel). This
///   matches the host theme's `Color::Default` semantics.
/// - Modifier flag bits → `Modifier::BOLD` / `ITALIC` /
///   `UNDERLINED` / `DIM` / `REVERSED`.
///
/// Truecolor terminals render the `Rgb` colours exactly; 16-color
/// terminals get ratatui's automatic nearest-named-color
/// downsampling. There's a small visual difference vs. the
/// legacy `host_theme → tui_theme` adapter path in 16-color
/// terminals (the legacy path picks named ANSI colours directly);
/// the cell-grid model is designed for truecolor and accepts
/// this downsampling cost in low-colour-depth terminals.
fn cell_to_style(cell: &Cell) -> Style {
    let mut style = Style::default();
    if cell.fg != 0 {
        style = style.fg(rgb_u32_to_color(cell.fg));
    }
    if cell.bg != 0 {
        style = style.bg(rgb_u32_to_color(cell.bg));
    }
    let mut mods = Modifier::empty();
    if cell.is_bold() {
        mods |= Modifier::BOLD;
    }
    if cell.is_italic() {
        mods |= Modifier::ITALIC;
    }
    if cell.is_underline() {
        mods |= Modifier::UNDERLINED;
    }
    if cell.is_dim() {
        mods |= Modifier::DIM;
    }
    if cell.is_reverse() {
        mods |= Modifier::REVERSED;
    }
    if !mods.is_empty() {
        style = style.add_modifier(mods);
    }
    style
}

/// Convert a packed `0xRRGGBB` `u32` colour to a ratatui
/// `Color::Rgb`. Centralised so the bit layout is one place to
/// update if the cell-grid ever extends to RGBA.
fn rgb_u32_to_color(rgb: u32) -> Color {
    let r = ((rgb >> 16) & 0xff) as u8;
    let g = ((rgb >> 8) & 0xff) as u8;
    let b = (rgb & 0xff) as u8;
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_cells::{cell_flags, Cell, CellRow};

    fn row(cells: Vec<Cell>) -> CellRow {
        CellRow::new(cells, 0, Vec::<lattice_cells::row::InlayOffset>::new())
    }

    /// Empty row → empty Vec. Defensive baseline.
    #[test]
    fn empty_row_yields_no_spans() {
        let r = row(Vec::new());
        assert!(cell_row_to_combined_spans(&r).is_empty());
        assert!(cell_row_to_source_spans(&r).is_empty());
    }

    /// Single cell → one span carrying that cell's codepoint and
    /// theme-resolved fg.
    #[test]
    fn single_cell_yields_one_span() {
        let c = Cell::new(b'x' as u32, 0xcdd6f4, 0, 0);
        let r = row(vec![c]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "x");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
        // No modifiers set.
        assert!(spans[0].style.add_modifier.is_empty());
    }

    /// Adjacent cells with identical `(fg, bg, modifiers)` merge
    /// into one span — the grouping that makes the converter
    /// efficient (fewer ratatui spans = less paint work).
    #[test]
    fn adjacent_same_style_cells_merge() {
        let fg = 0xcba6f7;
        let c1 = Cell::new(b'f' as u32, fg, 0, cell_flags::BOLD);
        let c2 = Cell::new(b'n' as u32, fg, 0, cell_flags::BOLD);
        let r = row(vec![c1, c2]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans.len(), 1, "matching cells must collapse to one span");
        assert_eq!(spans[0].content.as_ref(), "fn");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// Cells with different fg break the span — even when bg and
    /// modifiers match.
    #[test]
    fn different_fg_breaks_span() {
        let a = Cell::new(b'a' as u32, 0xff0000, 0, 0);
        let b = Cell::new(b'b' as u32, 0x00ff00, 0, 0);
        let r = row(vec![a, b]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "a");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(0xff, 0, 0)));
        assert_eq!(spans[1].content.as_ref(), "b");
        assert_eq!(spans[1].style.fg, Some(Color::Rgb(0, 0xff, 0)));
    }

    /// Cells with different modifier bits break the span — even
    /// when colours match. `fn` bold cell + `(` non-bold cell
    /// must emit two spans.
    #[test]
    fn different_modifiers_break_span() {
        let fg = 0xcdd6f4;
        let bold = Cell::new(b'a' as u32, fg, 0, cell_flags::BOLD);
        let plain = Cell::new(b'b' as u32, fg, 0, 0);
        let r = row(vec![bold, plain]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    /// All five modifier flags map to the right ratatui modifier
    /// bits. Captures the full table so future TUI modifier
    /// additions can't quietly drop a flag.
    #[test]
    fn modifier_flags_map_to_ratatui_modifiers() {
        let cases = [
            (cell_flags::BOLD, Modifier::BOLD),
            (cell_flags::ITALIC, Modifier::ITALIC),
            (cell_flags::UNDERLINE, Modifier::UNDERLINED),
            (cell_flags::DIM, Modifier::DIM),
            (cell_flags::REVERSE, Modifier::REVERSED),
        ];
        for (cell_flag, ratatui_mod) in cases {
            let c = Cell::new(b'a' as u32, 0xcdd6f4, 0, cell_flag);
            let r = row(vec![c]);
            let spans = cell_row_to_combined_spans(&r);
            assert!(
                spans[0].style.add_modifier.contains(ratatui_mod),
                "cell flag {cell_flag:#06x} must map to ratatui {ratatui_mod:?}; \
                 got {:?}",
                spans[0].style.add_modifier
            );
        }
    }

    /// Modifier flags compose — bold + italic + underline on one
    /// cell yields one span with all three ratatui modifiers set.
    #[test]
    fn modifier_flags_compose() {
        let mods = cell_flags::BOLD | cell_flags::ITALIC | cell_flags::UNDERLINE;
        let c = Cell::new(b'x' as u32, 0xcdd6f4, 0, mods);
        let r = row(vec![c]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans.len(), 1);
        let s = spans[0].style.add_modifier;
        assert!(s.contains(Modifier::BOLD));
        assert!(s.contains(Modifier::ITALIC));
        assert!(s.contains(Modifier::UNDERLINED));
    }

    /// fg == 0 means "use pane default" — the resulting span has
    /// `style.fg == None`. Matches `host::Color::Default` semantics.
    #[test]
    fn fg_zero_leaves_fg_unset() {
        let c = Cell::new(b'x' as u32, 0, 0, 0);
        let r = row(vec![c]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, None);
        assert_eq!(spans[0].style.bg, None);
    }

    /// `bg != 0` lands as a ratatui background colour.
    #[test]
    fn bg_nonzero_sets_bg() {
        let c = Cell::new(b'x' as u32, 0xcdd6f4, 0x1e1e2e, 0);
        let r = row(vec![c]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(spans[0].style.bg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
    }

    /// `cell_row_to_source_spans` filters out INLAY-flagged cells.
    /// The output covers source-byte positions only — drop-in
    /// compatible with the existing TUI body-spans shape.
    #[test]
    fn source_spans_skip_inlay_cells() {
        let fg = 0xcdd6f4;
        let s1 = Cell::new(b'h' as u32, fg, 0, 0);
        let s2 = Cell::new(b'i' as u32, fg, 0, 0);
        let i1 = Cell::new(b':' as u32, 0x7f7f7f, 0, cell_flags::INLAY);
        let i2 = Cell::new(b' ' as u32, 0x7f7f7f, 0, cell_flags::INLAY);
        let r = row(vec![s1, i1, i2, s2]);

        // Combined includes everything.
        let combined = cell_row_to_combined_spans(&r);
        assert_eq!(
            combined.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "h: i"
        );

        // Source-only spans drop the inlay cells.
        let source = cell_row_to_source_spans(&r);
        assert_eq!(
            source.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "hi"
        );
        // The two source cells share style → one span.
        assert_eq!(source.len(), 1);
    }

    /// Empty filter result yields an empty Vec — covers the case
    /// where a row is entirely inlay-spliced (every cell carries
    /// INLAY). Defensive against panic-on-empty in span emit.
    #[test]
    fn source_spans_empty_when_all_inlay() {
        let i = Cell::new(b'?' as u32, 0x7f7f7f, 0, cell_flags::INLAY);
        let r = row(vec![i, i, i]);
        let source = cell_row_to_source_spans(&r);
        assert!(source.is_empty());
    }

    /// Non-ASCII codepoints round-trip through the converter.
    /// `char::from_u32` should accept any valid scalar; invalid
    /// codepoints fall back to `?`.
    #[test]
    fn non_ascii_codepoints_round_trip() {
        let c1 = Cell::new('é' as u32, 0xcdd6f4, 0, 0);
        let c2 = Cell::new('→' as u32, 0xcdd6f4, 0, 0);
        let r = row(vec![c1, c2]);
        let spans = cell_row_to_combined_spans(&r);
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "é→"
        );
    }

    /// Realistic row: keyword + space + identifier + paren. Each
    /// style transition emits one span. Captures the worker's
    /// typical output shape end-to-end at the converter boundary.
    #[test]
    fn keyword_identifier_paren_row_emits_four_spans() {
        let kw_fg = 0xcba6f7;
        let id_fg = 0xcdd6f4;
        let fn_fg = 0x89b4fa;
        let punct_fg = 0x9399b2;
        // `fn main(`
        let cells = vec![
            // `fn` keyword (bold)
            Cell::new(b'f' as u32, kw_fg, 0, cell_flags::BOLD),
            Cell::new(b'n' as u32, kw_fg, 0, cell_flags::BOLD),
            // space — default fg
            Cell::new(b' ' as u32, id_fg, 0, 0),
            // `main` function name
            Cell::new(b'm' as u32, fn_fg, 0, 0),
            Cell::new(b'a' as u32, fn_fg, 0, 0),
            Cell::new(b'i' as u32, fn_fg, 0, 0),
            Cell::new(b'n' as u32, fn_fg, 0, 0),
            // `(` punctuation
            Cell::new(b'(' as u32, punct_fg, 0, 0),
        ];
        let r = row(cells);
        let spans = cell_row_to_combined_spans(&r);
        // 4 spans: `fn` (bold) / ` ` (default) / `main` (function fg)
        // / `(` (punct fg).
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content.as_ref(), "fn");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content.as_ref(), " ");
        assert_eq!(spans[2].content.as_ref(), "main");
        assert!(!spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[3].content.as_ref(), "(");
    }

    // ---- S3.c.1 — whitespace decoration on cell-derived bodies ----
    //
    // Validates that `crate::render::apply_whitespace_decoration`
    // walks cell-derived spans correctly. The decoration function
    // consumes spans + line text opaquely and walks each char by
    // utf-8 byte offset; cell-derived source spans cover the same
    // source-byte positions one-to-one with `line_text`, so the
    // classifier should fire at identical positions to the
    // legacy RowPrepaint path.

    use crate::render::{apply_whitespace_decoration, WhitespaceDecoration};
    use ratatui::style::Style as TuiStyle;

    fn ws_deco_all_off() -> WhitespaceDecoration {
        WhitespaceDecoration {
            tab: None,
            trailing: None,
            leading: None,
            space: None,
            eol: None,
            style_normal: TuiStyle::default(),
            style_trailing: TuiStyle::default(),
        }
    }

    fn ws_deco(
        tab: Option<char>,
        trailing: Option<char>,
        leading: Option<char>,
        space: Option<char>,
        eol: Option<char>,
    ) -> WhitespaceDecoration {
        WhitespaceDecoration {
            tab,
            trailing,
            leading,
            space,
            eol,
            style_normal: TuiStyle::default(),
            style_trailing: TuiStyle::default(),
        }
    }

    /// Helper: concatenate every span's text into one `String` so
    /// tests can assert on the visible output without caring how
    /// the spans were split.
    fn collect_text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Mid-line space cells get substituted by the `·` space glyph.
    /// Cell-derived path produces source spans containing the
    /// literal space; the classifier walks bytes and replaces.
    #[test]
    fn s3c1_mid_line_space_substituted() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, 0),
            Cell::new(b' ' as u32, fg, 0, 0),
            Cell::new(b'b' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let body = cell_row_to_source_spans(&r);
        let line_text = "a b";
        let d = ws_deco(None, None, None, Some('·'), None);
        let out = apply_whitespace_decoration(body, line_text, &d);
        assert_eq!(collect_text(&out), "a·b");
    }

    /// Leading-whitespace classification fires for spaces before
    /// the first non-whitespace byte. Cell-derived spans don't
    /// confuse the position tracking — `pos` advances by utf-8
    /// byte length per char regardless of span boundaries.
    #[test]
    fn s3c1_leading_whitespace_substituted() {
        let fg = 0xcdd6f4;
        // `  hi` — two leading spaces.
        let cells = vec![
            Cell::new(b' ' as u32, fg, 0, 0),
            Cell::new(b' ' as u32, fg, 0, 0),
            Cell::new(b'h' as u32, fg, 0, 0),
            Cell::new(b'i' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let body = cell_row_to_source_spans(&r);
        let line_text = "  hi";
        let d = ws_deco(None, None, Some('›'), None, None);
        let out = apply_whitespace_decoration(body, line_text, &d);
        assert_eq!(collect_text(&out), "››hi");
    }

    /// Trailing-whitespace classification fires for spaces after
    /// the last non-whitespace byte. Cells-derived spans must
    /// carry those trailing chars so the classifier sees them.
    #[test]
    fn s3c1_trailing_whitespace_substituted() {
        let fg = 0xcdd6f4;
        // `hi  ` — two trailing spaces.
        let cells = vec![
            Cell::new(b'h' as u32, fg, 0, 0),
            Cell::new(b'i' as u32, fg, 0, 0),
            Cell::new(b' ' as u32, fg, 0, 0),
            Cell::new(b' ' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let body = cell_row_to_source_spans(&r);
        let line_text = "hi  ";
        let d = ws_deco(None, Some('▷'), None, None, None);
        let out = apply_whitespace_decoration(body, line_text, &d);
        assert_eq!(collect_text(&out), "hi▷▷");
    }

    /// Tab cell substituted by the tab glyph. Cells carry the
    /// `\t` codepoint verbatim — the converter preserves it; the
    /// classifier substitutes.
    #[test]
    fn s3c1_tab_cell_substituted() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'x' as u32, fg, 0, 0),
            Cell::new(b'\t' as u32, fg, 0, 0),
            Cell::new(b'y' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let body = cell_row_to_source_spans(&r);
        let line_text = "x\ty";
        let d = ws_deco(Some('→'), None, None, None, None);
        let out = apply_whitespace_decoration(body, line_text, &d);
        assert_eq!(collect_text(&out), "x→y");
    }

    /// EOL marker appends after every cell — including for cells-
    /// derived bodies. Captures the contract that the EOL glyph
    /// emit is independent of the input spans' provenance.
    #[test]
    fn s3c1_eol_marker_appends_after_cells() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'h' as u32, fg, 0, 0),
            Cell::new(b'i' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let body = cell_row_to_source_spans(&r);
        let line_text = "hi";
        let d = ws_deco(None, None, None, None, Some('¶'));
        let out = apply_whitespace_decoration(body, line_text, &d);
        assert_eq!(collect_text(&out), "hi¶");
    }

    /// All-off whitespace decoration is a no-op: the cell-derived
    /// body passes through unchanged. Defensive against any
    /// future shortcut that might mutate input when no glyphs are
    /// configured.
    #[test]
    fn s3c1_no_op_decoration_preserves_cell_spans() {
        let fg = 0xcdd6f4;
        let cells = vec![
            Cell::new(b'a' as u32, fg, 0, 0),
            Cell::new(b' ' as u32, fg, 0, 0),
            Cell::new(b'b' as u32, fg, 0, 0),
        ];
        let r = row(cells);
        let body_before = cell_row_to_source_spans(&r);
        let line_text = "a b";
        let d = ws_deco_all_off();
        let body_after =
            apply_whitespace_decoration(body_before.clone(), line_text, &d);
        // Same text, same span count, same styles.
        assert_eq!(body_after.len(), body_before.len());
        for (a, b) in body_after.iter().zip(body_before.iter()) {
            assert_eq!(a.content.as_ref(), b.content.as_ref());
            assert_eq!(a.style, b.style);
        }
    }

    /// Whitespace decoration walks across span boundaries.
    /// Construct a cell-derived body where the space sits between
    /// two different-fg cells so it lands on a span boundary;
    /// the classifier must still fire at the correct byte
    /// position.
    #[test]
    fn s3c1_substitution_across_span_boundary() {
        let fg_a = 0xff0000;
        let fg_b = 0x00ff00;
        let cells = vec![
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b' ' as u32, fg_a, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
        ];
        let r = row(cells);
        // First two cells share fg_a → one span; the third cell
        // breaks to fg_b → second span. Verify boundary.
        let body = cell_row_to_source_spans(&r);
        assert_eq!(body.len(), 2);
        let line_text = "a b";
        let d = ws_deco(None, None, None, Some('·'), None);
        let out = apply_whitespace_decoration(body, line_text, &d);
        assert_eq!(collect_text(&out), "a·b");
    }
}
