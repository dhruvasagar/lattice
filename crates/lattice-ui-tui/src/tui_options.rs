// `linkme`'s distributed slices use `link_section` to aggregate
// items at link time. The macro expansions in this file emit
// such declarations; allow the workspace's `unsafe_code = "deny"`
// lint locally with the same safety rationale documented in
// `lattice-config`'s `option_decl.rs` etc.
#![allow(unsafe_code)]

//! Renderer-specific options for the TUI front-end. M.2.0c
//! migrates these from the imperative `Option::builder()` form to
//! the proc-macro form (Design B + D from
//! `mode-architecture.md`'s discussion). Self-registration via
//! `linkme` -- the registry's `init_from_linkme` walks the slice
//! at App boot and registers each option.
//!
//! What lives here vs. in `lattice_config::core_options`: an
//! option belongs here only if it's meaningless to a non-TUI
//! renderer (e.g. `ui.separator_color` -- a future GPU renderer
//! wouldn't draw an ASCII separator at all). Renderer-agnostic
//! options (`number`, `tabstop`, `foldmethod`, ...) live in
//! `lattice_config::core_options` and any renderer respects
//! them.
//!
//! Storage shape: every option stores its current *primitive*
//! value (`bool` for the toggle, `String` for the color name
//! and the separator glyph) in the config. The derived
//! [`crate::theme::Theme`] `Style` values are kept on
//! `App.theme` as a cached projection and synced from config
//! after every `:set` (see `App::sync_theme_from_config`).

use crate::theme::parse_color;

// Validators referenced by `#[validate(...)]` attributes below.
fn validate_separator(s: &String) -> Result<(), String> {
    if s.chars().count() == 1 {
        Ok(())
    } else {
        Err(format!(
            "ui.separator must be one character, got `{s}` ({} chars)",
            s.chars().count()
        ))
    }
}

fn validate_color(s: &String) -> Result<(), String> {
    parse_color(s).map(|_| ())
}

// Group binding: TUI-specific UI options under the `appearance`
// group (theme / colors / sprite icons). User customizes via
// `:customize appearance`.
lattice_config::options! {
    group = lattice_config::Appearance;

    /// Apply a `DIM` overlay on inactive panes' syntax-highlighted
    /// text so the active pane stands out without losing color.
    #[name("ui.dim_inactive")]
    pub UiDimInactive: bool = true;

    /// Character drawn in the column separating side-by-side
    /// panes (default `│`).
    #[name("ui.separator")]
    #[validate(validate_separator)]
    pub UiSeparator: String = String::from("│");

    /// Foreground color of the pane separator. Accepts named ANSI
    /// colors (red, blue, darkgray, ...) and `default` for the
    /// terminal default.
    #[name("ui.separator_color")]
    #[validate(validate_color)]
    pub UiSeparatorColor: String = String::from("darkgray");

    /// Foreground color of the active pane's status line.
    #[name("ui.statusline_active_fg")]
    #[validate(validate_color)]
    pub UiStatuslineActiveFg: String = String::from("default");

    /// Foreground color of inactive panes' status lines.
    #[name("ui.statusline_inactive_fg")]
    #[validate(validate_color)]
    pub UiStatuslineInactiveFg: String = String::from("darkgray");

    /// Whether to render file-type icons as Nerd Fonts v3 glyphs.
    ///
    /// `true` -- expects a Nerd Font (FiraCode Nerd Font, JetBrains
    /// Mono Nerd Font, Hack Nerd Font, ...) configured as the
    /// terminal font; produces the rich per-language icon set.
    ///
    /// `false` (default) -- renders a BMP-block fallback palette
    /// (◆ ≡ ◇ ■ ♪ ▶ ·) that works in every modern monospace font.
    /// Pick this if you see `?` boxes in the file tree / oil.
    #[name("ui.nerd_fonts")]
    pub UiNerdFonts: bool = false;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_config::ConfigRegistry;

    #[test]
    fn type_keyed_reads_work_post_boot() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert!(*r.get_typed::<UiDimInactive>().unwrap());
        assert_eq!(r.get_typed::<UiSeparator>().unwrap().as_str(), "│");
        assert_eq!(r.get_typed::<UiSeparatorColor>().unwrap().as_str(), "darkgray");
        assert_eq!(r.get_typed::<UiStatuslineActiveFg>().unwrap().as_str(), "default");
        assert_eq!(
            r.get_typed::<UiStatuslineInactiveFg>().unwrap().as_str(),
            "darkgray"
        );
        // BMP fallback is the default so the first frame renders
        // in any terminal font.
        assert!(!*r.get_typed::<UiNerdFonts>().unwrap());
    }

    #[test]
    fn nerd_fonts_flag_parses_on_off() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r.parse_and_set_command("ui.nerd_fonts=on").unwrap();
        assert!(*r.get_typed::<UiNerdFonts>().unwrap());
        r.parse_and_set_command("ui.nerd_fonts=off").unwrap();
        assert!(!*r.get_typed::<UiNerdFonts>().unwrap());
    }

    #[test]
    fn separator_validate_rejects_multi_char_strings() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        let err = r.parse_and_set_command("ui.separator=ab").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must be one character"), "got `{msg}`");
    }

    #[test]
    fn separator_color_validate_rejects_invalid_color_names() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        let err = r
            .parse_and_set_command("ui.separator_color=puce")
            .unwrap_err();
        let msg = format!("{err}");
        // theme::parse_color's error wording: "unknown color: puce".
        assert!(msg.contains("puce"), "got `{msg}`");
    }
}
