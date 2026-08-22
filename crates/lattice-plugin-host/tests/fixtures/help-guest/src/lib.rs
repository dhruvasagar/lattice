//! CR.3 help fixture guest.
//!
//! Declares four topics through the imported `register-topic`, chosen to
//! cover the shapes that can fail silently:
//!
//!   - `""` (empty name) — must land at the BARE plugin id, so a one-page
//!     plugin is `:help help-guest` rather than `:help help-guest.help-guest`.
//!   - `"usage"` — the ordinary case, namespaced to `help-guest.usage`, with
//!     a `related-commands` pattern so `:describe-command` cross-links to it.
//!   - `"buffers"` — deliberately collides with a BUILTIN topic name.
//!     Namespacing must make it `help-guest.buffers` and leave the real
//!     `:help buffers` untouched. Without a fixture that tries this, nothing
//!     proves the namespace is load-bearing rather than decorative.
//!   - `"empty"` — an empty body, which the host must reject WITHOUT
//!     failing the load or costing the other three topics.
//!
//! Both real bodies are `include_str!`'d, which is the entire point of the
//! seam: the markdown is compiled into this component, so it travels with
//! the plugin and disappears with it.

wit_bindgen::generate!({
    world: "help-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::help::register_topic;

struct Component;

impl Guest for Component {
    fn register_help_topics() {
        // Empty name ⇒ the bare plugin id.
        let _ = register_topic(
            "",
            "The help-guest fixture plugin.",
            include_str!("../doc/index.md"),
            &[],
        );
        let _ = register_topic(
            "usage",
            "How the help-guest fixture ships its docs.",
            include_str!("../doc/usage.md"),
            &["help-guest".to_string()],
        );
        // Namespacing must defuse this.
        let _ = register_topic(
            "buffers",
            "Not the builtin buffers page.",
            "# Impostor\n\nIf `:help buffers` shows this, namespacing failed.",
            &[],
        );
        // Rejected host-side; must not cost the three above.
        let _ = register_topic("empty", "Has no body.", "", &[]);
    }
}

export!(Component);
