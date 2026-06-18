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

use lattice_host::ui::theme as host_theme;

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

    /// Style for directory entries in file-tree and oil buffers.
    pub file_tree_dir_style: Style,
    /// Style for hidden files (names starting with `.`).
    pub file_tree_hidden_style: Style,
    /// Base style for regular file entries.
    pub file_tree_file_style: Style,
    /// Whether to render file-type icons as Nerd Fonts v3 glyphs.
    /// When false, the icons module emits the BMP-block fallback
    /// palette (◆ ≡ ◇ ■ ♪ ▶ ·) that works in every modern monospace
    /// font. Synced from the `ui.nerd_fonts` typed option in
    /// `App::sync_theme_from_config`.
    pub nerd_fonts: bool,

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

    // ---- M.7.3 whitespace decoration ---------------------
    /// Style applied to "neutral" whitespace glyphs (tab,
    /// leading, mid-text space, EOL). Default: dim dark-gray --
    /// visible enough to read structure, quiet enough to not
    /// fight the syntax highlight. Trailing whitespace gets a
    /// louder style ([`Self::whitespace_trailing_style`]); they
    /// split because trailing is a lint signal where the others
    /// are structural.
    pub whitespace_style: Style,
    /// Style for trailing-whitespace glyphs. Default: red,
    /// no modifier -- "this shouldn't be here" without
    /// shouting.
    pub whitespace_trailing_style: Style,

    // ---- M.7.3 current-line highlight --------------------
    /// Background applied to the cursor's row when
    /// `current-line-highlight-mode` is active (M.7.2 minor /
    /// `:set cursorline`). Default: a subtle dark gray
    /// (`Color::Indexed(236)` -- the conventional darker-than-
    /// background row tint in 256-color palettes). Active pane
    /// only; selection bg wins per-cell when the two overlap.
    pub cursor_line_bg: Color,

    // ---- msg-mode.3: `*messages*` level highlights -------
    /// Style for the timestamp prefix (`HH:MM:SS.mmm`) at the
    /// start of every `*messages*` row. Dim by default so the
    /// time doesn't compete with the level + body for
    /// attention.
    pub messages_timestamp_style: Style,
    /// Style for the `TRACE` level token. Dim — `trace`-class
    /// records are firehose-y; the user opts in via
    /// `messages.filter` and shouldn't have them shout.
    pub messages_trace_style: Style,
    /// Style for the `DEBUG` level token. Cyan: distinct from
    /// info but not alarming.
    pub messages_debug_style: Style,
    /// Style for the `INFO` level token. Default: terminal-
    /// default fg, no modifier — neutral.
    pub messages_info_style: Style,
    /// Style for the `WARN` level token. Yellow + bold.
    pub messages_warn_style: Style,
    /// Style for the `ERROR` level token. Red + bold.
    pub messages_error_style: Style,

    // ---- Diff (D.3.b.3 2026-05-29) ---------------------------
    /// Glyph color for the `+` sign in the diff gutter column
    /// (D.3.d.1). The glyph itself is hardcoded `+`; this style
    /// supplies its colour.
    pub diff_add_sign_style: Style,
    /// Glyph color for the `~` sign in the diff gutter column.
    pub diff_change_sign_style: Style,
    /// Glyph color for the `-` sign in the diff gutter column.
    /// D.3.d.0 doesn't currently emit Remove on the current
    /// side (deletion blocks surface removes), but the entry
    /// is reserved for future renderers.
    pub diff_remove_sign_style: Style,
    /// D.6.f (2026-05-31): glyph colour for the `?` sign in
    /// the diff gutter — three-way Conflict hunks.
    pub diff_conflict_sign_style: Style,
    /// Background tint applied to lines added in the current
    /// side (D.3.e).
    pub diff_add_line_bg: Color,
    /// Background tint applied to lines changed in the current
    /// side (D.3.e).
    pub diff_change_line_bg: Color,
    /// Background tint applied to deletion-block virtual rows
    /// (D.3.b.1 / D.3.b.2). Sits behind the baseline-text cells
    /// in the deletion block.
    pub diff_deletion_block_bg: Color,
    /// D.6.f (2026-05-31): background tint for three-way
    /// conflict regions. Visually distinct from
    /// Add/Change/Remove tints so users spot conflicts at a
    /// glance even when the gutter glyph is off-screen.
    pub diff_conflict_line_bg: Color,
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
            diagnostic_warning_style: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            diagnostic_info_glyph: '●',
            diagnostic_info_style: Style::new().fg(Color::Blue),
            diagnostic_hint_glyph: '·',
            diagnostic_hint_style: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            file_tree_dir_style: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            file_tree_hidden_style: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            file_tree_file_style: Style::new(),
            // Default to the BMP fallback so the first frame works
            // in any terminal font. Users on a Nerd-Font-patched
            // terminal opt in via `:set ui.nerd_fonts on`.
            nerd_fonts: false,
            // M.7.3: whitespace + current-line defaults.
            whitespace_style: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            whitespace_trailing_style: Style::new().fg(Color::Red),
            cursor_line_bg: Color::Indexed(236),
            // msg-mode.3: matches the format produced by
            // `crate::app::messages::format_message_record`.
            messages_timestamp_style: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            messages_trace_style: Style::new().add_modifier(Modifier::DIM),
            messages_debug_style: Style::new().fg(Color::Cyan),
            messages_info_style: Style::new(),
            messages_warn_style: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            messages_error_style: Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),

            // D.3.b.3 (2026-05-29): defaults mirror
            // `host_theme::Theme::default()` Diff section.
            diff_add_sign_style: Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            diff_change_sign_style: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            diff_remove_sign_style: Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            diff_conflict_sign_style: Style::new()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            diff_add_line_bg: Color::Rgb(0, 50, 0),
            diff_change_line_bg: Color::Rgb(50, 50, 0),
            diff_deletion_block_bg: Color::Rgb(60, 0, 0),
            diff_conflict_line_bg: Color::Rgb(60, 0, 60),
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

/// Parse a user-typed color name into a ratatui [`Color`].
///
/// Phase 5.3: delegates to `lattice_host::ui::theme::parse_color`
/// (the canonical parser) and converts the host [`host_theme::Color`]
/// into a ratatui [`Color`]. The validation surface (accepted
/// names, error format) stays identical so `:set ui.*_color=...`
/// behaves the same. Hex colors arrive post-1.0 (depends on a
/// terminal-true-color check).
pub fn parse_color(s: &str) -> Result<Color, String> {
    host_theme::parse_color(s).map(host_color_to_ratatui)
}

/// Adapt a renderer-neutral [`host_theme::Color`] into a ratatui
/// [`Color`]. Lossless on all variants except `Rgb` when the
/// terminal doesn't support truecolor -- in that case the
/// renderer's frame submission stage handles the lossy fallback
/// (ratatui already does palette closest-match itself).
pub fn host_color_to_ratatui(c: host_theme::Color) -> Color {
    use host_theme::NamedColor as N;
    match c {
        host_theme::Color::Default => Color::Reset,
        host_theme::Color::Named(N::Black) => Color::Black,
        host_theme::Color::Named(N::Red) => Color::Red,
        host_theme::Color::Named(N::Green) => Color::Green,
        host_theme::Color::Named(N::Yellow) => Color::Yellow,
        host_theme::Color::Named(N::Blue) => Color::Blue,
        host_theme::Color::Named(N::Magenta) => Color::Magenta,
        host_theme::Color::Named(N::Cyan) => Color::Cyan,
        host_theme::Color::Named(N::Gray) => Color::Gray,
        host_theme::Color::Named(N::DarkGray) => Color::DarkGray,
        host_theme::Color::Named(N::LightRed) => Color::LightRed,
        host_theme::Color::Named(N::LightGreen) => Color::LightGreen,
        host_theme::Color::Named(N::LightYellow) => Color::LightYellow,
        host_theme::Color::Named(N::LightBlue) => Color::LightBlue,
        host_theme::Color::Named(N::LightMagenta) => Color::LightMagenta,
        host_theme::Color::Named(N::LightCyan) => Color::LightCyan,
        host_theme::Color::Named(N::White) => Color::White,
        host_theme::Color::Indexed(idx) => Color::Indexed(idx),
        host_theme::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Adapt a renderer-neutral [`host_theme::Style`] into a ratatui
/// [`Style`]. Empty fg/bg map to "unset" on the ratatui side
/// (same as `Style::default()`); modifiers chain via
/// `add_modifier`.
pub fn host_style_to_ratatui(s: host_theme::Style) -> Style {
    let mut style = Style::default();
    if let Some(fg) = s.fg {
        style = style.fg(host_color_to_ratatui(fg));
    }
    if let Some(bg) = s.bg {
        style = style.bg(host_color_to_ratatui(bg));
    }
    if s.modifiers.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.modifiers.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.modifiers.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if s.modifiers.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    if s.modifiers.reverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// Build the ratatui-typed [`Theme`] cache from the canonical host
/// [`host_theme::Theme`] **plus** the resolved theme table. The TUI's
/// `App.theme` cache is the renderer's hot-path adapted view; it is
/// rebuilt at theme-change / option-set rate, never per frame.
///
/// T.4 (theme-system): migrated *style* elements now source from the
/// [`host_theme::ResolvedTheme`] table (the single source of truth) via
/// the captured [`host_theme::BuiltinElementIds`], rather than from the
/// flat `Theme` struct. The cache itself stays — it is the safeguard
/// that keeps per-line decoration painting free of per-frame style
/// adaptation (design §10.1; `feedback_renderer_cache_protects_ux`).
/// Un-migrated fields still read from `h`; each moves to `resolved` as
/// its Thread-B slice lands.
pub fn build_tui_theme(
    h: &host_theme::Theme,
    resolved: &host_theme::ResolvedTheme,
    ids: &host_theme::BuiltinElementIds,
) -> Theme {
    let resolved_style = |id| host_style_to_ratatui(resolved.get(id));
    // Background tint from an element's resolved `bg` channel (the diff
    // tint elements set `bg`); `Reset` = no tint if unresolved.
    let resolved_bg = |id| {
        resolved
            .get(id)
            .bg
            .map(host_color_to_ratatui)
            .unwrap_or(Color::Reset)
    };
    Theme {
            pane_status_active: host_style_to_ratatui(h.pane_status_active),
            pane_status_inactive: host_style_to_ratatui(h.pane_status_inactive),
            inactive_pane_overlay: host_style_to_ratatui(h.inactive_pane_overlay),
            dim_inactive_panes: h.dim_inactive_panes,
            pane_separator: host_style_to_ratatui(h.pane_separator),
            pane_separator_vertical: h.pane_separator_vertical,
            pane_separator_horizontal: h.pane_separator_horizontal,
            file_tree_dir_style: host_style_to_ratatui(h.file_tree_dir_style),
            file_tree_hidden_style: host_style_to_ratatui(h.file_tree_hidden_style),
            file_tree_file_style: host_style_to_ratatui(h.file_tree_file_style),
            nerd_fonts: h.nerd_fonts,
            // T.4.a: diagnostic styles source from the resolved table
            // (`diagnostic.{error,warning,info,hint}`); the glyphs stay
            // on the host `Theme` until T.6.t.
            diagnostic_error_glyph: h.diagnostic_error_glyph,
            diagnostic_error_style: resolved_style(ids.diagnostic_error),
            diagnostic_warning_glyph: h.diagnostic_warning_glyph,
            diagnostic_warning_style: resolved_style(ids.diagnostic_warning),
            diagnostic_info_glyph: h.diagnostic_info_glyph,
            diagnostic_info_style: resolved_style(ids.diagnostic_info),
            diagnostic_hint_glyph: h.diagnostic_hint_glyph,
            diagnostic_hint_style: resolved_style(ids.diagnostic_hint),
            whitespace_style: host_style_to_ratatui(h.whitespace_style),
            whitespace_trailing_style: host_style_to_ratatui(h.whitespace_trailing_style),
            cursor_line_bg: host_color_to_ratatui(h.cursor_line_bg),
            messages_timestamp_style: host_style_to_ratatui(h.messages_timestamp_style),
            messages_trace_style: host_style_to_ratatui(h.messages_trace_style),
            messages_debug_style: host_style_to_ratatui(h.messages_debug_style),
            messages_info_style: host_style_to_ratatui(h.messages_info_style),
            messages_warn_style: host_style_to_ratatui(h.messages_warn_style),
            messages_error_style: host_style_to_ratatui(h.messages_error_style),
            // T.4.b: diff signs (fg) + line/block tints (bg) source
            // from the resolved table (`diff.*`).
            diff_add_sign_style: resolved_style(ids.diff_add_sign),
            diff_change_sign_style: resolved_style(ids.diff_change_sign),
            diff_remove_sign_style: resolved_style(ids.diff_remove_sign),
            diff_conflict_sign_style: resolved_style(ids.diff_conflict_sign),
            diff_add_line_bg: resolved_bg(ids.diff_add_line),
            diff_change_line_bg: resolved_bg(ids.diff_change_line),
            diff_deletion_block_bg: resolved_bg(ids.diff_deletion_block),
            diff_conflict_line_bg: resolved_bg(ids.diff_conflict_line),
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

    #[test]
    fn host_theme_default_adapts_to_tui_theme_default() {
        // Phase 5.3 contract + T.4: the host default theme, run
        // through the cache builder (host fields + the resolved
        // default table for migrated style elements), builds the
        // same ratatui `Theme::default()` the TUI hand-rolls. This
        // pins the per-element parity net on the TUI side — the
        // resolved-sourced diagnostic styles must equal the legacy
        // ratatui defaults. Superseded by the per-element parity pin
        // when host `Theme` is dismantled (T.6.t).
        use host_theme::ThemeRegistry as _;
        let host = host_theme::Theme::default();
        let reg = host_theme::InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = host_theme::BuiltinElementIds::capture(&reg);
        let built = build_tui_theme(&host, &resolved, &ids);
        let tui: Theme = Theme::default();
        assert_eq!(
            built, tui,
            "host theme default + resolved table must build the TUI theme default"
        );
    }

    #[test]
    fn parse_color_routes_through_host() {
        // The TUI parser delegates to the host parser. Pin the
        // observable behaviour: same string → equivalent
        // ratatui Color.
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert_eq!(parse_color("default").unwrap(), Color::Reset);
        assert_eq!(parse_color("DarkGray").unwrap(), Color::DarkGray);
        assert!(parse_color("rainbow").is_err());
    }
}
