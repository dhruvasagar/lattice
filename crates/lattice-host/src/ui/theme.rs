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
}
