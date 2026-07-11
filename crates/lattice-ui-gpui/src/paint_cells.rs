//! S4.final.b (2026-05-27): per-cell `paint_glyph` body path.
//! S4.final.f (2026-05-27): default-on; env-var toggle retired.
//!
//! The active-pane document body is drawn by this module
//! instead of by [`gpui::ShapedLine::paint`]. The cell-grid
//! substrate (`Arc<CellMatrix>` published by the cells worker)
//! is walked one row at a time; each cell becomes:
//!
//! 1. A background `paint_quad` at `(cell_x, line_y) ..
//!    (cell_x + advance, line_y + line_height)` when
//!    `cell.bg != 0`.
//! 2. A `paint_glyph` (or `paint_emoji` for colour glyphs) at
//!    baseline `(cell_x, line_y + ascent)` using the resolved
//!    `(font_id, glyph_id)` pair from [`GlyphResolver`].
//!
//! Modifier rendering (BOLD weight / ITALIC slant / UNDERLINE
//! geometry / DIM attenuation / REVERSE swap) landed in
//! S4.final.d via [`apply_color_modifiers`] and
//! [`cell_font_variant`]. Emoji / CJK / non-Latin fallback
//! flows through GPUI's `layout_line` fallback chain in the
//! resolver (S4.final.e).
//!
//! Cursor + diagnostic + overlay quads continue to flow
//! through the existing `EditorElement::paint` bookkeeping
//! (computed against `ShapedLine` metrics in `prepaint`); only
//! the text-body glyph emission swaps. Hit-testing primitives
//! landed in S4.final.c (`crate::hit_test`), ready for the
//! eventual mouse-select handler.
//!
//! ## Active vs inactive panes
//!
//! `EditorElement.cell_matrix` is `Some` only for the active
//! pane (the cells worker publishes for the active document).
//! Active-pane rows therefore go through `paint_cells_row`
//! unconditionally. Inactive panes (`cell_matrix == None`) fall
//! through to the legacy `ShapedLine::paint` path inside
//! `EditorElement::paint`. Migrating inactive panes off
//! `shape_line` is a follow-up that needs the cells worker to
//! publish for non-active buffers.

#![cfg(feature = "window")]

use std::sync::Mutex;

use gpui::{
    Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Point, Window, fill, point, px, rgb, size,
};
use lattice_cells::Cell;

use crate::glyph_resolver::GlyphResolver;

/// Multiplier applied to each RGB channel of fg + bg when the
/// `DIM` flag is set. Matches `cells_paint::DIM_FACTOR` so the
/// GPUI peer's two cell-grid paths produce visually identical
/// output for DIM cells.
const DIM_FACTOR: f32 = 0.6;

/// Pixels of vertical offset from the glyph baseline at which
/// the underline quad is painted. Positive = below baseline.
/// GPUI 0.2.2's `TextSystem` doesn't expose `underline_position`
/// publicly (only `ShapedLine::paint` consumes the
/// `UnderlineStyle` internally), so paint_cells uses a
/// font-agnostic constant offset. Two px below the baseline
/// reads as a flat underline at typical 14–16-px body sizes.
const UNDERLINE_OFFSET_FROM_BASELINE: f32 = 2.0;
/// Thickness of the underline quad in pixels. Matches
/// `cells_paint::cell_to_text_run`'s `UnderlineStyle.thickness =
/// px(1.0)`.
const UNDERLINE_THICKNESS_PX: f32 = 1.0;

/// Multiply each RGB channel of `packed` by [`DIM_FACTOR`].
/// Used by `apply_color_modifiers` when the cell's `DIM` flag
/// is set. Mirrors `cells_paint::dim_channel` byte-for-byte so
/// the two paths agree on DIM colour output.
fn dim_channel(packed: u32) -> u32 {
    let r = ((packed >> 16) & 0xff) as f32;
    let g = ((packed >> 8) & 0xff) as f32;
    let b = (packed & 0xff) as f32;
    let r = (r * DIM_FACTOR) as u32;
    let g = (g * DIM_FACTOR) as u32;
    let b = (b * DIM_FACTOR) as u32;
    (r << 16) | (g << 8) | b
}

/// Apply REVERSE + DIM to a cell's `(fg, bg)` pair before
/// paint. REVERSE swaps the two; DIM attenuates both (each
/// independently; transparent bg stays transparent). Returns
/// the colours the caller should pass to `paint_quad` (bg) and
/// `paint_glyph` (fg).
///
/// REVERSE with `cell.bg == 0` documented behaviour: the swap
/// leaves `fg = 0` (renderer default) and `bg = cell.fg`.
/// Mirrors `cells_paint::cell_to_text_run`.
pub(crate) fn apply_color_modifiers(cell: &Cell) -> (u32, u32) {
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
    (fg, bg)
}

/// Build a per-cell [`Font`] variant from the row's base font
/// and the cell's modifier bits. `BOLD` → `weight =
/// FontWeight::BOLD`; `ITALIC` → `style = FontStyle::Italic`.
/// Other bits don't affect font selection.
///
/// `Font` clones are cheap (the family / fallbacks are
/// `Arc<SharedString>` underneath), so calling this per cell
/// is fine on the hot path. Adjacent cells with the same
/// modifier bits do redundant clones today; if profiling shows
/// it matters, a future slice can group cells by
/// `(BOLD, ITALIC)` and clone once per group.
pub(crate) fn cell_font_variant(base: &Font, cell: &Cell) -> Font {
    let mut font = base.clone();
    if cell.is_bold() {
        font.weight = FontWeight::BOLD;
    }
    if cell.is_italic() {
        font.style = FontStyle::Italic;
    }
    font
}

/// Paint only the background quads for a display row.
/// Used when `ui.ligatures=true` — text glyphs are handled by
/// [`gpui::ShapedLine::paint`] so that multi-char runs are shaped
/// as a unit and OpenType ligature sequences (`->`, `!=`, `=>`, …)
/// form. Background quads still come from the cell matrix so
/// syntax-token backgrounds render correctly.
pub fn paint_cells_row_bg_only(
    cells: &[Cell],
    line_origin: Point<Pixels>,
    advance: Pixels,
    line_height: Pixels,
    window: &mut Window,
) {
    for (idx, cell) in cells.iter().enumerate() {
        let cell_x = line_origin.x + advance * (idx as f32);
        let (_, bg_u32) = apply_color_modifiers(cell);
        if bg_u32 != 0 {
            window.paint_quad(fill(
                Bounds::new(point(cell_x, line_origin.y), size(advance, line_height)),
                rgb(bg_u32),
            ));
        }
    }
}

/// Paint a single document-body row by emitting per-cell
/// background quads and glyphs. Used by `EditorElement::paint`
/// for active-pane document bodies when `ui.ligatures=false`;
/// otherwise the row goes through `ShapedLine::paint`.
///
/// W.5 (soft-wrap): takes a `&[Cell]` slice rather than the whole
/// `CellRow` so the caller can pass `CellRow::segment(seg, width)`
/// — one wrapped display segment's columns — and the `idx`-based
/// `cell_x` positions the slice at the row's local column 0. With
/// wrapping off the caller passes the full row (`segment(0, 0)`),
/// so behaviour is identical to the pre-W.5 whole-row paint.
///
/// Returns the number of glyphs actually drawn (background
/// quads not counted). Useful for diagnostics / bench (S5)
/// where coverage trends are interesting.
///
/// `line_origin` is the top-left of the row in window
/// coordinates. `advance` is the per-cell horizontal stride
/// (monospace; supplied by the caller — typically the same
/// `glyph_advance` the rest of `EditorElement::paint`
/// uses). `line_height` sizes the bg quad; `ascent` locates
/// the baseline within the row. `default_fg` is the colour
/// painted when `cell.fg == 0` (caller resolves the host theme
/// default foreground).
///
/// The lock on `resolver` is acquired once per row, not per
/// cell. Paint is single-threaded inside the GPUI window event
/// loop so there's no contention, but the coarser lock keeps
/// the per-glyph overhead minimal.
#[allow(clippy::too_many_arguments)]
pub fn paint_cells_row(
    cells: &[Cell],
    line_origin: Point<Pixels>,
    advance: Pixels,
    line_height: Pixels,
    ascent: Pixels,
    font: &Font,
    font_size: Pixels,
    default_fg: u32,
    resolver: &Mutex<GlyphResolver>,
    window: &mut Window,
) -> usize {
    let mut painted = 0usize;
    let mut resolver_guard = match resolver.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            // Paint must not panic on the hot path; recover the
            // guard and emit a diagnostic. Lock poisoning is a
            // bug elsewhere — the cache itself doesn't have
            // invariants that a partial mutation would break, so
            // accepting the poisoned guard is safe.
            tracing::warn!(
                target: "lattice_gpui::paint_cells",
                "glyph resolver mutex was poisoned; continuing with recovered guard"
            );
            poisoned.into_inner()
        }
    };

    for (idx, cell) in cells.iter().enumerate() {
        let cell_x = line_origin.x + advance * (idx as f32);

        // S4.final.d: REVERSE + DIM applied first so subsequent
        // bg / fg / underline emission all see the final
        // colours. `apply_color_modifiers` mirrors
        // `cells_paint::cell_to_text_run`'s REVERSE + DIM
        // handling — the two paths agree on output.
        let (fg_u32, bg_u32) = apply_color_modifiers(cell);

        if bg_u32 != 0 {
            let bg_bounds = Bounds::new(point(cell_x, line_origin.y), size(advance, line_height));
            window.paint_quad(fill(bg_bounds, rgb(bg_u32)));
        }

        let Some(ch) = char::from_u32(cell.codepoint) else {
            continue;
        };
        if ch != ' ' && ch != '\0' {
            // S4.final.d: per-cell font variant for BOLD /
            // ITALIC. BOLD/ITALIC are font-selection bits;
            // they affect the glyph_id the resolver returns.
            // Other modifier bits (UNDERLINE / DIM / REVERSE)
            // don't change the glyph_id — they shape the paint
            // around it.
            let cell_font = cell_font_variant(font, cell);

            if let Some(resolved) = resolver_guard.resolve(ch, &cell_font, font_size, window) {
                let baseline = point(cell_x, line_origin.y + ascent);
                let final_fg = if fg_u32 != 0 { fg_u32 } else { default_fg };
                let color: Hsla = rgb(final_fg).into();

                let paint_result = if resolved.is_emoji {
                    window.paint_emoji(baseline, resolved.font_id, resolved.glyph_id, font_size)
                } else {
                    window.paint_glyph(
                        baseline,
                        resolved.font_id,
                        resolved.glyph_id,
                        font_size,
                        color,
                    )
                };

                match paint_result {
                    Ok(()) => painted += 1,
                    Err(err) => {
                        tracing::debug!(
                            target: "lattice_gpui::paint_cells",
                            ch = ?ch,
                            cell_idx = idx,
                            error = ?err,
                            "paint_glyph failed"
                        );
                    }
                }
            }
            // Sticky-`None` resolution falls through with no
            // glyph emitted (S4.final.e will draw a tofu).
        }

        // S4.final.d: underline quad. Painted regardless of
        // glyph success — the underline geometry doesn't depend
        // on the glyph itself, and a cell whose codepoint has
        // no glyph (sticky-None) still wants its underline.
        // The underline colour follows fg (after REVERSE / DIM),
        // falling back to the host theme default when fg=0.
        if cell.is_underline() {
            let underline_color = if fg_u32 != 0 { fg_u32 } else { default_fg };
            let underline_y = line_origin.y + ascent + px(UNDERLINE_OFFSET_FROM_BASELINE);
            let underline_bounds = Bounds::new(
                point(cell_x, underline_y),
                size(advance, px(UNDERLINE_THICKNESS_PX)),
            );
            window.paint_quad(fill(underline_bounds, rgb(underline_color)));
        }
    }

    painted
}

// S4.final.f (2026-05-27): the `paint_cells_enabled` env-var
// toggle retired here. `paint_cells_row` is now the default
// for active-pane document bodies in
// `EditorElement::paint`; the toggle is no longer the entry
// point. `LATTICE_PAINT_CELLS=1` no longer has any effect.

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::font;
    use lattice_cells::cell_flags;

    // ----- dim_channel -----

    /// `dim_channel` zeros stay zeros; `0xffffff * 0.6 = 153`
    /// per channel = `0x999999`. Locks the formula so a
    /// follow-up tweak of `DIM_FACTOR` shows up here. Mirrors
    /// `cells_paint::dim_channel`'s table — the two paths
    /// must agree.
    #[test]
    fn dim_channel_attenuation_table() {
        assert_eq!(dim_channel(0x000000), 0x000000);
        assert_eq!(dim_channel(0xffffff), 0x999999);
        // 0x80 = 128; 128 * 0.6 = 76.8; floor → 76 = 0x4c
        assert_eq!(dim_channel(0x808080), 0x4c4c4c);
    }

    // ----- apply_color_modifiers -----

    /// No modifier bits → colours pass through unchanged.
    #[test]
    fn apply_color_modifiers_no_bits_is_identity() {
        let cell = Cell::new('a' as u32, 0xff0000, 0x00ff00, 0);
        assert_eq!(apply_color_modifiers(&cell), (0xff0000, 0x00ff00));
    }

    /// REVERSE bit swaps fg ↔ bg.
    #[test]
    fn apply_color_modifiers_reverse_swaps_fg_and_bg() {
        let cell = Cell::new('a' as u32, 0xff0000, 0x00ff00, cell_flags::REVERSE);
        assert_eq!(apply_color_modifiers(&cell), (0x00ff00, 0xff0000));
    }

    /// REVERSE with `bg == 0`: documented limitation — the
    /// swap leaves `fg = 0` (renderer default) and `bg =
    /// cell.fg`. Same contract as `cells_paint::cell_to_text_run`.
    #[test]
    fn apply_color_modifiers_reverse_with_transparent_bg() {
        let cell = Cell::new('a' as u32, 0xff0000, 0, cell_flags::REVERSE);
        assert_eq!(apply_color_modifiers(&cell), (0, 0xff0000));
    }

    /// DIM attenuates fg and bg independently.
    #[test]
    fn apply_color_modifiers_dim_attenuates_both() {
        let cell = Cell::new('a' as u32, 0xffffff, 0x808080, cell_flags::DIM);
        assert_eq!(apply_color_modifiers(&cell), (0x999999, 0x4c4c4c));
    }

    /// DIM with `bg == 0`: bg stays transparent, fg
    /// attenuated.
    #[test]
    fn apply_color_modifiers_dim_keeps_transparent_bg() {
        let cell = Cell::new('a' as u32, 0xffffff, 0, cell_flags::DIM);
        assert_eq!(apply_color_modifiers(&cell), (0x999999, 0));
    }

    /// REVERSE + DIM compose: swap first, then attenuate.
    #[test]
    fn apply_color_modifiers_reverse_then_dim() {
        let cell = Cell::new(
            'a' as u32,
            0xffffff,
            0x808080,
            cell_flags::REVERSE | cell_flags::DIM,
        );
        // After REVERSE: (0x808080, 0xffffff).
        // After DIM: (0x4c4c4c, 0x999999).
        assert_eq!(apply_color_modifiers(&cell), (0x4c4c4c, 0x999999));
    }

    // ----- cell_font_variant -----

    /// No modifier bits → font variant equals the base font.
    #[test]
    fn cell_font_variant_no_bits_returns_base_font() {
        let base = font("monospace");
        let cell = Cell::new('a' as u32, 0, 0, 0);
        let variant = cell_font_variant(&base, &cell);
        assert_eq!(variant.weight, base.weight);
        assert_eq!(variant.style, base.style);
    }

    /// BOLD bit sets `font.weight = FontWeight::BOLD`.
    #[test]
    fn cell_font_variant_bold_sets_weight() {
        let base = font("monospace");
        let cell = Cell::new('a' as u32, 0, 0, cell_flags::BOLD);
        let variant = cell_font_variant(&base, &cell);
        assert_eq!(variant.weight, FontWeight::BOLD);
        assert_eq!(variant.style, FontStyle::Normal);
    }

    /// ITALIC bit sets `font.style = FontStyle::Italic`.
    #[test]
    fn cell_font_variant_italic_sets_style() {
        let base = font("monospace");
        let cell = Cell::new('a' as u32, 0, 0, cell_flags::ITALIC);
        let variant = cell_font_variant(&base, &cell);
        assert_eq!(variant.weight, FontWeight::NORMAL);
        assert_eq!(variant.style, FontStyle::Italic);
    }

    /// BOLD + ITALIC compose.
    #[test]
    fn cell_font_variant_bold_italic_compose() {
        let base = font("monospace");
        let cell = Cell::new('a' as u32, 0, 0, cell_flags::BOLD | cell_flags::ITALIC);
        let variant = cell_font_variant(&base, &cell);
        assert_eq!(variant.weight, FontWeight::BOLD);
        assert_eq!(variant.style, FontStyle::Italic);
    }

    /// `UNDERLINE`, `DIM`, `REVERSE`, `INLAY`, `WS_MARKER`
    /// don't affect the font variant. Locks the contract that
    /// only `BOLD` + `ITALIC` are font-selection bits.
    #[test]
    fn cell_font_variant_other_bits_dont_affect_font() {
        let base = font("monospace");
        let cell = Cell::new(
            'a' as u32,
            0,
            0,
            cell_flags::UNDERLINE
                | cell_flags::DIM
                | cell_flags::REVERSE
                | cell_flags::INLAY
                | cell_flags::WS_MARKER,
        );
        let variant = cell_font_variant(&base, &cell);
        assert_eq!(variant.weight, base.weight);
        assert_eq!(variant.style, base.style);
    }
}
