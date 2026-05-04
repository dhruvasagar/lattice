//! Renderer-specific options for the TUI front-end. Registered
//! against the shared [`lattice_config::ConfigRegistry`] at App
//! startup so they show up in `:set <Tab>`, `:describe-option`,
//! and the (future) customize buffer alongside core options.
//!
//! What lives here vs. in `lattice-config::core_options`: an option
//! belongs here only if it's meaningless to a non-TUI renderer
//! (e.g. `ui.separator_color` -- a future GPU renderer wouldn't
//! draw an ASCII separator at all). Renderer-agnostic options
//! (`number`, `tabstop`, `foldmethod`, ...) live in
//! `lattice_config::core_options` and any renderer respects them.
//!
//! Storage shape: every option stores its current *primitive*
//! value (`bool` for the toggle, `String` for the color name and
//! the separator glyph) in the config. The derived [`Theme`]
//! `Style` values are kept on `App.theme` as a cached projection
//! and synced from config after every `:set` (see
//! `App::sync_theme_from_config`).

use lattice_config::{ConfigRegistry, Option, OptionHandle};

use crate::theme::parse_color;

/// Typed handles to every TUI-specific option. The App holds one of
/// these and reads via `config.get(handles.foo)` on hot paths --
/// or, for theme-derived `Style` values, reads the cached projection
/// on `App.theme` (refreshed via `sync_theme_from_config` after
/// `:set`).
pub struct TuiOptions {
    pub dim_inactive: OptionHandle<bool>,
    pub separator: OptionHandle<String>,
    pub separator_color: OptionHandle<String>,
    pub statusline_active_fg: OptionHandle<String>,
    pub statusline_inactive_fg: OptionHandle<String>,
}

/// Register every TUI-specific option against `registry` and hand
/// back the typed handle struct.
pub fn register_tui_options(registry: &ConfigRegistry) -> TuiOptions {
    let dim_inactive = registry.register(Option::<bool>::new(
        "ui.dim_inactive",
        true,
        "Apply a `DIM` overlay on inactive panes' syntax-highlighted text \
         so the active pane stands out without losing color.",
    ));
    let separator = registry.register(
        Option::<String>::builder(
            "ui.separator",
            "│".into(),
            "Character drawn in the column separating side-by-side panes (default `│`).",
        )
        .validate(|s| {
            if s.chars().count() == 1 {
                Ok(())
            } else {
                Err(format!(
                    "ui.separator must be one character, got `{s}` ({} chars)",
                    s.chars().count()
                ))
            }
        })
        .build(),
    );
    let separator_color = registry.register(
        Option::<String>::builder(
            "ui.separator_color",
            "darkgray".into(),
            "Foreground color of the pane separator. Accepts named ANSI colors \
             (red, blue, darkgray, ...) and `default` for the terminal default.",
        )
        .validate(|s| parse_color(s).map(|_| ()))
        .build(),
    );
    let statusline_active_fg = registry.register(
        Option::<String>::builder(
            "ui.statusline_active_fg",
            "default".into(),
            "Foreground color of the active pane's status line.",
        )
        .validate(|s| parse_color(s).map(|_| ()))
        .build(),
    );
    let statusline_inactive_fg = registry.register(
        Option::<String>::builder(
            "ui.statusline_inactive_fg",
            "darkgray".into(),
            "Foreground color of inactive panes' status lines.",
        )
        .validate(|s| parse_color(s).map(|_| ()))
        .build(),
    );
    TuiOptions {
        dim_inactive,
        separator,
        separator_color,
        statusline_active_fg,
        statusline_inactive_fg,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn register_tui_options_returns_handles_to_all_ui_options() {
        let r = ConfigRegistry::new();
        let h = register_tui_options(&r);
        assert!(*r.get(h.dim_inactive));
        assert_eq!(*r.get(h.separator), "│");
        assert_eq!(*r.get(h.separator_color), "darkgray");
        assert_eq!(*r.get(h.statusline_active_fg), "default");
        assert_eq!(*r.get(h.statusline_inactive_fg), "darkgray");
    }

    #[test]
    fn separator_validate_rejects_multi_char_strings() {
        let r = ConfigRegistry::new();
        register_tui_options(&r);
        let err = r.parse_and_set_command("ui.separator=ab").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must be one character"), "got `{msg}`");
    }

    #[test]
    fn separator_color_validate_rejects_invalid_color_names() {
        let r = ConfigRegistry::new();
        register_tui_options(&r);
        let err = r
            .parse_and_set_command("ui.separator_color=puce")
            .unwrap_err();
        let msg = format!("{err}");
        // theme::parse_color's error wording: "unknown color: puce".
        assert!(msg.contains("puce"), "got `{msg}`");
    }
}
