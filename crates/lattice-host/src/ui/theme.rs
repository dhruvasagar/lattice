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
    builtin_themes, parse_color, BuiltinElementIds, Color, FamilyId, FontScale,
    InMemoryThemeRegistry, Modifiers, NamedColor, NamedTheme, ResolvedTheme, Style, ThemeRegistry,
    ThemeRegistryHandle, Weight,
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

/// One full UI theme. Cheap to clone (every field is `Copy`).
///
/// S2.3.a (2026-05-26): `Hash` is part of the derive set so the
/// cell-grid renderer's [`crate::render_state::CellsRenderState`]
/// can fold the theme into [`lattice_cells::MatrixVersion::theme`]
/// — any palette change bumps the version and the cell-builder
/// rebuilds with fresh fg/bg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Theme {
    // ---- Pane chrome ----
    // T.4.c: `inactive_pane_overlay` moved to the `pane.inactive_overlay`
    // element. T.9: `pane_status_active` / `pane_status_inactive` /
    // `pane_separator` STYLE fields are gone — they now resolve through
    // the `pane.status.active` / `pane.status.inactive` / `pane.separator`
    // elements, and their `:set ui.*` fg overrides flow via the registry
    // override API (see `Editor::sync_host_theme_from_config`). The
    // non-style chrome (`dim_inactive_panes` flag + separator glyphs)
    // stays here until T.6.t migrates the flags/chars to `ui.*` options.
    pub dim_inactive_panes: bool,
    pub pane_separator_vertical: char,
    pub pane_separator_horizontal: char,

    // ---- File tree ----
    // T.4.c: `file_tree_{dir,hidden,file}_style` moved to the
    // `file_tree.{dir,hidden,file}` elements; renderers read via
    // `ResolvedTheme`. `nerd_fonts` is a flag, not a style → T.6.t.
    pub nerd_fonts: bool,

    // ---- Diagnostics ----
    // T.4.a: the `diagnostic_*_style` fields moved to theme elements
    // (`diagnostic.{error,warning,info,hint}`); both renderers read
    // them via `ResolvedTheme`. The glyph chars stay here until
    // T.6.t migrates them to `ui.*` options (a glyph is not a style).
    pub diagnostic_error_glyph: char,
    pub diagnostic_warning_glyph: char,
    pub diagnostic_info_glyph: char,
    pub diagnostic_hint_glyph: char,

    // ---- Whitespace + current-line ----
    // T.4.d/T.5: `cursor_line_bg` → `editor.cursor_line`;
    // `whitespace`/`whitespace.trailing` styles now resolve through
    // the theme elements (cell builder + display-line paths + native
    // cache all read the resolved table).

    // ---- *messages* buffer level styling ----
    // T.4.d: moved to `messages.{timestamp,trace,debug,info,warn,error}`
    // elements; the TUI reads them via the resolved table.

    // ---- Diff ----
    // T.4.b: the diff sign styles + line/block tints moved to theme
    // elements (`diff.{add,change,remove,conflict}.sign`,
    // `diff.{add,change,conflict}.line`, `diff.deletion_block`); both
    // renderers read them via `ResolvedTheme`. The `+`/`~`/`-`/`?`
    // glyphs are still hardcoded at the gutter render sites.
}

impl Default for Theme {
    fn default() -> Self {
        // Defaults mirror `lattice-ui-tui::theme::Theme::default()`
        // exactly; a test in lattice-ui-tui asserts the adapted
        // form matches the TUI's hand-rolled defaults.
        Self {
            // T.9: pane_status_active / pane_status_inactive /
            // pane_separator styles now live as the `pane.status.*` /
            // `pane.separator` elements (active = reverse+bold, inactive
            // = darkgray+dim, separator = darkgray) — see the element
            // registry defaults.
            dim_inactive_panes: true,
            pane_separator_vertical: '│',
            pane_separator_horizontal: '─',

            // T.4.c: inactive_pane_overlay + file_tree.* resolve
            // through theme elements now.
            nerd_fonts: false,

            diagnostic_error_glyph: '■',
            diagnostic_warning_glyph: '▲',
            diagnostic_info_glyph: '●',
            diagnostic_hint_glyph: '·',

            // T.5.c: whitespace styles resolve through theme elements.
            // T.4.d: cursor_line + messages.* likewise. T.4.b: diff.*.
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn default_theme_dims_inactive_panes() {
        let t = Theme::default();
        assert!(t.dim_inactive_panes);
    }

    #[test]
    fn default_separator_is_box_drawing_vertical() {
        assert_eq!(Theme::default().pane_separator_vertical, '│');
    }

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
