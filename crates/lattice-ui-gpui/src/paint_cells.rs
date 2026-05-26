//! S4.final.b (2026-05-27): per-cell `paint_glyph` body path.
//!
//! The active-pane document body, when the runtime toggle
//! [`paint_cells_enabled`] is on, is drawn by this module
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
//! S4.final.b is the wiring + integration point. Modifier
//! rendering (BOLD weight, ITALIC slant, UNDERLINE geometry,
//! DIM attenuation, REVERSE swap) lands in S4.final.d. Emoji /
//! font-fallback handling lands in S4.final.e — until then
//! cells whose primary-font glyph_id is `.notdef` simply
//! aren't drawn (the cached `None` resolution stays sticky).
//!
//! Cursor + diagnostic + overlay quads continue to flow
//! through the existing `EditorElement::paint` bookkeeping
//! (computed against `ShapedLine` metrics in `prepaint`); only
//! the text-body glyph emission swaps. Hit-testing migrates in
//! S4.final.c.

#![cfg(feature = "window")]

use std::sync::Mutex;

use gpui::{Bounds, Font, Hsla, Pixels, Point, Window, fill, point, rgb, size};
use lattice_cells::CellRow;

use crate::glyph_resolver::GlyphResolver;

/// Paint a single document-body row by emitting per-cell
/// background quads and glyphs. Used by `EditorElement::paint`
/// when [`paint_cells_enabled`] is on; otherwise the row goes
/// through `ShapedLine::paint`.
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
    row: &CellRow,
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

    for (idx, cell) in row.cells.iter().enumerate() {
        let cell_x = line_origin.x + advance * (idx as f32);

        if cell.bg != 0 {
            let bg_bounds = Bounds::new(
                point(cell_x, line_origin.y),
                size(advance, line_height),
            );
            window.paint_quad(fill(bg_bounds, rgb(cell.bg)));
        }

        let Some(ch) = char::from_u32(cell.codepoint) else {
            continue;
        };
        if ch == ' ' || ch == '\0' {
            // Blank cells: bg quad already drawn, no glyph to
            // emit. Skip resolver work.
            continue;
        }

        let Some(resolved) = resolver_guard.resolve(ch, font, font_size, window) else {
            // Sticky-`None` codepoint: no glyph available in any
            // fallback font. S4.final.e will draw a tofu /
            // placeholder; today we leave the bg quad and move on.
            continue;
        };

        let baseline = point(cell_x, line_origin.y + ascent);
        let fg_u32 = if cell.fg != 0 { cell.fg } else { default_fg };
        let color: Hsla = rgb(fg_u32).into();

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

    painted
}

/// Returns `true` when the runtime toggle is on. Reads
/// `LATTICE_PAINT_CELLS` from the environment once and caches
/// the result for the process lifetime. Accepts `"1"`, `"true"`,
/// `"TRUE"` (case-insensitive); any other value (including
/// unset) returns `false`.
///
/// The toggle gates the entire body cutover. When off (the
/// default), `EditorElement::paint` runs the existing
/// `ShapedLine::paint` body loop unchanged. When on,
/// [`paint_cells_row`] runs per visible row and `ShapedLine`'s
/// body paint is skipped; the prepaint-time ShapedLine
/// metrics still drive cursor + overlay positioning until
/// S4.final.c migrates hit-testing.
pub fn paint_cells_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("LATTICE_PAINT_CELLS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}
