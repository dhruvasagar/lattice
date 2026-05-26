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

    // ---- S3.c.2 — semantic-tokens overlay on cell-derived bodies ----
    //
    // `apply_semantic_token_overlay(spans, overlay_start,
    // overlay_end, fg, modifiers)` is the LSP semantic-tokens
    // pass. It walks spans by byte position; the portion of
    // each span intersecting `[overlay_start, overlay_end)`
    // gets fg replaced and the supplied modifiers OR-ed in.
    // bg, underline, reverse from earlier passes are preserved.
    //
    // For cell-derived bodies, the invariant is that source
    // spans cover source-byte positions one-to-one with
    // `line_text`, so the overlay's byte walk fires at the
    // correct positions regardless of how cells were grouped.

    use crate::render::apply_semantic_token_overlay;

    /// Helper: build a uniform-fg body covering one short line.
    fn flat_body(text: &str, fg: u32) -> Vec<Span<'static>> {
        let cells: Vec<Cell> = text
            .bytes()
            .map(|b| Cell::new(b as u32, fg, 0, 0))
            .collect();
        cell_row_to_source_spans(&row(cells))
    }

    /// Mid-row overlay: covers bytes [2, 6) of an 8-byte line.
    /// Result: three spans — pre (unchanged) / mid (new fg +
    /// modifiers) / post (unchanged).
    #[test]
    fn s3c2_overlay_splits_span_when_partial() {
        let body = flat_body("abcdefgh", 0xcdd6f4);
        let overlay_fg = Color::Rgb(0xff, 0x00, 0x00);
        let overlay_mods = Modifier::ITALIC;
        let out = apply_semantic_token_overlay(
            body,
            2,
            6,
            overlay_fg,
            overlay_mods,
        );
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_ref(), "ab");
        assert_eq!(out[1].content.as_ref(), "cdef");
        assert_eq!(out[2].content.as_ref(), "gh");
        // Middle span has the overlay's fg + modifier set.
        assert_eq!(out[1].style.fg, Some(overlay_fg));
        assert!(out[1].style.add_modifier.contains(Modifier::ITALIC));
        // Outer spans keep the cell-derived fg.
        assert_eq!(out[0].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
        assert_eq!(out[2].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }

    /// Overlay covering the entire body: fg replaced everywhere;
    /// no pre/post slice needed.
    #[test]
    fn s3c2_overlay_full_coverage() {
        let body = flat_body("hi", 0xcdd6f4);
        let overlay_fg = Color::Rgb(0xff, 0xa5, 0x00);
        let out =
            apply_semantic_token_overlay(body, 0, 2, overlay_fg, Modifier::empty());
        // Exactly the original cells' span(s) with new fg.
        let combined: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(combined, "hi");
        for s in &out {
            assert_eq!(s.style.fg, Some(overlay_fg));
        }
    }

    /// Overlay outside the body's byte range (start past EOL):
    /// no-op pass-through, spans preserved.
    #[test]
    fn s3c2_overlay_outside_range_is_noop() {
        let body = flat_body("abc", 0xcdd6f4);
        let pre_text: String = body.iter().map(|s| s.content.as_ref()).collect();
        let pre_styles: Vec<_> = body.iter().map(|s| s.style).collect();
        let out =
            apply_semantic_token_overlay(body, 10, 20, Color::Red, Modifier::ITALIC);
        let post_text: String = out.iter().map(|s| s.content.as_ref()).collect();
        let post_styles: Vec<_> = out.iter().map(|s| s.style).collect();
        assert_eq!(post_text, pre_text);
        assert_eq!(post_styles, pre_styles);
    }

    /// Overlay preserves the cell's existing modifiers (bold from
    /// syntax style) and ORs in the overlay's modifier (italic
    /// from semantic). Captures the merge contract.
    #[test]
    fn s3c2_overlay_preserves_existing_modifiers() {
        // Body cell carries BOLD from syntax style.
        let fg = 0xcba6f7;
        let cells = vec![
            Cell::new(b'k' as u32, fg, 0, cell_flags::BOLD),
            Cell::new(b'w' as u32, fg, 0, cell_flags::BOLD),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        // Overlay adds ITALIC.
        let out = apply_semantic_token_overlay(
            body,
            0,
            2,
            Color::Cyan,
            Modifier::ITALIC,
        );
        // One span (full coverage, same style); both modifiers
        // present.
        for s in &out {
            assert!(s.style.add_modifier.contains(Modifier::BOLD));
            assert!(s.style.add_modifier.contains(Modifier::ITALIC));
        }
    }

    /// Overlay replaces fg only — bg from an earlier pass stays
    /// untouched. Construct a cell with non-zero bg to seed it.
    #[test]
    fn s3c2_overlay_replaces_fg_keeps_bg() {
        let cells = vec![
            Cell::new(b'x' as u32, 0xcdd6f4, 0x1e1e2e, 0),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        // Sanity: cell-derived span has bg set.
        assert_eq!(body[0].style.bg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        let out = apply_semantic_token_overlay(
            body,
            0,
            1,
            Color::Magenta,
            Modifier::empty(),
        );
        // fg replaced, bg preserved.
        assert_eq!(out[0].style.fg, Some(Color::Magenta));
        assert_eq!(out[0].style.bg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
    }

    // ---- S3.c.3 — bg-layer overlays on cell-derived bodies ----
    //
    // `apply_match_overlay` is the bg-layer engine for visual,
    // hlsearch, current_match, substitute, and doc-highlight
    // overlays. Unlike the semantic-tokens pass, it *replaces*
    // the entire `Style` for the overlap region (the caller
    // chooses fg + bg + modifiers as one bundle).
    //
    // `apply_underline_overlay` is the diagnostics-underline
    // engine. It ADDs `Modifier::UNDERLINED` to the overlap
    // region's existing style; fg / bg from earlier passes stay
    // intact. The `severity_color` parameter is intentionally
    // unused at paint time — see the upstream doc comment for
    // terminal-compatibility reasons.

    use crate::render::{apply_match_overlay, apply_underline_overlay};

    /// Helper: yellow bg + black fg + bold — the canonical hlsearch
    /// style used in the codebase's `match_style()` helper.
    fn match_style_yellow_bg() -> TuiStyle {
        TuiStyle::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }

    /// Mid-row match overlay on a single-span cell body: splits
    /// into pre/mid/post with the overlap region carrying the
    /// overlay style verbatim (fg + bg + modifiers all replaced).
    #[test]
    fn s3c3_match_overlay_splits_single_span_body() {
        let body = flat_body("abcdefgh", 0xcdd6f4);
        let overlay = match_style_yellow_bg();
        let out = apply_match_overlay(body, 2, 6, overlay);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_ref(), "ab");
        assert_eq!(out[1].content.as_ref(), "cdef");
        assert_eq!(out[2].content.as_ref(), "gh");
        // Middle span: overlay style exactly.
        assert_eq!(out[1].style.fg, Some(Color::Black));
        assert_eq!(out[1].style.bg, Some(Color::Yellow));
        assert!(out[1].style.add_modifier.contains(Modifier::BOLD));
        // Outer spans keep the cell-derived fg, bg=None.
        assert_eq!(out[0].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
        assert_eq!(out[0].style.bg, None);
        assert_eq!(out[2].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }

    /// Match overlay covering the entire body: every cell-derived
    /// span's style becomes the overlay style (no pre / post
    /// slices needed).
    #[test]
    fn s3c3_match_overlay_full_coverage() {
        let body = flat_body("hi", 0xcdd6f4);
        let overlay = match_style_yellow_bg();
        let out = apply_match_overlay(body, 0, 2, overlay);
        assert_eq!(collect_text(&out), "hi");
        for s in &out {
            assert_eq!(s.style.fg, Some(Color::Black));
            assert_eq!(s.style.bg, Some(Color::Yellow));
        }
    }

    /// Match overlay outside the body's byte range: no mutation.
    /// Captures the no-op contract for ranges past EOL.
    #[test]
    fn s3c3_match_overlay_outside_range_noop() {
        let body = flat_body("abc", 0xcdd6f4);
        let pre_text = collect_text(&body);
        let pre_styles: Vec<_> = body.iter().map(|s| s.style).collect();
        let out = apply_match_overlay(body, 10, 20, match_style_yellow_bg());
        assert_eq!(collect_text(&out), pre_text);
        let post_styles: Vec<_> = out.iter().map(|s| s.style).collect();
        assert_eq!(post_styles, pre_styles);
    }

    /// Match overlay across a fg boundary in a multi-span body:
    /// both halves of the overlap region adopt the overlay style.
    /// Captures the cross-boundary walk semantics for bg-layer
    /// overlays.
    #[test]
    fn s3c3_match_overlay_spans_multi_span_body() {
        let fg_a = 0xff0000;
        let fg_b = 0x00ff00;
        let cells = vec![
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        assert_eq!(body.len(), 2);
        let overlay = match_style_yellow_bg();
        let out = apply_match_overlay(body, 2, 5, overlay);
        // Walk the spans and verify the overlap [2, 5) carries
        // the overlay style on BOTH sides of the fg-boundary
        // at byte 3.
        let mut cursor = 0usize;
        for s in &out {
            let len = s.content.len();
            let span_start = cursor;
            let span_end = cursor + len;
            if span_start >= 2 && span_end <= 5 {
                assert_eq!(
                    s.style.bg,
                    Some(Color::Yellow),
                    "overlap span '{}' must carry overlay bg",
                    s.content.as_ref()
                );
            }
            cursor = span_end;
        }
    }

    /// Match overlay's style assignment REPLACES the cell's
    /// existing modifiers (it does not OR in). A cell carrying
    /// BOLD from syntax style + an overlay style without BOLD
    /// results in the overlay's modifier set, not the merged
    /// one. This is the documented difference vs. the semantic
    /// tokens overlay's `add_modifier` semantics.
    #[test]
    fn s3c3_match_overlay_replaces_modifiers() {
        // Cell with BOLD syntax modifier.
        let cells = vec![
            Cell::new(b'x' as u32, 0xcba6f7, 0, cell_flags::BOLD),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        // Overlay style has ITALIC, NOT BOLD.
        let overlay = TuiStyle::default()
            .bg(Color::Yellow)
            .add_modifier(Modifier::ITALIC);
        let out = apply_match_overlay(body, 0, 1, overlay);
        // Replaced: ITALIC present, BOLD absent.
        assert!(out[0].style.add_modifier.contains(Modifier::ITALIC));
        assert!(
            !out[0].style.add_modifier.contains(Modifier::BOLD),
            "match overlay must REPLACE the style — BOLD from syntax must be dropped"
        );
    }

    /// Underline overlay (diagnostics): adds UNDERLINED modifier
    /// to the overlap region; fg / bg from earlier passes stay
    /// intact. Captures the additive contract for the diagnostic
    /// layer.
    #[test]
    fn s3c3_underline_overlay_adds_only_underline() {
        let cells = vec![
            Cell::new(b'e' as u32, 0xcdd6f4, 0x1e1e2e, cell_flags::BOLD),
            Cell::new(b'r' as u32, 0xcdd6f4, 0x1e1e2e, cell_flags::BOLD),
            Cell::new(b'r' as u32, 0xcdd6f4, 0x1e1e2e, cell_flags::BOLD),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        // Sanity: cells share style → one span pre-overlay.
        assert_eq!(body.len(), 1);
        let out =
            apply_underline_overlay(body, 0, 3, Color::Red /* unused */);
        // UNDERLINED added; fg / bg / BOLD preserved.
        for s in &out {
            assert!(s.style.add_modifier.contains(Modifier::UNDERLINED));
            assert!(s.style.add_modifier.contains(Modifier::BOLD));
            assert_eq!(s.style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
            assert_eq!(s.style.bg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        }
    }

    /// Underline overlay covering only part of the row: pre /
    /// mid (underlined) / post slices. The mid keeps the cell's
    /// existing style and only adds UNDERLINED.
    #[test]
    fn s3c3_underline_overlay_partial_coverage_keeps_outer_style() {
        let body = flat_body("abcdef", 0xcdd6f4);
        let out = apply_underline_overlay(body, 2, 4, Color::Red);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_ref(), "ab");
        assert!(!out[0].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(out[1].content.as_ref(), "cd");
        assert!(out[1].style.add_modifier.contains(Modifier::UNDERLINED));
        // Mid keeps the cell's fg too — only the modifier is
        // additive.
        assert_eq!(out[1].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
        assert_eq!(out[2].content.as_ref(), "ef");
        assert!(!out[2].style.add_modifier.contains(Modifier::UNDERLINED));
    }

    /// Multiple bg-layer overlays compose by sequential
    /// application: doc-highlight (yellow bg) followed by visual
    /// selection (cyan bg) leaves the cyan bg on the overlap —
    /// the second pass's `apply_match_overlay` REPLACES the
    /// first's. Captures the documented sequencing.
    #[test]
    fn s3c3_match_overlay_composes_by_sequence() {
        let body = flat_body("abcdef", 0xcdd6f4);
        let yellow = TuiStyle::default().bg(Color::Yellow).fg(Color::Black);
        let cyan = TuiStyle::default().bg(Color::Cyan).fg(Color::Black);
        // Doc-highlight: bytes [1, 5).
        let after_dh = apply_match_overlay(body, 1, 5, yellow);
        // Visual selection: bytes [2, 4) — replaces the inner
        // portion of the doc-highlight bg.
        let out = apply_match_overlay(after_dh, 2, 4, cyan);
        // Walk and verify: byte 0 unchanged; byte 1 = yellow;
        // bytes 2..4 = cyan; byte 4 = yellow; byte 5 = unchanged.
        let mut cursor = 0usize;
        for s in &out {
            let len = s.content.len();
            let mid = cursor + len / 2;
            match mid {
                0 => assert_eq!(s.style.bg, None),
                1 => assert_eq!(s.style.bg, Some(Color::Yellow)),
                2 | 3 => assert_eq!(s.style.bg, Some(Color::Cyan)),
                4 => assert_eq!(s.style.bg, Some(Color::Yellow)),
                5 => assert_eq!(s.style.bg, None),
                _ => {}
            }
            cursor += len;
        }
    }

    // ---- S3.c.4 — fold suffix + post-overlay inlay splice ----
    //
    // Two tail-of-pipeline passes wrap up the per-line render:
    //
    // 1. The post-overlay inlay splice (`splice_virtual_text_into_spans`)
    //    inserts the LSP `inlayHint` virtual text into the body at
    //    a source-byte offset. Cell-derived source spans cover
    //    source bytes 1:1 with `line_text`, exactly matching the
    //    RowPrepaint shape this splice was designed against.
    // 2. The closed-fold `' ┄ N lines folded'` suffix is a plain
    //    `Span::push` after every overlay — no byte-position math.
    //    It composes trivially with any body shape.
    //
    // For cell-derived bodies these two passes are unchanged
    // contractually; the tests here pin that against regression.

    use crate::render::splice_virtual_text_into_spans;

    fn dim_gray_style() -> TuiStyle {
        TuiStyle::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC)
    }

    /// Inlay splice at byte 0 prepends the virtual text before
    /// every cell-derived span. The body's first span stays
    /// intact; the virtual span emits before it.
    #[test]
    fn s3c4_inlay_splice_at_byte_zero_prepends() {
        let body = flat_body("hi", 0xcdd6f4);
        let out = splice_virtual_text_into_spans(
            body,
            0,
            ": ".to_string(),
            dim_gray_style(),
        );
        assert_eq!(collect_text(&out), ": hi");
        // First span is the virtual splice.
        assert_eq!(out[0].content.as_ref(), ": ");
        assert_eq!(out[0].style.fg, Some(Color::DarkGray));
        // Source body follows.
        assert_eq!(out[1].content.as_ref(), "hi");
        assert_eq!(out[1].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }

    /// Inlay splice mid-row splits the single cell-derived span
    /// on the byte boundary. Captures the contract that the
    /// splice walks cell-derived spans byte-by-byte.
    #[test]
    fn s3c4_inlay_splice_mid_span_splits_the_span() {
        let body = flat_body("abcdef", 0xcdd6f4);
        // One source span covers bytes [0, 6); splice at byte 3.
        let out = splice_virtual_text_into_spans(
            body,
            3,
            "[i]".to_string(),
            dim_gray_style(),
        );
        // pre / inlay / post.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_ref(), "abc");
        assert_eq!(out[1].content.as_ref(), "[i]");
        assert_eq!(out[1].style.fg, Some(Color::DarkGray));
        assert!(out[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(out[2].content.as_ref(), "def");
        // Both source halves keep the cell's fg.
        assert_eq!(out[0].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
        assert_eq!(out[2].style.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }

    /// Inlay splice at a span boundary inserts cleanly between
    /// two cell-derived spans without splitting either. Pin the
    /// no-split contract — important so byte-position tracking
    /// stays simple for downstream code that walks display
    /// columns.
    #[test]
    fn s3c4_inlay_splice_at_span_boundary_does_not_split() {
        let fg_a = 0xff0000;
        let fg_b = 0x00ff00;
        let cells = vec![
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'b' as u32, fg_a, 0, 0),
            Cell::new(b'c' as u32, fg_b, 0, 0),
            Cell::new(b'd' as u32, fg_b, 0, 0),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        assert_eq!(body.len(), 2);
        // Splice at byte 2 — exactly the boundary between the
        // two cell-derived spans.
        let out = splice_virtual_text_into_spans(
            body,
            2,
            "/*X*/".to_string(),
            dim_gray_style(),
        );
        // Three spans: first body span "ab" / inlay "/*X*/" /
        // second body span "cd". Neither body span split.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_ref(), "ab");
        assert_eq!(out[0].style.fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
        assert_eq!(out[1].content.as_ref(), "/*X*/");
        assert_eq!(out[2].content.as_ref(), "cd");
        assert_eq!(out[2].style.fg, Some(Color::Rgb(0x00, 0xff, 0x00)));
    }

    /// Inlay splice past the end of the body (typical LSP `inlayHint`
    /// trailing annotation at EOL) appends the virtual text as the
    /// final span.
    #[test]
    fn s3c4_inlay_splice_past_end_appends() {
        let body = flat_body("hi", 0xcdd6f4);
        let out = splice_virtual_text_into_spans(
            body,
            999,
            " → unit".to_string(),
            dim_gray_style(),
        );
        // Body spans first, virtual span last.
        assert_eq!(collect_text(&out), "hi → unit");
        let last = out.last().unwrap();
        assert_eq!(last.content.as_ref(), " → unit");
        assert_eq!(last.style.fg, Some(Color::DarkGray));
    }

    /// Multiple inlay splices applied in reverse byte order (the
    /// production loop's pattern — `on_line.sort_by(|a, b|
    /// b.byte.cmp(&a.byte))` then splice) so earlier splices
    /// don't shift later ones. Validates the cell-derived body
    /// composes correctly with the same loop shape.
    #[test]
    fn s3c4_multiple_inlays_in_reverse_byte_order() {
        let body = flat_body("xy", 0xcdd6f4);
        // Two splices: at byte 1 and at byte 2. Apply in reverse
        // (byte 2 first, then byte 1) so the byte-1 splice's
        // offset stays valid.
        let after_second = splice_virtual_text_into_spans(
            body,
            2,
            "/B/".to_string(),
            dim_gray_style(),
        );
        let after_first = splice_virtual_text_into_spans(
            after_second,
            1,
            "/A/".to_string(),
            dim_gray_style(),
        );
        // Result: `x` `/A/` `y` `/B/` — both inlays at their
        // intended positions.
        assert_eq!(collect_text(&after_first), "x/A/y/B/");
    }

    /// Fold suffix is a plain trailing-span push — composes with
    /// any body shape. Captures that cell-derived bodies don't
    /// need special handling.
    #[test]
    fn s3c4_fold_suffix_appends_after_cell_body() {
        let body = flat_body("fn main() {}", 0xcdd6f4);
        let pre_count = body.len();
        let mut out = body;
        // Mirror the closed-fold suffix push from
        // `compose_visible_lines_inner` line ~3590.
        out.push(Span::styled(
            " ┄ 3 lines folded".to_string(),
            TuiStyle::default().fg(Color::DarkGray),
        ));
        // Body untouched; one extra span at the tail.
        assert_eq!(out.len(), pre_count + 1);
        let last = out.last().unwrap();
        assert_eq!(last.content.as_ref(), " ┄ 3 lines folded");
        assert_eq!(last.style.fg, Some(Color::DarkGray));
    }

    /// Inlay splice followed by fold suffix: the splice lands at
    /// its byte offset; the suffix appends at the very end after
    /// any inlay. Captures the documented ordering — overlays
    /// run first, then the inlay splice, then the fold suffix.
    #[test]
    fn s3c4_inlay_splice_then_fold_suffix_order() {
        let body = flat_body("ab", 0xcdd6f4);
        // Inlay at byte 2 (end-of-line).
        let mut after_inlay = splice_virtual_text_into_spans(
            body,
            2,
            ": T".to_string(),
            dim_gray_style(),
        );
        // Fold suffix.
        after_inlay.push(Span::styled(
            " ┄ 5 lines folded".to_string(),
            TuiStyle::default().fg(Color::DarkGray),
        ));
        assert_eq!(collect_text(&after_inlay), "ab: T ┄ 5 lines folded");
        // Suffix is the LAST span; inlay is before it.
        let last = after_inlay.last().unwrap();
        assert!(last.content.as_ref().starts_with(" ┄"));
    }

    /// Overlay spanning two different-fg cell-derived spans:
    /// each gets its overlapping portion fg-replaced. Captures
    /// the cross-boundary walk.
    #[test]
    fn s3c2_overlay_spans_multi_span_body() {
        let fg_a = 0xff0000;
        let fg_b = 0x00ff00;
        // 6 bytes: `aaabbb`. cells 0..3 are fg_a; cells 3..6 are
        // fg_b — body emits two spans.
        let cells = vec![
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'a' as u32, fg_a, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
            Cell::new(b'b' as u32, fg_b, 0, 0),
        ];
        let body = cell_row_to_source_spans(&row(cells));
        assert_eq!(body.len(), 2);
        // Overlay covers bytes [2, 5) — crossing the boundary at
        // byte 3.
        let overlay_fg = Color::Yellow;
        let out = apply_semantic_token_overlay(
            body,
            2,
            5,
            overlay_fg,
            Modifier::empty(),
        );
        // Expect:
        //  - "aa" (fg_a, unchanged)
        //  - "a"  (overlay fg)
        //  - "bb" (overlay fg)
        //  - "b"  (fg_b, unchanged)
        let combined: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(combined, "aaabbb");
        // Walk and check the overlay-fg covers byte positions
        // 2..5.
        let mut cursor = 0usize;
        for s in &out {
            let len = s.content.len();
            let overlap_start = cursor.max(2);
            let overlap_end = (cursor + len).min(5);
            if overlap_start < overlap_end {
                // This span overlaps the overlay range; if fully
                // inside, fg must be overlay_fg.
                if cursor >= 2 && cursor + len <= 5 {
                    assert_eq!(
                        s.style.fg,
                        Some(overlay_fg),
                        "span '{}' at byte {cursor} must carry overlay fg",
                        s.content.as_ref()
                    );
                }
            }
            cursor += len;
        }
    }
}
