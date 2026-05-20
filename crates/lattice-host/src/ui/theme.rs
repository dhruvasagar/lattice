//! Renderer-neutral theme types.
//!
//! Mirrors the shape of `lattice-ui-tui::theme::Theme` but
//! expressed in terms a non-ratatui renderer can also consume.
//! Every field that the TUI theme expresses as `ratatui::Style`
//! becomes a [`Style`] here; every `ratatui::Color` becomes
//! a [`Color`].
//!
//! The TUI ships an adapter `From<&host::Theme>` for its
//! ratatui-typed `Theme`; the future GPUI renderer will do the
//! analogous conversion to its native `Hsla` + variable-font
//! style shape.
//!
//! ## Phase 5.3 status
//!
//! These types are introduced ALONGSIDE the existing TUI Theme.
//! `App` gains a `host_theme: Theme` field that
//! `sync_theme_from_config` keeps in sync with the TUI cache.
//! Render code is unchanged for this slice -- it keeps reading
//! from the TUI-typed `App.theme`. Future cleanup (when GPUI
//! ships) moves the cached TUI view off `App` and onto the
//! TUI runtime, leaving `App.theme` (renamed or repointed) as
//! the single canonical `host::Theme`.
//!
//! ## Default match contract
//!
//! Every field's [`Default`] value here MUST adapt (via
//! `lattice-ui-tui`'s `From<&host::Theme>` impl) to the same
//! ratatui `Style` / `Color` the TUI's `Theme::default()`
//! produces. A test in `lattice-ui-tui` pins this round-trip
//! so a drift in either default impl fails CI immediately.

/// One full UI theme. Cheap to clone (every field is `Copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub diagnostic_error_glyph: char,
    pub diagnostic_error_style: Style,
    pub diagnostic_warning_glyph: char,
    pub diagnostic_warning_style: Style,
    pub diagnostic_info_glyph: char,
    pub diagnostic_info_style: Style,
    pub diagnostic_hint_glyph: char,
    pub diagnostic_hint_style: Style,

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
}

/// A single style: optional foreground + optional background +
/// modifiers (bold/italic/etc). `None` for fg/bg means "do not
/// set this channel" (matches ratatui's empty-style semantics
/// and GPUI's `Style::transparent_black` background semantics).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: Modifiers,
}

impl Style {
    /// Style with no fg/bg/modifiers -- the renderer's "use my
    /// existing style." Equivalent to `ratatui::Style::default()`
    /// or `ratatui::Style::new()`.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    pub fn bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(mut self) -> Self {
        self.modifiers.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.modifiers.italic = true;
        self
    }

    pub fn underline(mut self) -> Self {
        self.modifiers.underline = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.modifiers.dim = true;
        self
    }

    pub fn reverse(mut self) -> Self {
        self.modifiers.reverse = true;
        self
    }
}

/// Text-attribute modifiers. Bools rather than bitflags so a new
/// modifier (strikethrough, blink, ...) is a struct-field add
/// instead of a flag-byte expansion; the renderers' adapter code
/// pattern-matches against the explicit field set rather than
/// chasing flag bits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Modifiers {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub reverse: bool,
}

/// Renderer-neutral color. The variants cover every shape any
/// terminal-or-GPU renderer ever needs: `Default` for "use the
/// terminal/window's default", `Named` for the 16 ANSI palette
/// names (TUI's 16-color fallback path), `Indexed` for the
/// 256-color palette, `Rgb` for 24-bit truecolor.
///
/// TUI renderer maps `Rgb` to `Indexed`-closest-match when the
/// terminal doesn't support truecolor. GPUI ignores `Named` /
/// `Indexed` lookups in palette-aware mode and reads `Rgb`
/// directly. The host owns the lossless form; each renderer
/// owns its own lossy-mapping at adapter time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Terminal / window default for this channel. Maps to
    /// `ratatui::Color::Reset`.
    Default,
    /// One of the 16 named ANSI colors. The TUI's primary
    /// palette path; GPUI maps these to its theme's named-color
    /// table.
    Named(NamedColor),
    /// 256-color palette index (xterm 256-color extension).
    Indexed(u8),
    /// 24-bit truecolor.
    Rgb(u8, u8, u8),
}

impl Color {
    /// Convert to a 24-bit `0xRRGGBB` packed `u32` for GPU-side
    /// renderers (which want raw truecolor, not the renderer-
    /// neutral [`Color`] enum). [`Color::Default`] returns
    /// `fallback` — the caller decides what "use the terminal /
    /// window default channel" means in pixel-space.
    ///
    /// Named colors map to canonical ANSI RGB values that match
    /// what xterm + most modern terminal emulators use. The
    /// indexed (xterm 256) path computes the 6×6×6 cube + the
    /// 24-step grayscale ramp standardly.
    ///
    /// Phase 5.8.K: GPUI peer's `GpuiTheme` rebuild reads
    /// host-themed colours through this helper so any palette
    /// change visible to the TUI also propagates to the window.
    /// Renderer-neutral; lives on `host_theme` because the
    /// `Color` enum is the canonical owner.
    pub fn to_rgb_u32(self, fallback: u32) -> u32 {
        use NamedColor as N;
        match self {
            Color::Default => fallback,
            Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
            Color::Named(n) => match n {
                N::Black => 0x000000,
                N::Red => 0xcd0000,
                N::Green => 0x00cd00,
                N::Yellow => 0xcdcd00,
                N::Blue => 0x0000ee,
                N::Magenta => 0xcd00cd,
                N::Cyan => 0x00cdcd,
                N::Gray => 0xe5e5e5,
                N::DarkGray => 0x7f7f7f,
                N::LightRed => 0xff0000,
                N::LightGreen => 0x00ff00,
                N::LightYellow => 0xffff00,
                N::LightBlue => 0x5c5cff,
                N::LightMagenta => 0xff00ff,
                N::LightCyan => 0x00ffff,
                N::White => 0xffffff,
            },
            Color::Indexed(idx) => indexed_to_rgb_u32(idx),
        }
    }
}

/// Map an xterm 256-colour index to a packed `0xRRGGBB` value.
/// - 0..=15: ANSI base colors (matches [`Color::Named`] mapping)
/// - 16..=231: 6×6×6 cube; each channel steps through
///   `[0, 95, 135, 175, 215, 255]`
/// - 232..=255: 24-step grayscale ramp from `0x080808` to
///   `0xeeeeee` in `+10` increments
fn indexed_to_rgb_u32(idx: u8) -> u32 {
    if idx < 16 {
        let names = [
            NamedColor::Black,
            NamedColor::Red,
            NamedColor::Green,
            NamedColor::Yellow,
            NamedColor::Blue,
            NamedColor::Magenta,
            NamedColor::Cyan,
            NamedColor::Gray,
            NamedColor::DarkGray,
            NamedColor::LightRed,
            NamedColor::LightGreen,
            NamedColor::LightYellow,
            NamedColor::LightBlue,
            NamedColor::LightMagenta,
            NamedColor::LightCyan,
            NamedColor::White,
        ];
        Color::Named(names[idx as usize]).to_rgb_u32(0)
    } else if idx < 232 {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let n = idx - 16;
        let r = STEPS[(n / 36) as usize];
        let g = STEPS[((n / 6) % 6) as usize];
        let b = STEPS[(n % 6) as usize];
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    } else {
        let level = 8 + 10 * (idx - 232) as u32;
        (level << 16) | (level << 8) | level
    }
}

/// The 16 named ANSI colors. Order matches ratatui's
/// `Color::Black..White` enumeration so the adapter is a
/// straightforward variant-by-variant match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
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
            diagnostic_error_style: Style::empty().fg(Color::Named(NamedColor::Red)).bold(),
            diagnostic_warning_glyph: '▲',
            diagnostic_warning_style: Style::empty().fg(Color::Named(NamedColor::Yellow)).bold(),
            diagnostic_info_glyph: '●',
            diagnostic_info_style: Style::empty().fg(Color::Named(NamedColor::Blue)),
            diagnostic_hint_glyph: '·',
            diagnostic_hint_style: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),

            whitespace_style: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),
            whitespace_trailing_style: Style::empty().fg(Color::Named(NamedColor::Red)),
            cursor_line_bg: Color::Indexed(236),

            messages_timestamp_style: Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim(),
            messages_trace_style: Style::empty().dim(),
            messages_debug_style: Style::empty().fg(Color::Named(NamedColor::Cyan)),
            messages_info_style: Style::empty(),
            messages_warn_style: Style::empty().fg(Color::Named(NamedColor::Yellow)).bold(),
            messages_error_style: Style::empty().fg(Color::Named(NamedColor::Red)).bold(),
        }
    }
}

/// Parse a user-typed color name into a host [`Color`]. Accepts
/// the 16 ANSI names (lowercase + dark-prefixed variants) plus
/// `default` / `reset` for terminal-default. Hex colors arrive
/// post-1.0 (depends on a terminal-true-color check at the
/// renderer side).
///
/// Mirrors the parse surface of `lattice-ui-tui::theme::parse_color`;
/// the TUI's path now goes through this function and adapts the
/// returned host `Color` into `ratatui::Color`.
pub fn parse_color(s: &str) -> Result<Color, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "default" | "reset" => Color::Default,
        "black" => Color::Named(NamedColor::Black),
        "red" => Color::Named(NamedColor::Red),
        "green" => Color::Named(NamedColor::Green),
        "yellow" => Color::Named(NamedColor::Yellow),
        "blue" => Color::Named(NamedColor::Blue),
        "magenta" => Color::Named(NamedColor::Magenta),
        "cyan" => Color::Named(NamedColor::Cyan),
        "gray" | "grey" | "white" => Color::Named(NamedColor::Gray),
        "darkgray" | "darkgrey" => Color::Named(NamedColor::DarkGray),
        "lightred" => Color::Named(NamedColor::LightRed),
        "lightgreen" => Color::Named(NamedColor::LightGreen),
        "lightyellow" => Color::Named(NamedColor::LightYellow),
        "lightblue" => Color::Named(NamedColor::LightBlue),
        "lightmagenta" => Color::Named(NamedColor::LightMagenta),
        "lightcyan" => Color::Named(NamedColor::LightCyan),
        other => return Err(format!("unknown color `{other}`")),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_color_named() {
        assert_eq!(parse_color("red").unwrap(), Color::Named(NamedColor::Red));
        assert_eq!(
            parse_color("DarkGray").unwrap(),
            Color::Named(NamedColor::DarkGray)
        );
        assert_eq!(parse_color("default").unwrap(), Color::Default);
    }

    // ---- 5.8.K: Color::to_rgb_u32 (host → GPU adapter) ----

    #[test]
    fn rgb_to_u32_packs_24_bit() {
        // 0xRRGGBB ordering. 0xff0000 = red, 0x00ff00 = green,
        // 0x0000ff = blue.
        assert_eq!(Color::Rgb(0xff, 0x00, 0x00).to_rgb_u32(0), 0xff0000);
        assert_eq!(Color::Rgb(0x00, 0xff, 0x00).to_rgb_u32(0), 0x00ff00);
        assert_eq!(Color::Rgb(0x00, 0x00, 0xff).to_rgb_u32(0), 0x0000ff);
        assert_eq!(Color::Rgb(0x12, 0x34, 0x56).to_rgb_u32(0), 0x123456);
    }

    #[test]
    fn default_color_returns_fallback() {
        // `Color::Default` means "use the terminal / window
        // default channel" — there's no truecolour answer, so we
        // hand back the caller's chosen fallback.
        assert_eq!(Color::Default.to_rgb_u32(0xdeadbe), 0xdeadbe);
        assert_eq!(Color::Default.to_rgb_u32(0), 0);
    }

    #[test]
    fn named_red_canonical_ansi_value() {
        // The 16 named ANSI colors map to standard xterm RGB.
        // Red == 0xcd0000 in the canonical xterm palette.
        assert_eq!(Color::Named(NamedColor::Red).to_rgb_u32(0), 0xcd0000);
        assert_eq!(Color::Named(NamedColor::White).to_rgb_u32(0), 0xffffff);
        assert_eq!(Color::Named(NamedColor::Black).to_rgb_u32(0), 0x000000);
    }

    #[test]
    fn indexed_below_16_matches_named() {
        // Indexed 0..=15 must agree with their Named equivalents
        // (callers should not see a discontinuity between the
        // 16-color named palette and the indexed-256 path).
        assert_eq!(
            Color::Indexed(1).to_rgb_u32(0),
            Color::Named(NamedColor::Red).to_rgb_u32(0)
        );
        assert_eq!(
            Color::Indexed(15).to_rgb_u32(0),
            Color::Named(NamedColor::White).to_rgb_u32(0)
        );
    }

    #[test]
    fn indexed_cube_corner_pure_black() {
        // Index 16 is the start of the 6×6×6 colour cube — pure
        // (0,0,0) black.
        assert_eq!(Color::Indexed(16).to_rgb_u32(0), 0x000000);
    }

    #[test]
    fn indexed_cube_corner_pure_white() {
        // Index 231 is the end of the cube — (255,255,255) white.
        assert_eq!(Color::Indexed(231).to_rgb_u32(0), 0xffffff);
    }

    #[test]
    fn indexed_grayscale_ramp() {
        // 232..=255 is a 24-step grey ramp from 0x080808 to
        // 0xeeeeee in +10 increments.
        assert_eq!(Color::Indexed(232).to_rgb_u32(0), 0x080808);
        assert_eq!(Color::Indexed(255).to_rgb_u32(0), 0xeeeeee);
    }

    #[test]
    fn parse_color_unknown_errors() {
        assert!(parse_color("rainbow").is_err());
    }

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
