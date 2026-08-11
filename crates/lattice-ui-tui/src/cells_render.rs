//! B2.4 (2026-06-04): `DisplayMatrix` → ratatui span conversion.
//!
//! The TUI consumes the canonical
//! [`lattice_host::display_matrix::DisplayMatrix`] published by the
//! cells worker and emits a `Vec<Span<'static>>` ready for ratatui's
//! text widgets. This module is the substrate→TUI translation layer.
//!
//! ## Why a separate module
//!
//! A `DisplayLine` carries style-*tagged* byte runs
//! ([`lattice_host::display_matrix::DisplayRun`]: a
//! `lattice_syntax::Style` enum + non-style flag bits), NOT resolved
//! colours — the renderer resolves `style → fg + modifiers` against the
//! host theme at paint (truecolor `Color::Rgb`, or ratatui's
//! nearest-named downsample in low-colour terminals).
//! [`display_line_to_source_spans`] walks the runs, drops `INLAY` runs
//! so the spans cover source bytes one-to-one with the rope line (the
//! overlay pipeline positions overlays by source-byte offset), groups
//! consecutive runs with the same resolved style, and emits one
//! [`ratatui::text::Span`] per group.
//!
//! Pre-B2.4 this module converted the projected per-character cell grid
//! (`cell_row_to_source_spans`); B2.4a cut the TUI over to the
//! `DisplayMatrix` and B2.4b deleted the cell→span path. The remaining
//! cell path (worker projection, GPU reader) goes away in B3/B4.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// B2.4 (2026-06-04): the `DisplayLine` analogue of
/// [`cell_row_to_source_spans`] — the TUI's source of document-body
/// spans once it consumes the canonical `DisplayMatrix` directly
/// instead of the projected cell grid. Walks `line.runs`, drops
/// `INLAY` runs (so the spans cover source bytes one-to-one with the
/// rope line — overlays positioned by source byte work unchanged), and
/// resolves each run's syntax `style` + flags → a ratatui [`Style`] via
/// the host theme.
///
/// **Resolution parity.** This reproduces the worker's
/// `display_line_to_cell_row` projection byte-for-byte so the cutover is
/// visually invisible: `style → fg` via `theme.syntax_style(..).fg`
/// (→ `to_rgb_u32`); a `WS_TRAILING` marker run takes
/// `theme.whitespace_trailing_style` fg; modifiers come from the host
/// theme's per-style `Modifiers`. fg `0` maps to `None` (pane default),
/// exactly as the cell path's `fg == 0`. Runs are grouped by the
/// *resolved* ratatui style, so two runs with distinct syntax tags but
/// identical resolved colour merge — matching the cell path, which
/// grouped on resolved `(fg, bg, mods)`.
///
/// Returns an empty `Vec` for an empty line or one that is entirely
/// inlay runs.
pub fn display_line_to_source_spans(
    line: &lattice_host::display_matrix::DisplayLine,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> Vec<Span<'static>> {
    use lattice_cells::cell_flags;
    use lattice_host::ui::theme::resolve_syntax_style;
    // Resolve the default + trailing fg once (mirrors the worker's
    // `display_line_to_cell_row`). `default_fg` is the trailing fg's
    // fallback, exactly as in the projection.
    let default_fg = resolve_syntax_style(resolved, ids, lattice_syntax::Style::Default)
        .fg
        .map(|c| c.to_rgb_u32(0))
        .unwrap_or(0);
    let trailing_fg = resolved
        .get(ids.whitespace_trailing)
        .fg
        .map(|c| c.to_rgb_u32(default_fg))
        .unwrap_or(default_fg);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_text = String::new();
    let mut current_style: Option<Style> = None;
    let mut byte_off = 0usize;
    for run in line.runs.iter() {
        let end = byte_off + run.len as usize;
        let slice = &line.text[byte_off..end];
        byte_off = end;
        // Source spans exclude inlay runs.
        if run.flags & cell_flags::INLAY != 0 {
            continue;
        }
        let style = display_run_to_style(run, resolved, ids, trailing_fg);
        if current_style == Some(style) {
            current_text.push_str(slice);
        } else {
            if !current_text.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut current_text),
                    current_style.unwrap_or_default(),
                ));
            }
            current_text.push_str(slice);
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

/// Resolve one `DisplayRun` (non-inlay) to a ratatui [`Style`] via the
/// host theme. Mirrors `display_line_to_cell_row`'s per-run resolution:
/// a `WS_TRAILING` marker run takes `trailing_fg`; otherwise the syntax
/// style's fg. fg `0` ⇒ leave unset (pane default). Modifier bits come
/// from the host theme's per-style `Modifiers`.
fn display_run_to_style(
    run: &lattice_host::display_matrix::DisplayRun,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
    trailing_fg: u32,
) -> Style {
    use lattice_cells::cell_flags;
    let is_ws_marker = run.flags & cell_flags::WS_MARKER != 0;
    let is_trailing = run.flags & cell_flags::WS_TRAILING != 0;
    let host = lattice_host::ui::theme::resolve_syntax_style(resolved, ids, run.style);
    // For `Style::Default` this is the theme's default fg — the same
    // value `default_fg` resolves to — so no special-case is needed.
    let style_fg = host.fg.map(|c| c.to_rgb_u32(0)).unwrap_or(0);
    let fg = if is_ws_marker && is_trailing {
        trailing_fg
    } else {
        style_fg
    };
    let mut style = Style::default();
    if fg != 0 {
        style = style.fg(rgb_u32_to_color(fg));
    }
    // DR.2: intra-line refinement. A refined run overrides its row's
    // diff tint with a stronger one; foreground is untouched, which is
    // what keeps the syntax colour visible underneath. Set here rather
    // than by a byte-range walk in the renderer because run boundaries
    // already account for tab expansion and whitespace markers — a
    // source-byte walk over RENDERED content drifts on the first tab.
    if let Some(kind) = run.refine {
        let element = match kind {
            lattice_cells::RefineKind::Added => ids.diff_add_refine_bg,
            lattice_cells::RefineKind::Removed => ids.diff_remove_refine_bg,
        };
        if let Some(bg) = resolved.get(element).bg {
            style = style.bg(rgb_u32_to_color(bg.to_rgb_u32(0)));
        }
    }
    let m = &host.modifiers;
    let mut mods = Modifier::empty();
    if m.bold {
        mods |= Modifier::BOLD;
    }
    if m.italic {
        mods |= Modifier::ITALIC;
    }
    if m.underline {
        mods |= Modifier::UNDERLINED;
    }
    if m.dim {
        mods |= Modifier::DIM;
    }
    if m.reverse {
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
    use lattice_cells::cell_flags;

    /// Build a source-body `Vec<Span>` from `(text, fg_rgb)` segments —
    /// the shape `display_line_to_source_spans` produces and the overlay
    /// pipeline below consumes. `fg_rgb == 0` ⇒ pane-default fg
    /// (`style.fg == None`). One span per segment (pass pre-grouped
    /// segments); replaces the retired cell-grid body builders. The
    /// overlay functions under test are renderer-generic over
    /// `Vec<Span>`, so the bodies need no cell/display provenance.
    fn body(parts: &[(&str, u32)]) -> Vec<Span<'static>> {
        parts
            .iter()
            .map(|(text, fg)| {
                let mut style = Style::default();
                if *fg != 0 {
                    style = style.fg(rgb_u32_to_color(*fg));
                }
                Span::styled(text.to_string(), style)
            })
            .collect()
    }

    // ---- B2.4 — DisplayMatrix → source spans ----
    //
    // `display_line_to_source_spans` is the TUI's cutover from the
    // projected cell grid to the canonical `DisplayMatrix`. These pin
    // its resolution parity (style→theme fg, inlay drop, trailing fg,
    // merge-across-dropped-inlay) so it stays byte-identical to the
    // worker's `display_line_to_cell_row` projection the cell path used.

    use lattice_host::display_matrix::{DisplayLine, DisplayRun};
    use lattice_host::ui::theme::{
        BuiltinElementIds, InMemoryThemeRegistry, ResolvedTheme, ThemeRegistry as _,
        resolve_syntax_style,
    };

    /// T.5.b: build the resolved table + builtin ids from the
    /// default registry — the same construction the renderer uses
    /// at boot. Replaces the deleted `HostTheme::default()` +
    /// `Theme::syntax_style` reads in these tests.
    fn defaults() -> (std::sync::Arc<ResolvedTheme>, BuiltinElementIds) {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        (resolved, ids)
    }

    /// Build a `DisplayLine` from `(text, style, flags)` run specs.
    fn dline(specs: &[(&str, lattice_syntax::Style, u16)]) -> DisplayLine {
        let mut text = String::new();
        let mut runs = Vec::new();
        for (s, style, flags) in specs {
            runs.push(DisplayRun {
                len: s.len() as u32,
                style: *style,
                flags: *flags,
                refine: None,
            });
            text.push_str(s);
        }
        let col_count = text.chars().count() as u32;
        DisplayLine {
            source_line: 0,
            text: std::sync::Arc::from(text.as_str()),
            runs: std::sync::Arc::from(runs.into_boxed_slice()),
            col_map: std::sync::Arc::from([] as [(u32, u32); 0]),
            col_count,
            fold: None,
        }
    }

    /// Expected ratatui colour for a `0xRRGGBB` fg: `None` when `0`
    /// (pane default), else `Color::Rgb`. Mirrors the resolver.
    fn expect_color(rgb: u32) -> Option<Color> {
        if rgb == 0 {
            None
        } else {
            Some(Color::Rgb(
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            ))
        }
    }

    /// A keyword run resolves to the host theme's keyword fg (+ bold if
    /// the theme sets it), exactly as the projection did.
    #[test]
    fn display_keyword_run_takes_theme_keyword_fg() {
        let (resolved, ids) = defaults();
        let kw_fg = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::Keyword)
            .fg
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(0);
        let line = dline(&[
            ("fn", lattice_syntax::Style::Keyword, 0),
            (" x", lattice_syntax::Style::Default, 0),
        ]);
        let spans = display_line_to_source_spans(&line, &resolved, &ids);
        assert_eq!(spans[0].content.as_ref(), "fn");
        assert_eq!(spans[0].style.fg, expect_color(kw_fg));
    }

    /// Inlay runs are dropped from the source-span output (overlays
    /// position by source byte, so inlay text must not appear here).
    #[test]
    fn display_source_spans_drop_inlay_runs() {
        let (resolved, ids) = defaults();
        let line = dline(&[
            ("hi", lattice_syntax::Style::Default, 0),
            (": i32", lattice_syntax::Style::Default, cell_flags::INLAY),
        ]);
        let text: String = display_line_to_source_spans(&line, &resolved, &ids)
            .iter()
            .map(|s| s.content.as_ref().to_string())
            .collect();
        assert_eq!(text, "hi");
    }

    /// A `WS_TRAILING` marker run takes the theme's trailing-whitespace
    /// fg (the cell path baked this into the cell `fg`).
    #[test]
    fn display_trailing_ws_run_takes_trailing_fg() {
        let (resolved, ids) = defaults();
        let default_fg = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::Default)
            .fg
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(0);
        let trailing_fg = resolved
            .get(ids.whitespace_trailing)
            .fg
            .map(|c| c.to_rgb_u32(default_fg))
            .unwrap_or(default_fg);
        let line = dline(&[
            ("x", lattice_syntax::Style::Default, 0),
            (
                "·",
                lattice_syntax::Style::Default,
                cell_flags::WS_MARKER | cell_flags::WS_TRAILING,
            ),
        ]);
        let spans = display_line_to_source_spans(&line, &resolved, &ids);
        let last = spans.last().unwrap();
        assert_eq!(last.content.as_ref(), "·");
        assert_eq!(last.style.fg, expect_color(trailing_fg));
    }

    /// Two same-style source runs separated by a dropped inlay merge
    /// into one span — matches the cell path, which grouped on the
    /// resolved style across the inlay-filtered cell stream.
    #[test]
    fn display_same_style_runs_merge_across_dropped_inlay() {
        let (resolved, ids) = defaults();
        let line = dline(&[
            ("ab", lattice_syntax::Style::Default, 0),
            ("INLAY", lattice_syntax::Style::Default, cell_flags::INLAY),
            ("cd", lattice_syntax::Style::Default, 0),
        ]);
        let spans = display_line_to_source_spans(&line, &resolved, &ids);
        assert_eq!(
            spans.len(),
            1,
            "same-style runs merge across a dropped inlay"
        );
        assert_eq!(spans[0].content.as_ref(), "abcd");
    }

    /// A line of only inlay runs yields no source spans.
    #[test]
    fn display_all_inlay_line_yields_no_source_spans() {
        let (resolved, ids) = defaults();
        let line = dline(&[(": T", lattice_syntax::Style::Default, cell_flags::INLAY)]);
        assert!(display_line_to_source_spans(&line, &resolved, &ids).is_empty());
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

    use crate::render::{WhitespaceDecoration, apply_whitespace_decoration};
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
        let body = body(&[("a b", fg)]);
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
        let body = body(&[("  hi", fg)]);
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
        let body = body(&[("hi  ", fg)]);
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
        let body = body(&[("x\ty", fg)]);
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
        let body = body(&[("hi", fg)]);
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
        let body_before = body(&[("a b", fg)]);
        let line_text = "a b";
        let d = ws_deco_all_off();
        let body_after = apply_whitespace_decoration(body_before.clone(), line_text, &d);
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
        // `a ` (fg_a, one span) then `b` (fg_b) — the space lands at the
        // span boundary; the classifier must still fire at byte 1.
        let body = body(&[("a ", fg_a), ("b", fg_b)]);
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

    /// Helper: build a uniform-fg body covering one short line — one
    /// span, the shape a single-style `DisplayLine` resolves to.
    fn flat_body(text: &str, fg: u32) -> Vec<Span<'static>> {
        body(&[(text, fg)])
    }

    /// Mid-row overlay: covers bytes [2, 6) of an 8-byte line.
    /// Result: three spans — pre (unchanged) / mid (new fg +
    /// modifiers) / post (unchanged).
    #[test]
    fn s3c2_overlay_splits_span_when_partial() {
        let body = flat_body("abcdefgh", 0xcdd6f4);
        let overlay_fg = Color::Rgb(0xff, 0x00, 0x00);
        let overlay_mods = Modifier::ITALIC;
        let out = apply_semantic_token_overlay(body, 2, 6, overlay_fg, overlay_mods);
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
        let out = apply_semantic_token_overlay(body, 0, 2, overlay_fg, Modifier::empty());
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
        let out = apply_semantic_token_overlay(body, 10, 20, Color::Red, Modifier::ITALIC);
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
        // Body span carries BOLD from syntax style.
        let fg = 0xcba6f7;
        let body = vec![Span::styled(
            "kw".to_string(),
            Style::default()
                .fg(rgb_u32_to_color(fg))
                .add_modifier(Modifier::BOLD),
        )];
        // Overlay adds ITALIC.
        let out = apply_semantic_token_overlay(body, 0, 2, Color::Cyan, Modifier::ITALIC);
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
        // Seed a body span with a bg from an earlier pass.
        let body = vec![Span::styled(
            "x".to_string(),
            Style::default()
                .fg(rgb_u32_to_color(0xcdd6f4))
                .bg(rgb_u32_to_color(0x1e1e2e)),
        )];
        assert_eq!(body[0].style.bg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        let out = apply_semantic_token_overlay(body, 0, 1, Color::Magenta, Modifier::empty());
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
        let body = body(&[("aaa", fg_a), ("bbb", fg_b)]);
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
        // Body span with BOLD syntax modifier.
        let body = vec![Span::styled(
            "x".to_string(),
            Style::default()
                .fg(rgb_u32_to_color(0xcba6f7))
                .add_modifier(Modifier::BOLD),
        )];
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
        // "err" with BOLD + bg from earlier passes — one span.
        let body = vec![Span::styled(
            "err".to_string(),
            Style::default()
                .fg(rgb_u32_to_color(0xcdd6f4))
                .bg(rgb_u32_to_color(0x1e1e2e))
                .add_modifier(Modifier::BOLD),
        )];
        assert_eq!(body.len(), 1);
        let out = apply_underline_overlay(body, 0, 3, Color::Red /* unused */);
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
        let out = splice_virtual_text_into_spans(body, 0, ": ".to_string(), dim_gray_style());
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
        let out = splice_virtual_text_into_spans(body, 3, "[i]".to_string(), dim_gray_style());
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
        let body = body(&[("ab", fg_a), ("cd", fg_b)]);
        assert_eq!(body.len(), 2);
        // Splice at byte 2 — exactly the boundary between the
        // two cell-derived spans.
        let out = splice_virtual_text_into_spans(body, 2, "/*X*/".to_string(), dim_gray_style());
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
        let out =
            splice_virtual_text_into_spans(body, 999, " → unit".to_string(), dim_gray_style());
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
        let after_second =
            splice_virtual_text_into_spans(body, 2, "/B/".to_string(), dim_gray_style());
        let after_first =
            splice_virtual_text_into_spans(after_second, 1, "/A/".to_string(), dim_gray_style());
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
        let mut after_inlay =
            splice_virtual_text_into_spans(body, 2, ": T".to_string(), dim_gray_style());
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
        // 6 bytes: `aaabbb` — `aaa` (fg_a) + `bbb` (fg_b) → two spans.
        let body = body(&[("aaa", fg_a), ("bbb", fg_b)]);
        assert_eq!(body.len(), 2);
        // Overlay covers bytes [2, 5) — crossing the boundary at
        // byte 3.
        let overlay_fg = Color::Yellow;
        let out = apply_semantic_token_overlay(body, 2, 5, overlay_fg, Modifier::empty());
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
