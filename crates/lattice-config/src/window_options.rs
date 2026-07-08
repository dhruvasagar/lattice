// `linkme`'s distributed slices use `link_section` to aggregate
// items at link time. The `options!` macro expansion in this file
// emits such a declaration; allow the workspace's `unsafe_code =
// "deny"` lint locally with the same safety rationale documented in
// `option_decl.rs`, `group.rs`, and `core_options.rs`.
#![allow(unsafe_code)]

//! GPUI window options (`ui.window.*`). GPUI peer only; the TUI never reads
//! these. `decorations` is applied at window creation; `start-maximized`
//! drives a one-shot maximize on launch. See
//! `docs/dev/architecture/gpui-window-chrome.md`.

use crate::Decorations;

crate::options! {
    group = crate::Window;

    /// OS window chrome (GPUI only; ignored by the terminal UI). One of:
    ///   `full` (default) — system titlebar + window controls.
    ///   `none` — borderless: no titlebar or controls, like alacritty
    ///     `decorations = none` / kitty / emacs `undecorated`. On macOS a
    ///     `none` window is non-resizable by any means — even Raycast/yabai
    ///     can't move or resize it; it opens at a fixed size.
    ///   `transparent` — frameless-looking but still resizable: a transparent
    ///     titlebar with the traffic-light buttons hidden. Edge-resize works and
    ///     window managers (Raycast/yabai) can drive it; rounded corners + shadow
    ///     remain. The macOS-friendly frameless option.
    /// Applied at window creation; a change takes effect on the next launch.
    #[name("ui.window.decorations")]
    pub WindowDecorationsOption: Decorations = Decorations::Full;

    /// Maximize the window on launch (fill the work area, keep the menu
    /// bar — not native fullscreen). GPUI peer only; ignored by the TUI.
    #[name("ui.window.start-maximized")]
    pub StartMaximized: bool = false;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use crate::{ConfigRegistry, Decorations, StartMaximized, WindowDecorationsOption};

    fn reg() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r
    }

    #[test]
    fn defaults_match_spec() {
        let r = reg();
        assert_eq!(*r.get_typed::<WindowDecorationsOption>().unwrap(), Decorations::Full);
        assert!(!*r.get_typed::<StartMaximized>().unwrap());
    }

    #[test]
    fn set_decorations_none_parses() {
        let r = reg();
        r.parse_and_set_command("ui.window.decorations=none").unwrap();
        assert_eq!(*r.get_typed::<WindowDecorationsOption>().unwrap(), Decorations::None_);
    }

    #[test]
    fn set_start_maximized_true_parses() {
        let r = reg();
        r.parse_and_set_command("ui.window.start-maximized=true").unwrap();
        assert!(*r.get_typed::<StartMaximized>().unwrap());
    }

    #[test]
    fn bad_decorations_value_errors() {
        let r = reg();
        assert!(r.parse_and_set_command("ui.window.decorations=wat").is_err());
    }

    // Regression: the `ui.window.*` options must resolve through the real TOML
    // FILE path (load_file -> walk_table -> apply_scalar), not just the `:set`
    // path — a `[ui.window]` table with hyphenated leaf keys must apply. This is
    // the boundary a maximize-on-launch bug report pointed at (the read was fine;
    // the bug was the apply mechanism).
    #[test]
    fn toml_file_path_applies_ui_window_table() {
        let r = reg();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lattice_probe_{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[ui.window]\ndecorations = \"none\"\nstart-maximized = true\n",
        )
        .unwrap();
        let out = crate::load_file(&r, &path, &[]);
        std::fs::remove_file(&path).ok();
        assert!(
            out.messages.is_empty(),
            "load messages: {:?}",
            out.messages
        );
        assert_eq!(
            *r.get_typed::<WindowDecorationsOption>().unwrap(),
            Decorations::None_,
            "decorations from file"
        );
        assert!(
            *r.get_typed::<StartMaximized>().unwrap(),
            "start-maximized from file"
        );
    }
}
