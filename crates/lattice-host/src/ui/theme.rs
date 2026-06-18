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
    parse_color, BuiltinElementIds, Color, FamilyId, FontScale, InMemoryThemeRegistry, Modifiers,
    NamedColor, ResolvedTheme, Style, ThemeRegistry, ThemeRegistryHandle, Weight,
};

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
    pub pane_status_active: Style,
    pub pane_status_inactive: Style,
    pub inactive_pane_overlay: Style,
    pub dim_inactive_panes: bool,
    pub pane_separator: Style,
    pub pane_separator_vertical: char,
    pub pane_separator_horizontal: char,

    // ---- File tree ----
    pub file_tree_dir_style: Style,
    pub file_tree_hidden_style: Style,
    pub file_tree_file_style: Style,
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
    pub whitespace_style: Style,
    pub whitespace_trailing_style: Style,
    pub cursor_line_bg: Color,

    // ---- *messages* buffer level styling ----
    pub messages_timestamp_style: Style,
    pub messages_trace_style: Style,
    pub messages_debug_style: Style,
    pub messages_info_style: Style,
    pub messages_warn_style: Style,
    pub messages_error_style: Style,

    // ---- Diff ----
    // T.4.b: the diff sign styles + line/block tints moved to theme
    // elements (`diff.{add,change,remove,conflict}.sign`,
    // `diff.{add,change,conflict}.line`, `diff.deletion_block`); both
    // renderers read them via `ResolvedTheme`. The `+`/`~`/`-`/`?`
    // glyphs are still hardcoded at the gutter render sites.
}

impl Theme {
    /// Renderer-neutral [`Style`] for a syntax-highlight category.
    /// The canonical source of truth for `SyntaxStyle → visual style`
    /// across all peers. Phase 5.8.AF.6 / issue-2 hoist: before this
    /// landed the TUI and GPUI peers each carried their own divergent
    /// mapping (TUI named-ANSI / GPUI Catppuccin hex). Both now read
    /// through this method and adapt the returned host [`Style`] into
    /// their renderer-native form.
    ///
    /// Palette: Catppuccin Mocha hex values (designed-for-readability
    /// dark theme). Truecolor terminals render exact; 16-color
    /// terminals get the ratatui closest-named fallback automatically.
    /// Modifiers (bold / italic / underline) layer on top so even a
    /// 16-color path retains the "this is a heading" / "this is a
    /// link" structural cues.
    pub fn syntax_style(&self, s: lattice_syntax::Style) -> Style {
        use lattice_syntax::Style as S;
        // Catppuccin Mocha — https://github.com/catppuccin/catppuccin
        // Text       cdd6f4
        // Subtext0   a6adc8
        // Overlay2   9399b2
        // Overlay0   6c7086
        // Lavender   b4befe
        // Blue       89b4fa
        // Sapphire   74c7ec
        // Sky        89dceb
        // Teal       94e2d5
        // Green      a6e3a1
        // Yellow     f9e2af
        // Peach      fab387
        // Maroon     eba0ac
        // Red        f38ba8
        // Mauve      cba6f7
        // Pink       f5c2e7
        let rgb = |r: u8, g: u8, b: u8| Color::Rgb(r, g, b);
        let style = |fg: Color| Style::empty().fg(fg);
        match s {
            S::Default => style(rgb(0xcd, 0xd6, 0xf4)),
            S::Comment | S::LineComment => style(rgb(0x6c, 0x70, 0x86)).italic(),
            S::String => style(rgb(0xa6, 0xe3, 0xa1)),
            S::Keyword => style(rgb(0xcb, 0xa6, 0xf7)).bold(),
            S::Type => style(rgb(0xf9, 0xe2, 0xaf)),
            S::Number => style(rgb(0xfa, 0xb3, 0x87)),
            S::Function => style(rgb(0x89, 0xb4, 0xfa)),
            S::Constant => style(rgb(0xfa, 0xb3, 0x87)),
            S::Variable => style(rgb(0xcd, 0xd6, 0xf4)),
            S::Operator => style(rgb(0x94, 0xe2, 0xd5)),
            S::Punctuation => style(rgb(0x93, 0x99, 0xb2)),
            S::Attribute => style(rgb(0xf3, 0x8b, 0xa8)),
            S::Heading1 => style(rgb(0xf3, 0x8b, 0xa8)).bold().underline(),
            S::Heading2 => style(rgb(0xfa, 0xb3, 0x87)).bold(),
            S::Heading3 => style(rgb(0xf9, 0xe2, 0xaf)).bold(),
            S::Heading4 => style(rgb(0xa6, 0xe3, 0xa1)).bold(),
            S::Heading5 => style(rgb(0x89, 0xb4, 0xfa)).bold(),
            S::Heading6 => style(rgb(0xcb, 0xa6, 0xf7)).bold(),
            S::Bold => style(rgb(0xeb, 0xa0, 0xac)).bold(),
            S::Italic => style(rgb(0xf5, 0xc2, 0xe7)).italic(),
            S::Link => style(rgb(0x89, 0xb4, 0xfa)).underline(),
            S::Url => style(rgb(0x74, 0xc7, 0xec)).underline(),
            S::MarkupRaw => style(rgb(0x6c, 0x70, 0x86)).dim(),
            S::Markup => style(rgb(0x93, 0x99, 0xb2)).bold(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        // Defaults mirror `lattice-ui-tui::theme::Theme::default()`
        // exactly; a test in lattice-ui-tui asserts the adapted
        // form matches the TUI's hand-rolled defaults.
        Self {
            pane_status_active: Style::empty().reverse().bold(),
            pane_status_inactive: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),
            inactive_pane_overlay: Style::empty().dim(),
            dim_inactive_panes: true,
            pane_separator: Style::empty().fg(Color::Named(NamedColor::DarkGray)),
            pane_separator_vertical: '│',
            pane_separator_horizontal: '─',

            file_tree_dir_style: Style::empty().fg(Color::Named(NamedColor::Blue)).bold(),
            file_tree_hidden_style: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),
            file_tree_file_style: Style::empty(),
            nerd_fonts: false,

            diagnostic_error_glyph: '■',
            diagnostic_warning_glyph: '▲',
            diagnostic_info_glyph: '●',
            diagnostic_hint_glyph: '·',

            whitespace_style: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),
            whitespace_trailing_style: Style::empty().fg(Color::Named(NamedColor::Red)),
            cursor_line_bg: Color::Indexed(236),

            messages_timestamp_style: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),
            messages_trace_style: Style::empty().dim(),
            messages_debug_style: Style::empty().fg(Color::Named(NamedColor::Cyan)),
            messages_info_style: Style::empty(),
            messages_warn_style: Style::empty().fg(Color::Named(NamedColor::Yellow)).bold(),
            messages_error_style: Style::empty().fg(Color::Named(NamedColor::Red)).bold(),
            // T.4.b: diff sign styles + line/block tints now resolve
            // through the theme elements (`diff.*`), not this struct.
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

    // ---- Phase 5.8.AF.6 / issue-2: SyntaxStyle hoist ----

    #[test]
    fn syntax_style_keyword_carries_catppuccin_mauve_bold() {
        let t = Theme::default();
        let s = t.syntax_style(lattice_syntax::Style::Keyword);
        assert_eq!(s.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert!(s.modifiers.bold);
    }

    #[test]
    fn syntax_style_comment_is_overlay0_italic() {
        let t = Theme::default();
        let s = t.syntax_style(lattice_syntax::Style::Comment);
        assert_eq!(s.fg, Some(Color::Rgb(0x6c, 0x70, 0x86)));
        assert!(s.modifiers.italic);
        // Same shape for LineComment.
        let line = t.syntax_style(lattice_syntax::Style::LineComment);
        assert_eq!(line, s);
    }

    #[test]
    fn syntax_style_link_underlines() {
        let t = Theme::default();
        let s = t.syntax_style(lattice_syntax::Style::Link);
        assert!(s.modifiers.underline);
    }

    #[test]
    fn syntax_style_default_uses_text_foreground() {
        let t = Theme::default();
        let s = t.syntax_style(lattice_syntax::Style::Default);
        assert_eq!(s.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }

    #[test]
    fn syntax_style_returns_rgb_so_to_rgb_u32_is_lossless() {
        // GPUI adapter chain MUST round-trip without falling back
        // to the indexed/named lossy path -- otherwise the peer's
        // colour drifts from what host_theme declared.
        let t = Theme::default();
        let s = t.syntax_style(lattice_syntax::Style::String);
        let fg = s.fg.expect("string carries a foreground");
        assert!(matches!(fg, Color::Rgb(_, _, _)));
        assert_eq!(fg.to_rgb_u32(0), 0xa6e3a1);
    }
}
