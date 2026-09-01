//! OA.14d fixture guest — the load-time-option shape, which is org's.
//!
//! `register-options` declares `keywords` (namespaced by the host to
//! `<plugin>.keywords`). `register-theme-elements` reads it back and registers
//! ONE element whose name is the value it saw:
//!
//! ```text
//!   keywords = "TODO"      → element `<plugin>.saw-TODO`
//! ```
//!
//! The name is the assertion surface. A test that instead re-read the option
//! after the load would pass whether or not the `pre-plugin-loaded` handler beat
//! the read — the value would be right either way. The element records what the
//! guest actually held *at the moment it consumed the option*, which is the only
//! thing the barrier is claiming to control.

wit_bindgen::generate!({
    world: "preload-fixture",
    path: "../../../../../wit",
});

use lattice::plugin_host::config::{self, OptionType};
use lattice::plugin_host::theme::{ColorRef, ModifierSet, StyleSpec, register_element};

struct Component;

/// What the option holds if nobody sets it — the stand-in for org's compiled
/// `"TODO | DONE"`, i.e. the wrong-but-plausible answer a lost race produces.
const DEFAULT_KEYWORDS: &str = "compiled-default";

impl Guest for Component {
    fn register_options() {
        config::register_option(
            "keywords",
            OptionType::String,
            DEFAULT_KEYWORDS,
            "The value this fixture reads back at load time (OA.14d).",
        );
    }

    fn register_theme_elements() {
        let seen = config::get_option("keywords").unwrap_or_else(|| DEFAULT_KEYWORDS.to_string());
        let _ = register_element(
            &format!("saw-{seen}"),
            "Names the option value this component read during its load.",
            &StyleSpec {
                inherit: None,
                fg: Some(ColorRef::Default),
                bg: None,
                modifiers: ModifierSet {
                    bold: None,
                    italic: None,
                    underline: None,
                    dim: None,
                    reverse: None,
                },
                scale: None,
            },
        );
    }
}

export!(Component);
