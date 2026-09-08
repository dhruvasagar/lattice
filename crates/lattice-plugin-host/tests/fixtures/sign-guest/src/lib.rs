//! SG.3a sign fixture guest.
//!
//! Declares three signs through the imported `define-sign`, chosen to cover the
//! boundary shapes that can fail silently:
//!
//!   - `breakpoint` — both palettes populated at equal cell width. Dropping the
//!     fallback on the way across renders tofu for every user without a patched
//!     font, and nothing about the Nerd Font path would show it.
//!   - `current-line` — priority ABOVE the diagnostic threshold. The
//!     comparison is strictly-greater, so this is the only shape that proves a
//!     sign can take the mark cell from an error at all.
//!   - `note` — priority AT the threshold, which must NOT take the cell. The
//!     pair is what pins the boundary rather than just one side of it.
//!
//! It also redefines `breakpoint` at the end. The id must survive, or a plugin
//! reloading with a new glyph would orphan every placement already in flight.

wit_bindgen::generate!({
    world: "sign-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::signs::{SignSpec, define_sign};

struct Component;

impl Guest for Component {
    fn register_signs() {
        let _ = define_sign(
            "breakpoint",
            &SignSpec {
                text: "\u{f111}".to_string(),
                fallback: "●".to_string(),
                theme_element: "sign-guest.breakpoint".to_string(),
                priority: 20,
                column: String::new(),
            },
        );
        let _ = define_sign(
            "current-line",
            &SignSpec {
                text: "\u{f105}".to_string(),
                fallback: "▶".to_string(),
                theme_element: "sign-guest.current-line".to_string(),
                priority: 30,
                column: String::new(),
            },
        );
        // Level with a diagnostic, which must LOSE the cell — the comparison
        // is strictly-greater on purpose.
        let _ = define_sign(
            "note",
            &SignSpec {
                text: "\u{f075}".to_string(),
                fallback: "◆".to_string(),
                theme_element: "sign-guest.note".to_string(),
                priority: 10,
                column: String::new(),
            },
        );
        // A redefinition, which must keep the id rather than mint a new one.
        let _ = define_sign(
            "breakpoint",
            &SignSpec {
                text: "\u{f192}".to_string(),
                fallback: "◉".to_string(),
                theme_element: "sign-guest.breakpoint".to_string(),
                priority: 20,
                column: String::new(),
            },
        );
    }
}

export!(Component);
