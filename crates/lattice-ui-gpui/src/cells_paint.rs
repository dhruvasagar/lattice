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
use lattice_host::display_matrix::{DisplayLine, DisplayRun};
use lattice_host::ui::theme::Weight;

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

/// B3 (2026-06-04): the `DisplayLine` analogue of
/// [`cell_row_to_text_runs`] — the GPU's converter once it consumes the
/// canonical `DisplayMatrix` directly instead of the projected cell grid.
///
/// A `DisplayLine` carries style-*tagged* runs (a `lattice_syntax::Style` +
/// non-style flag bits), not resolved colours. This resolves each run to
/// the exact `(fg, bg, flags)` the worker's `display_line_to_cell_row`
/// projection produced (style → `theme.syntax_style(..).fg`; a
/// `WS_TRAILING` marker run → `whitespace_trailing_style` fg; an `INLAY`
/// run → the inlay-hint fg, no syntax modifiers), builds a synthetic
/// [`Cell`] from it, and reuses [`cell_to_text_run`] + [`style_key`] run
/// grouping — so the shaped output is byte-identical to the projected-cell
/// path the GPU consumed pre-B3 (`reverse`/`dim`/bg handling included).
///
/// Returns `(combined_text, runs, inlay_offsets)` over `line.text` (inlays
/// are inline in the display text, unlike the TUI's source-span path which
/// drops them); `inlay_offsets` is `line.col_map` verbatim.
pub fn display_line_to_text_runs(
    line: &DisplayLine,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
    font: &Font,
) -> (String, Vec<TextRun>, Vec<(u32, u32)>) {
    use lattice_host::ui::theme::resolve_syntax_style;
    let default_fg = resolve_syntax_style(resolved, ids, lattice_syntax::Style::Default)
        .fg
        .map(|c| c.to_rgb_u32(0))
        .unwrap_or(0);
    let trailing_fg = resolved
        .get(ids.whitespace_trailing)
        .fg
        .map(|c| c.to_rgb_u32(default_fg))
        .unwrap_or(default_fg);
    let mut runs: Vec<TextRun> = Vec::new();
    // T.10: each run group carries the synthetic [`Cell`] (fg/bg/flags,
    // the same payload as pre-T.10) PLUS the resolved rich attributes
    // ([`Weight`] / [`FontScale`]) the synthetic Cell can't hold —
    // `Cell` stays fg+flags by design (decided scope). The rich attrs
    // fold into the grouping signature ([`rich_key`]) so two runs with
    // identical fg/flags but different weight (a heading next to body
    // text) don't wrongly merge.
    let mut current: Option<(Cell, RichAttrs, usize)> = None;
    for run in line.runs.iter() {
        let run_len = run.len as usize;
        let (cell, rich) = display_run_to_synthetic_cell(run, resolved, ids, trailing_fg);
        match &mut current {
            Some((sample, sample_rich, len))
                if style_key(sample) == style_key(&cell)
                    && rich_key(sample_rich) == rich_key(&rich) =>
            {
                *len += run_len;
            }
            _ => {
                if let Some((sample, sample_rich, len)) = current.take() {
                    runs.push(rich_cell_to_text_run(&sample, &sample_rich, len, font));
                }
                current = Some((cell, rich, run_len));
            }
        }
    }
    if let Some((sample, sample_rich, len)) = current {
        runs.push(rich_cell_to_text_run(&sample, &sample_rich, len, font));
    }

    let inlay_offsets: Vec<(u32, u32)> = line.col_map.iter().copied().collect();
    (line.text.to_string(), runs, inlay_offsets)
}

/// F.2 (Thread F): split a display line into a base-size leading prefix
/// and a scaled remainder, for the emacs heading model — only the
/// heading *title* scales, the leading `#`/`##` markers stay base size
/// (`markdown-header-delimiter-face`). Returns `Some((prefix_cols,
/// title_scale))` where `prefix_cols` is the number of leading display
/// columns (chars) before the first run carrying a rich `scale > 1.0`
/// (the `# ` markers, resolved to `1.0`), and `title_scale` is that
/// run's scale; returns `None` for ordinary lines (no scaled run).
///
/// The GPUI peer renders a scaled row in two pieces sharing one baseline
/// — the prefix at base size, the title at `font_size * title_scale` —
/// and grows the row height by `title_scale` (variable row height). The
/// TUI peer has no analogue (a cell grid cannot vary font size); it
/// degrades to the resolved bold/weight/underline.
///
/// O(runs). Inlay / whitespace-marker runs carry no syntax scale and are
/// treated as base (mirrors [`display_run_to_synthetic_cell`]). Multi-
/// scale titles (e.g. inline code inside a heading) collapse to the
/// first scaled run's scale from `prefix_cols` onward — a deliberate
/// simplification (the common case is `[markers][title]`).
pub fn heading_scale_split(
    line: &DisplayLine,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> Option<(u32, f32)> {
    let text = line.text.as_ref();
    let mut byte = 0usize;
    let mut col = 0u32;
    for run in line.runs.iter() {
        let run_bytes = run.len as usize;
        let scale = if run.flags & (cell_flags::INLAY | cell_flags::WS_MARKER) != 0 {
            1.0
        } else {
            lattice_host::ui::theme::resolve_syntax_style(resolved, ids, run.style)
                .scale
                .map(|s| s.as_ratio())
                .unwrap_or(1.0)
        };
        if scale > 1.0 {
            return Some((col, scale));
        }
        let end = (byte + run_bytes).min(text.len());
        col += text
            .get(byte..end)
            .map(|s| s.chars().count() as u32)
            .unwrap_or(0);
        byte = end;
    }
    None
}

/// T.10: the rich-vocabulary attributes a [`DisplayRun`]'s resolved
/// host [`Style`] carries that the fg+flags [`Cell`] cannot. `weight`
/// is the visible deliverable (GPUI honors it via `TextRun.font.weight`);
/// `scale` is wired through to the apply site but no builtin element
/// sets it (a larger run would grow the line and break the uniform
/// row-height scroll model — Layer 2, deferred). `family` is read at
/// the synthetic-cell step but deferred (no renderer font table yet).
#[derive(Clone, Copy, Default)]
struct RichAttrs {
    weight: Option<Weight>,
    scale: Option<lattice_host::ui::theme::FontScale>,
}

/// Grouping signature for the rich attributes. Folded into the run
/// merge alongside [`style_key`] so a weight (or scale) change flushes
/// the in-progress run. `FontScale` is `u16` hundredths and `Weight`
/// is a small enum, so this is `Copy` + cheap to compare.
fn rich_key(r: &RichAttrs) -> (Option<Weight>, Option<u16>) {
    (r.weight, r.scale.map(|s| s.0))
}

/// Resolve one [`DisplayRun`] to the synthetic [`Cell`] the worker's
/// `display_line_to_cell_row` projection would have produced — same
/// fg/flag rules — so [`cell_to_text_run`] yields identical styling. The
/// codepoint is irrelevant (`cell_to_text_run` reads only fg/bg/flags).
///
/// T.10: also returns the resolved rich attributes ([`RichAttrs`]) the
/// fg+flags `Cell` can't carry — `weight` (honored on GPUI),
/// `scale`/`family` (threaded but deferred). Inlay / whitespace-marker
/// runs take NO rich attrs (they're synthetic decoration, not syntax
/// text), mirroring how they take no syntax modifiers.
fn display_run_to_synthetic_cell(
    run: &DisplayRun,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
    trailing_fg: u32,
) -> (Cell, RichAttrs) {
    let host = lattice_host::ui::theme::resolve_syntax_style(resolved, ids, run.style);
    let style_fg = host.fg.map(|c| c.to_rgb_u32(0)).unwrap_or(0);
    let mut mods: u16 = 0;
    let m = &host.modifiers;
    if m.bold {
        mods |= cell_flags::BOLD;
    }
    if m.italic {
        mods |= cell_flags::ITALIC;
    }
    if m.underline {
        mods |= cell_flags::UNDERLINE;
    }
    if m.dim {
        mods |= cell_flags::DIM;
    }
    if m.reverse {
        mods |= cell_flags::REVERSE;
    }
    // T.10: capture the rich attrs from the resolved host style. `family`
    // is resolved but DEFERRED — there's no renderer font table to map a
    // `FamilyId` to a `Font` yet, so we don't carry it (TODO: wire when a
    // font-family table lands).
    let rich = RichAttrs {
        weight: host.weight,
        scale: host.scale,
    };
    // DR.2: intra-line refinement — a refined run overrides its row's
    // diff tint with a stronger background. Foreground untouched, so
    // the syntax colour stays visible under it. `style_key` already
    // includes `cell.bg`, so run grouping picks this up for free.
    let refine_bg = run
        .refine
        .and_then(|kind| {
            let element = match kind {
                lattice_cells::RefineKind::Added => ids.diff_add_refine_bg,
                lattice_cells::RefineKind::Removed => ids.diff_remove_refine_bg,
            };
            resolved.get(element).bg
        })
        .map(|c| c.to_rgb_u32(0))
        .unwrap_or(0);
    let is_inlay = run.flags & cell_flags::INLAY != 0;
    let is_ws = run.flags & cell_flags::WS_MARKER != 0;
    let is_trailing = run.flags & cell_flags::WS_TRAILING != 0;
    if is_inlay {
        // Inlay runs take their OWN resolved fg with no syntax modifiers
        // — exactly what `display_line_to_cell_row` emits.
        //
        // DL.3a: this was a hardcoded `DarkGray` mirroring the worker's
        // `inlay_hint_fg`, with the run's style ignored on both sides.
        // The run now carries a real style (`Style::InlayHint` for LSP
        // hints, resolving through the registered `inlay.hint` element;
        // `Style::Element` for a producer with its own vocabulary), so
        // both peers resolve it the same way they resolve everything
        // else.
        (
            Cell::new(0, style_fg, 0, cell_flags::INLAY),
            RichAttrs::default(),
        )
    } else if is_ws {
        let fg = if is_trailing { trailing_fg } else { style_fg };
        (
            Cell::new(0, fg, 0, mods | cell_flags::WS_MARKER),
            RichAttrs::default(),
        )
    } else {
        (Cell::new(0, style_fg, refine_bg, mods), rich)
    }
}

/// Run-grouping key. Consecutive cells with the same key merge
/// into one [`TextRun`]; a change in any component flushes the
/// in-progress run. See [`STYLE_FLAGS_MASK`] for which flag bits
/// are considered style-significant.
fn style_key(cell: &Cell) -> (u32, u32, u16) {
    (cell.fg, cell.bg, cell.flags & STYLE_FLAGS_MASK)
}

/// T.10: map the rich-vocabulary [`Weight`] onto GPUI's
/// [`FontWeight`] axis. A resolved [`Weight`] is finer than the
/// `bold` modifier (which only ever sets `BOLD` = 700) and wins
/// over it when present on a run's style.
fn weight_to_font_weight(weight: Weight) -> FontWeight {
    match weight {
        Weight::Thin => FontWeight::THIN,
        Weight::ExtraLight => FontWeight::EXTRA_LIGHT,
        Weight::Light => FontWeight::LIGHT,
        Weight::Normal => FontWeight::NORMAL,
        Weight::Medium => FontWeight::MEDIUM,
        Weight::SemiBold => FontWeight::SEMIBOLD,
        Weight::Bold => FontWeight::BOLD,
        Weight::ExtraBold => FontWeight::EXTRA_BOLD,
        Weight::Black => FontWeight::BLACK,
    }
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

/// T.10: build a [`TextRun`] from a synthetic [`Cell`] plus the
/// resolved rich attributes the `Cell` can't carry. Delegates to
/// [`cell_to_text_run`] for fg/bg/underline/italic/bold-bool styling
/// (identical to pre-T.10), then layers the rich vocabulary on top:
///
/// - `weight` → OVERRIDE `font.weight` via [`weight_to_font_weight`].
///   A resolved [`Weight`] is finer than the bold-bool's 700 and wins
///   over it when present (e.g. `syntax.heading.1` resolves to
///   `ExtraBold`, not just bold).
/// - `scale` → wired here but applied to nothing: a [`TextRun`] has no
///   per-run font-size field in this gpui version (the single
///   `font_size` passed to `shape_line` sizes the whole line), and no
///   builtin element sets `scale`, so there is nothing to grow. See the
///   apply note below.
fn rich_cell_to_text_run(cell: &Cell, rich: &RichAttrs, len: usize, font_base: &Font) -> TextRun {
    let mut run = cell_to_text_run(cell, len, font_base);
    // A resolved `Weight` wins over the bold-bool's `FontWeight::BOLD`.
    if let Some(weight) = rich.weight {
        run.font.weight = weight_to_font_weight(weight);
    }
    // F.2 (Thread F): scale is honored at the row level, not per-run.
    // This gpui's `TextRun` has no per-run font size (the whole shaped
    // line takes one `font_size`), so `editor_element` reads
    // [`heading_scale_split`] and renders a scaled row in two pieces
    // sharing a baseline — the base-size marker prefix + the scaled
    // title — each shaped at its own `font_size`, with a matching row
    // height (variable row height). The per-run `scale` read here stays
    // inert by design — it only feeds [`rich_key`] so a scale change
    // still flushes the in-progress run group. `family` is still
    // deferred (no renderer font table).
    let _scale_ratio = rich.scale.map(|s| s.as_ratio()).unwrap_or(1.0);
    run
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

    // ---- DR.5 — a refined run reaches the painted cell (GPUI peer) ----
    //
    // The exact peer of `cells_render`'s pair in `lattice-ui-tui`.
    // Refinement is computed once in `lattice-diff` and both renderers
    // read the same `DisplayRun.refine`, so the two paint sites are the
    // only place they can silently diverge — and neither had a test.

    fn theme_defaults() -> (
        std::sync::Arc<lattice_host::ui::theme::ResolvedTheme>,
        lattice_host::ui::theme::BuiltinElementIds,
    ) {
        use lattice_host::ui::theme::{
            BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry as _,
        };
        let reg = InMemoryThemeRegistry::with_defaults();
        (reg.resolved(), BuiltinElementIds::capture(&reg))
    }

    fn refined_run(refine: Option<lattice_cells::RefineKind>) -> DisplayRun {
        DisplayRun {
            len: 3,
            style: lattice_syntax::Style::Default,
            flags: 0,
            refine,
        }
    }

    /// A refined run paints a background; an unrefined one does not.
    /// The foreground is untouched either way — refinement is a
    /// background axis, which is what keeps syntax colour readable.
    #[test]
    fn a_refined_run_paints_the_refine_background() {
        let (resolved, ids) = theme_defaults();
        let (plain, _) = display_run_to_synthetic_cell(&refined_run(None), &resolved, &ids, 0, 0);
        let (refined, _) = display_run_to_synthetic_cell(
            &refined_run(Some(lattice_cells::RefineKind::Removed)),
            &resolved,
            &ids,
            0,
            0,
        );
        assert_eq!(plain.bg, 0, "an unrefined run carries no background");
        assert_ne!(refined.bg, 0, "a refined run must paint one");
        assert_eq!(
            refined.fg, plain.fg,
            "refinement is a background axis; the syntax fg survives"
        );
    }

    /// The two sides paint DIFFERENT backgrounds — a shared colour
    /// would render an addition and a deletion identically.
    #[test]
    fn the_two_refine_sides_paint_different_backgrounds() {
        let (resolved, ids) = theme_defaults();
        let paint = |kind| {
            display_run_to_synthetic_cell(&refined_run(Some(kind)), &resolved, &ids, 0, 0)
                .0
                .bg
        };
        let added = paint(lattice_cells::RefineKind::Added);
        let removed = paint(lattice_cells::RefineKind::Removed);
        assert_ne!(added, 0);
        assert_ne!(removed, 0);
        assert_ne!(
            added, removed,
            "added and removed refinement must be distinguishable"
        );
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

    /// B3: the display path (`display_line_to_text_runs`) resolves a
    /// keyword run to the theme's keyword fg and an INLAY run to the
    /// inlay-hint fg, over the combined display text, with `col_map` →
    /// `inlay_offsets` verbatim — matching the projected-cell path.
    #[test]
    fn display_line_resolves_keyword_and_inlay_runs() {
        use lattice_host::display_matrix::{DisplayLine, DisplayRun};
        use lattice_host::ui::theme::{
            BuiltinElementIds, Color, InMemoryThemeRegistry, NamedColor, ThemeRegistry as _,
            resolve_syntax_style,
        };
        // T.5.b: resolve through the default registry's resolved table.
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        let kw_fg = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::Keyword)
            .fg
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(0);
        let inlay_fg = Color::Named(NamedColor::DarkGray).to_rgb_u32(0);
        // "fn" (Keyword) followed by ": i32" (inlay-spliced virtual text).
        let text = "fn: i32";
        let line = DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from(text),
            runs: std::sync::Arc::from(
                vec![
                    DisplayRun {
                        len: 2,
                        style: lattice_syntax::Style::Keyword,
                        flags: 0,
                        refine: None,
                    },
                    DisplayRun {
                        len: 5,
                        style: lattice_syntax::Style::Default,
                        flags: cell_flags::INLAY,
                        refine: None,
                    },
                ]
                .into_boxed_slice(),
            ),
            col_map: std::sync::Arc::from([(2u32, 5u32)] as [(u32, u32); 1]),
            col_count: text.chars().count() as u32,
            fold: None,
        };
        let (combined, runs, offsets) =
            display_line_to_text_runs(&line, &resolved, &ids, &font("monospace"));
        assert_eq!(combined, "fn: i32");
        assert_eq!(runs.len(), 2, "keyword + inlay → two runs");
        assert_eq!(runs[0].len, 2);
        assert_eq!(
            runs[0].color,
            rgb(kw_fg).into(),
            "keyword run takes theme keyword fg"
        );
        assert_eq!(runs[1].len, 5);
        assert_eq!(
            runs[1].color,
            rgb(inlay_fg).into(),
            "inlay run takes the inlay-hint fg"
        );
        assert_eq!(offsets, vec![(2, 5)], "col_map transfers as inlay_offsets");
    }

    /// T.10: a `syntax.heading.1` run resolves to the rich-vocabulary
    /// `ExtraBold` weight, and the display path threads it onto the
    /// run's `TextRun.font.weight` (overriding the bold-bool's 700).
    /// The demo deliverable: headings render heavier on GPUI.
    #[test]
    fn display_line_heading_run_takes_extrabold_weight() {
        use lattice_host::display_matrix::{DisplayLine, DisplayRun};
        use lattice_host::ui::theme::{
            BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry as _,
        };
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        let text = "# Title";
        let line = DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from(text),
            runs: std::sync::Arc::from(
                vec![DisplayRun {
                    len: text.len() as u32,
                    style: lattice_syntax::Style::Heading1,
                    flags: 0,
                    refine: None,
                }]
                .into_boxed_slice(),
            ),
            col_map: std::sync::Arc::from([] as [(u32, u32); 0]),
            col_count: text.chars().count() as u32,
            fold: None,
        };
        let (_, runs, _) = display_line_to_text_runs(&line, &resolved, &ids, &font("monospace"));
        assert_eq!(runs.len(), 1, "single heading run");
        assert_eq!(
            runs[0].font.weight,
            FontWeight::EXTRA_BOLD,
            "syntax.heading.1 resolves to ExtraBold and the run takes it"
        );
    }

    /// F.2 (Thread F): `heading_scale_split` returns the leading base-
    /// size marker width + the title scale. For `## Title` the markers
    /// run (`## `, 3 cols, base) precedes the scaled title run, so the
    /// split is `(3, 1.4)`. Plain body text returns `None`.
    #[test]
    fn heading_scale_split_reports_prefix_and_title_scale() {
        use lattice_host::display_matrix::{DisplayLine, DisplayRun};
        use lattice_host::ui::theme::{
            BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry as _,
        };
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        // "## " markers (Default, base) + "Title" (Heading2, 1.4x).
        let text = "## Title";
        let heading = DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from(text),
            runs: std::sync::Arc::from(
                vec![
                    DisplayRun {
                        len: 3,
                        style: lattice_syntax::Style::Default,
                        flags: 0,
                        refine: None,
                    },
                    DisplayRun {
                        len: 5,
                        style: lattice_syntax::Style::Heading2,
                        flags: 0,
                        refine: None,
                    },
                ]
                .into_boxed_slice(),
            ),
            col_map: std::sync::Arc::from([] as [(u32, u32); 0]),
            col_count: text.chars().count() as u32,
            fold: None,
        };
        let (prefix_cols, title_scale) =
            heading_scale_split(&heading, &resolved, &ids).expect("heading splits");
        assert_eq!(prefix_cols, 3, "the '## ' markers are the base-size prefix");
        assert!(
            (title_scale - 1.4).abs() < 1e-3,
            "title scales at heading.2's 1.4x"
        );

        let body = DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from("plain"),
            runs: std::sync::Arc::from(
                vec![DisplayRun {
                    len: 5,
                    style: lattice_syntax::Style::Default,
                    flags: 0,
                    refine: None,
                }]
                .into_boxed_slice(),
            ),
            col_map: std::sync::Arc::from([] as [(u32, u32); 0]),
            col_count: 5,
            fold: None,
        };
        assert_eq!(
            heading_scale_split(&body, &resolved, &ids),
            None,
            "plain body text has no scaled run"
        );
    }

    /// F.2: a leading inlay/whitespace run carries no syntax scale, so it
    /// counts toward the base-size prefix; the first genuinely-scaled run
    /// (the heading title) sets `title_scale`.
    #[test]
    fn heading_scale_split_counts_inlay_prefix_as_base() {
        use lattice_host::display_matrix::{DisplayLine, DisplayRun};
        use lattice_host::ui::theme::{
            BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry as _,
        };
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        let text = "# H1";
        let line = DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from(text),
            runs: std::sync::Arc::from(
                vec![
                    DisplayRun {
                        len: 2,
                        style: lattice_syntax::Style::Default,
                        flags: 0,
                        refine: None,
                    },
                    DisplayRun {
                        len: 2,
                        style: lattice_syntax::Style::Heading1,
                        flags: 0,
                        refine: None,
                    },
                ]
                .into_boxed_slice(),
            ),
            col_map: std::sync::Arc::from([] as [(u32, u32); 0]),
            col_count: text.chars().count() as u32,
            fold: None,
        };
        let (prefix_cols, title_scale) =
            heading_scale_split(&line, &resolved, &ids).expect("splits");
        assert_eq!(prefix_cols, 2, "'# ' is the 2-col base prefix");
        assert!((title_scale - 1.6).abs() < 1e-3, "heading.1 title at 1.6x");
    }

    /// T.10: two runs that share fg/flags but differ in resolved weight
    /// must NOT merge — the rich grouping signature breaks the run.
    /// (heading.1 = ExtraBold next to heading.2 = Bold.)
    #[test]
    fn display_line_different_weight_breaks_runs() {
        use lattice_host::display_matrix::{DisplayLine, DisplayRun};
        use lattice_host::ui::theme::{
            BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry as _,
        };
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        // Force identical fg by overriding both headings to the same fg;
        // they differ only in weight (ExtraBold vs Bold) after T.10.
        let text = "ab";
        let line = DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from(text),
            runs: std::sync::Arc::from(
                vec![
                    // DR.2 added `refine` to `DisplayRun` and updated
                    // GPUI's production paint path but not these
                    // `window`-gated initializers, so this test module
                    // has not compiled since. Invisible because
                    // `--features window` is not in the default build —
                    // the exact gap the cross-renderer parity rule
                    // exists to prevent.
                    DisplayRun {
                        len: 1,
                        style: lattice_syntax::Style::Heading1,
                        flags: 0,
                        refine: None,
                    },
                    DisplayRun {
                        len: 1,
                        style: lattice_syntax::Style::Heading2,
                        flags: 0,
                        refine: None,
                    },
                ]
                .into_boxed_slice(),
            ),
            col_map: std::sync::Arc::from([] as [(u32, u32); 0]),
            col_count: 2,
            fold: None,
        };
        let (_, runs, _) = display_line_to_text_runs(&line, &resolved, &ids, &font("monospace"));
        assert_eq!(
            runs.len(),
            2,
            "ExtraBold heading.1 and Bold heading.2 do not merge"
        );
        assert_eq!(runs[0].font.weight, FontWeight::EXTRA_BOLD);
        assert_eq!(runs[1].font.weight, FontWeight::BOLD);
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
            Cell::new(
                b'd' as u32,
                fg,
                0,
                cell_flags::INLAY | cell_flags::WS_MARKER,
            ),
        ];
        let r = row(cells);
        let (text, runs, _) = cell_row_to_text_runs(&r, &font("monospace"));
        assert_eq!(text, "abcd");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 4);
    }
}
