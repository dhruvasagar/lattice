//! The host `Theme` + its `SyntaxStyle → visual style` mapping.
//!
//! The renderer-neutral *primitives* — `Color`, `Style`,
//! `Modifiers`, `NamedColor`, the rich-vocabulary attribute types,
//! and `parse_color` — live in the leaf crate `lattice-theme` and
//! are **re-exported here** so every existing `lattice_host::ui::theme`
//! / `host_theme` call site is unchanged (T.1, theme-system slice
//! plan). Renderer crates (`lattice-ui-tui`, `lattice-ui-gpui`) ship
//! adapters that convert these into their native style types
//! (ratatui `Style` / `Color`, GPUI `Hsla` + per-run font shaping);
//! the host owns the canonical theme, each renderer maintains a
//! cached adapted view for hot-path reads.
//!
//! `Theme` itself, `Theme::syntax_style`, and the default theme stay
//! here for now. The element registry + palette + resolution
//! (`lattice-theme` T.2/T.3) eventually subsume the flat struct; the
//! consumer-migration thread (T.4+) repoints renderers at the
//! resolved table. See `docs/dev/architecture/theme-system.md`.

pub use lattice_theme::{
    builtin_themes, parse_color, BuiltinElementIds, Color, ColorRef, ElementInfo, ElementName,
    ElementOwner, FamilyId, FontScale, InMemoryThemeRegistry, Modifiers, NamedColor, NamedTheme,
    ResolvedTheme, Style, StyleSpec, ThemeRegistry, ThemeRegistryHandle, Weight,
};

/// T.5: map a `lattice_syntax::Style` category to its builtin
/// `syntax.*` [`lattice_theme::ElementId`]. The single source of the
/// syntax→element mapping, shared by the cell builder, both renderers'
/// display-line paths, and the diff overlay. Lives host-side because
/// `lattice-theme` is a leaf crate and cannot depend on
/// `lattice-syntax`.
pub fn syntax_element_id(
    ids: &BuiltinElementIds,
    style: lattice_syntax::Style,
) -> lattice_theme::ElementId {
    use lattice_syntax::Style as S;
    match style {
        S::Default => ids.syntax_default,
        S::Comment => ids.syntax_comment,
        S::LineComment => ids.syntax_line_comment,
        S::String => ids.syntax_string,
        S::Keyword => ids.syntax_keyword,
        S::Type => ids.syntax_type,
        S::Number => ids.syntax_number,
        S::Function => ids.syntax_function,
        S::Constant => ids.syntax_constant,
        S::Variable => ids.syntax_variable,
        S::Operator => ids.syntax_operator,
        S::Punctuation => ids.syntax_punctuation,
        S::Attribute => ids.syntax_attribute,
        S::Heading1 => ids.syntax_heading_1,
        S::Heading2 => ids.syntax_heading_2,
        S::Heading3 => ids.syntax_heading_3,
        S::Heading4 => ids.syntax_heading_4,
        S::Heading5 => ids.syntax_heading_5,
        S::Heading6 => ids.syntax_heading_6,
        S::Bold => ids.syntax_bold,
        S::Italic => ids.syntax_italic,
        S::Link => ids.syntax_link,
        S::Url => ids.syntax_url,
        S::MarkupRaw => ids.syntax_markup_raw,
        S::Markup => ids.syntax_markup,
    }
}

/// T.5: resolve a syntax category to its concrete [`Style`] via the
/// resolved table. The replacement for the deleted
/// `Theme::syntax_style` color `match` — colors now flow from the
/// active theme's palette through the resolved table. Every syntax
/// consumer (cell builder, display-line paths, diff overlay) calls
/// this, then adapts the host `Style` to its renderer-native form.
pub fn resolve_syntax_style(
    resolved: &ResolvedTheme,
    ids: &BuiltinElementIds,
    style: lattice_syntax::Style,
) -> Style {
    resolved.get(syntax_element_id(ids, style))
}

// T.6.t (2026-06-18): the host `Theme` struct is DELETED. All STYLE
// fields moved to the element / resolved-table system (T.4/T.5); the
// final 8 non-style fields (`dim_inactive_panes`,
// `pane_separator_{vertical,horizontal}`, `nerd_fonts`, the four
// `diagnostic_*_glyph` chars) migrated to `ui.*` typed options in
// `crate::ui::theme_options`. Renderers read the style table via
// `ResolvedTheme` + `BuiltinElementIds`, and the non-style flags/chars
// via the typed-options registry. The cell-matrix invalidation key
// (`MatrixVersion::theme`) is now `ResolvedTheme::version()`, not a
// content-hash of this struct. See `docs/dev/architecture/theme-system.md`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    // ---- T.5.b: SyntaxStyle resolution via the resolved table ----
    //
    // The legacy `Theme::syntax_style` color `match` was deleted in
    // T.5.b; every consumer now resolves through
    // `resolve_syntax_style` against the active theme's resolved
    // table. These pin that the default resolved table reproduces
    // the legacy Catppuccin-Mocha literals exactly (keyword =
    // mauve + bold, comment = overlay0 + italic, default = text),
    // so the cutover is colour-identical.

    /// Build the resolved table + builtin ids from the default
    /// registry — the same construction every renderer uses at boot.
    fn defaults() -> (std::sync::Arc<ResolvedTheme>, BuiltinElementIds) {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        (resolved, ids)
    }

    #[test]
    fn resolve_syntax_keyword_carries_catppuccin_mauve_bold() {
        let (resolved, ids) = defaults();
        let s = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::Keyword);
        assert_eq!(s.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert!(s.modifiers.bold);
    }

    #[test]
    fn resolve_syntax_comment_is_overlay0_italic() {
        let (resolved, ids) = defaults();
        let s = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::Comment);
        assert_eq!(s.fg, Some(Color::Rgb(0x6c, 0x70, 0x86)));
        assert!(s.modifiers.italic);
        // Same shape for LineComment.
        let line = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::LineComment);
        assert_eq!(line, s);
    }

    #[test]
    fn resolve_syntax_default_uses_text_foreground() {
        let (resolved, ids) = defaults();
        let s = resolve_syntax_style(&resolved, &ids, lattice_syntax::Style::Default);
        assert_eq!(s.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }
}
