//! UI theme (DESIGN.md §5.6, §5.12).
//!
//! Holds the customizable styling knobs the renderer reads each
//! frame. v1 ships a built-in default that matches vim's classic
//! split visuals (active status line reverse-videoed, inactive
//! dim, vertical separator with `│`); every field is exposed via
//! `:set ui.*` options so a user / config layer can override it.
//!
//! Adding a new themable surface is two edits: add a field to
//! [`Theme`] with its default + add an `OptionSpec` in
//! `crate::options::builtin_options()` that mutates that field.

use ratatui::style::{Color, Modifier, Style};

/// One full UI theme. Cheap to clone (Style + char fields are all
/// `Copy`); the App holds it directly and `:set ui.*` writes
/// through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Per-pane status line style for the active pane. Default
    /// reverse-video + bold so focus is unambiguous regardless of
    /// the user's terminal palette.
    pub pane_status_active: Style,
    /// Per-pane status line style for inactive panes. Default
    /// dim + dark-gray foreground so the line is visible but
    /// clearly secondary.
    pub pane_status_inactive: Style,
    /// Style applied to every span in an inactive document pane
    /// when [`Self::dim_inactive_panes`] is true. Composes with
    /// the existing syntax-highlight style. Default: `DIM`
    /// modifier only -- preserves color, just reduces intensity.
    pub inactive_pane_overlay: Style,
    /// Whether inactive panes get [`Self::inactive_pane_overlay`]
    /// applied on top of their syntax highlights. Off → inactive
    /// panes look identical to active (just no terminal cursor).
    pub dim_inactive_panes: bool,
    /// Style for the vertical-split separator column (`│` between
    /// side-by-side panes). Default: dark gray foreground, no bg.
    pub pane_separator: Style,
    /// Character drawn in the vertical-split separator column.
    /// Default: `│` (U+2502, BOX DRAWINGS LIGHT VERTICAL).
    pub pane_separator_vertical: char,
    /// Character drawn in the horizontal-split separator row.
    /// Default: `─` (U+2500). Currently unused -- horizontal
    /// splits are visually delimited by the per-pane status line
    /// at the bottom of the upper pane. Reserved for layouts
    /// that disable per-pane status lines.
    pub pane_separator_horizontal: char,

    // ---- Diagnostics (Phase 4.1.d.iii) ---------------------

    /// Glyph + color for an Error-severity diagnostic. Rendered
    /// in the gutter's severity column; also drives the inline
    /// underline color when an error range overlaps text.
    pub diagnostic_error_glyph: char,
    pub diagnostic_error_style: Style,
    /// Warning severity.
    pub diagnostic_warning_glyph: char,
    pub diagnostic_warning_style: Style,
    /// Information severity.
    pub diagnostic_info_glyph: char,
    pub diagnostic_info_style: Style,
    /// Hint severity.
    pub diagnostic_hint_glyph: char,
    pub diagnostic_hint_style: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            pane_status_active: Style::new()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
            pane_status_inactive: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            inactive_pane_overlay: Style::new().add_modifier(Modifier::DIM),
            dim_inactive_panes: true,
            pane_separator: Style::new().fg(Color::DarkGray),
            pane_separator_vertical: '│',
            pane_separator_horizontal: '─',
            // Severity glyphs: solid square / triangle / circle /
            // dot. Same shapes vim's nvim-lsp / VS Code use --
            // immediately readable, terminal-safe.
            diagnostic_error_glyph: '■',
            diagnostic_error_style: Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            diagnostic_warning_glyph: '▲',
            diagnostic_warning_style: Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            diagnostic_info_glyph: '●',
            diagnostic_info_style: Style::new().fg(Color::Blue),
            diagnostic_hint_glyph: '·',
            diagnostic_hint_style: Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        }
    }
}

/// Resolve the per-severity rendering bits from the theme.
/// Returns `(glyph, style)`.
pub fn diagnostic_glyph_and_style(
    theme: &Theme,
    severity: lattice_lsp::DiagnosticSeverity,
) -> (char, Style) {
    match severity {
        lattice_lsp::DiagnosticSeverity::ERROR => {
            (theme.diagnostic_error_glyph, theme.diagnostic_error_style)
        }
        lattice_lsp::DiagnosticSeverity::WARNING => (
            theme.diagnostic_warning_glyph,
            theme.diagnostic_warning_style,
        ),
        lattice_lsp::DiagnosticSeverity::INFORMATION => {
            (theme.diagnostic_info_glyph, theme.diagnostic_info_style)
        }
        lattice_lsp::DiagnosticSeverity::HINT => {
            (theme.diagnostic_hint_glyph, theme.diagnostic_hint_style)
        }
        _ => (theme.diagnostic_info_glyph, theme.diagnostic_info_style),
    }
}

/// Parse a user-typed color name into a ratatui [`Color`]. Accepts
/// the 16 ANSI names (lowercase + dark-prefixed variants) plus
/// `default` / `reset` for terminal-default. Hex colors arrive
/// post-1.0 (depends on a terminal-true-color check).
pub fn parse_color(s: &str) -> Result<Color, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "default" | "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" | "white" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        other => return Err(format!("unknown color `{other}`")),
    })
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

    #[test]
    fn parse_color_named() {
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert_eq!(parse_color("DarkGray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("default").unwrap(), Color::Reset);
    }

    #[test]
    fn parse_color_unknown_errors() {
        assert!(parse_color("rainbow").is_err());
    }
}
