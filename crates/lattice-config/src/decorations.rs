//! Value type for the `ui.window.decorations` typed option.
//!
//! Controls OS window chrome on the GPUI peer: `full` (default) keeps the
//! system titlebar + controls; `none` requests a borderless window (as in
//! alacritty `decorations = none` / kitty / emacs `undecorated`); `transparent`
//! is frameless-looking but stays resizable. Pure
//! presentation policy read only by the GPUI renderer — like [`crate::SignColumn`]
//! the value type lives here and impls [`OptionType`] locally. The TUI never
//! reads it. See `docs/dev/architecture/gpui-window-chrome.md`.

use crate::option_type::{EnumeratedValue, OptionType};

/// `ui.window.decorations` — OS window chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decorations {
    /// System titlebar + controls (the default; today's behavior).
    #[default]
    Full,
    /// Borderless: no titlebar / controls. `None_` avoids the `Option::None`
    /// name clash; the on-disk / `:set` label is `none`. On macOS this window is
    /// non-resizable (see the design fragment); use `transparent` if you need
    /// the window to stay resizable / controllable by Raycast/yabai.
    None_,
    /// Frameless-looking but still resizable: a transparent, full-size-content
    /// titlebar with the traffic-light buttons hidden (on macOS, via `setHidden:`
    /// after the window opens — NOT moved off-screen, which would break the
    /// window's Accessibility geometry). Unlike `none` the window keeps
    /// `NSResizableWindowMask` on macOS, so edge-resize and external window
    /// managers (Raycast/yabai) work. Keeps rounded corners + shadow. The
    /// on-disk / `:set` label is `transparent`.
    Transparent,
}

impl Decorations {
    pub fn label(&self) -> &'static str {
        match self {
            Decorations::Full => "full",
            Decorations::None_ => "none",
            Decorations::Transparent => "transparent",
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            Decorations::Full => "System titlebar and window controls (default)",
            Decorations::None_ => "Borderless window: no titlebar or controls (non-resizable on macOS)",
            Decorations::Transparent => {
                "Frameless-looking but resizable: transparent titlebar, hidden controls"
            }
        }
    }

    /// True when the window is drawn without OS chrome AND is non-resizable
    /// (the macOS `titlebar: None` case). `transparent` looks frameless but
    /// stays resizable, so it is NOT borderless by this predicate — the
    /// maximize path relies on this to decide whether zoom works.
    pub fn is_borderless(&self) -> bool {
        matches!(self, Decorations::None_)
    }

    pub fn all() -> [Decorations; 3] {
        [
            Decorations::Full,
            Decorations::None_,
            Decorations::Transparent,
        ]
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "full" => Ok(Decorations::Full),
            "none" => Ok(Decorations::None_),
            "transparent" => Ok(Decorations::Transparent),
            other => Err(format!(
                "ui.window.decorations: expected `full`, `none`, or `transparent`, got `{other}`"
            )),
        }
    }
}

impl OptionType for Decorations {
    fn parse(s: &str) -> Result<Self, String> {
        Decorations::parse_label(s)
    }
    fn format(&self) -> String {
        self.label().to_string()
    }
    fn type_label() -> &'static str {
        "decorations"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(Decorations::all().iter().map(|v| v.label()).collect())
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            Decorations::all()
                .iter()
                .map(|v| EnumeratedValue { form: v.label(), doc: v.doc() })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_full_not_borderless() {
        assert_eq!(Decorations::default(), Decorations::Full);
        assert!(!Decorations::default().is_borderless());
    }

    #[test]
    fn parse_round_trips_every_value() {
        for v in Decorations::all() {
            assert_eq!(Decorations::parse_label(v.label()).unwrap(), v);
            assert_eq!(Decorations::parse(v.label()).unwrap(), v);
            assert_eq!(v.format(), v.label());
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(Decorations::parse_label("buttonless").is_err());
        assert!(Decorations::parse_label("true").is_err());
    }

    #[test]
    fn none_is_borderless() {
        assert!(Decorations::None_.is_borderless());
    }

    #[test]
    fn enumerate_lists_all_forms() {
        assert_eq!(
            Decorations::enumerate().unwrap(),
            vec!["full", "none", "transparent"]
        );
    }

    #[test]
    fn transparent_is_not_borderless() {
        // `transparent` looks frameless but stays resizable, so the maximize
        // path must NOT treat it as the non-resizable borderless case.
        assert!(!Decorations::Transparent.is_borderless());
    }
}
