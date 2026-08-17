//! TC.4 theme fixture guest.
//!
//! Declares three elements through the imported `register-element`, chosen to
//! cover the boundary shapes that can fail silently:
//!
//!   - `background` — a PALETTE reference. This is the path that matters: a
//!     palette key re-resolves on `:colorscheme`, a literal does not.
//!   - `active` — `inherit` plus a tri-state modifier where `italic` is
//!     explicitly `false`. `Some(false)` means "clear the inherited italic"
//!     and must not collapse to `None` ("unspecified") on the way across.
//!   - `separator` — a packed literal RGB, where a channel-order mistake in
//!     the unpack would be invisible without an exact assertion.

wit_bindgen::generate!({
    world: "theme-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::theme::{
    ColorRef, ModifierSet, StyleSpec, register_element,
};

struct Component;

fn no_modifiers() -> ModifierSet {
    ModifierSet {
        bold: None,
        italic: None,
        underline: None,
        dim: None,
        reverse: None,
    }
}

impl Guest for Component {
    fn register_theme_elements() {
        let _ = register_element(
            "background",
            "The context strip backdrop.",
            &StyleSpec {
                inherit: None,
                fg: Some(ColorRef::Palette("overlay".to_string())),
                bg: None,
                modifiers: no_modifiers(),
                scale: None,
            },
        );
        let _ = register_element(
            "active",
            "The innermost context row.",
            &StyleSpec {
                inherit: Some("treesitter-context.background".to_string()),
                fg: None,
                bg: None,
                modifiers: ModifierSet {
                    bold: Some(true),
                    italic: Some(false),
                    underline: None,
                    dim: None,
                    reverse: None,
                },
                scale: None,
            },
        );
        let _ = register_element(
            "separator",
            "The rule under the context strip.",
            &StyleSpec {
                inherit: None,
                fg: Some(ColorRef::LiteralRgb(0x11_22_33)),
                bg: Some(ColorRef::Default),
                modifiers: no_modifiers(),
                scale: None,
            },
        );
    }
}

export!(Component);
