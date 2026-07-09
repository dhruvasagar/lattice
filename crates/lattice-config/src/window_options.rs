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

    /// OS window chrome. `full` (default) keeps the system titlebar and
    /// controls. `none` removes them for a borderless window (as in
    /// alacritty `decorations = none` / kitty / emacs `undecorated`).
    /// Applied at window creation; a change takes effect on next launch.
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
}
