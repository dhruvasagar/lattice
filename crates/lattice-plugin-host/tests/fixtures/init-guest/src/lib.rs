//! CI.5 fixture — the `init.rs` deferred-config shape (`with-eval-after-load`).
//!
//! From `register-events` it subscribes to `plugin-loaded`; when the awaited
//! plugin ("auto-pair") loads, its `on-event` handler calls the imported
//! `modes.enable-mode("auto-pair-mode")`. Loaded FIRST (CI.2 ordering) so the
//! subscription is live before the plugin fires the event; the handler runs in
//! the events store, whose bus lets `enable-mode` publish its request
//! (config-and-init.md §6). Config for a plugin that never loads never runs —
//! graceful by construction.
//!
//! OA.14d added the OTHER half of the same pattern: a second subscription, to
//! `pre-plugin-loaded`, for config a plugin reads at LOAD rather than after it.
//! Both handlers live in this one fixture on purpose — a real `init.rs` uses
//! both, and separating them into two fixtures would let a change break the
//! interaction between them without any test noticing.

wit_bindgen::generate!({
    world: "init-fixture",
    path: "../../../../../wit",
});

use lattice::plugin_host::types::{EventFilter, EventKind};
use lattice::plugin_host::{config, events, modes};

struct Component;

impl Guest for Component {
    fn register_events() {
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::PluginLoaded]),
                path_globs: None,
                major_modes: None,
            },
            1,
        );
        // OA.14d: a SECOND handler, on the awaited pre-load signal. Distinct
        // handler id, so a delivery routed to the wrong one is a visible
        // failure rather than a coincidence that happens to work.
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::PrePluginLoaded]),
                path_globs: None,
                major_modes: None,
            },
            2,
        );
    }

    fn on_event(_handler: u32, ev: Event) {
        // OA.14d: config for a value the plugin reads DURING its own load. The
        // name match is the point of the event carrying a name — this handler
        // runs for every plugin on the disk and must configure exactly one.
        if let Event::PrePluginLoaded(name) = &ev {
            if name == "preload-fixture" {
                config::set_option("preload-fixture.keywords", "from-init");
            }
            return;
        }
        if let Event::PluginLoaded(p) = ev {
            // Deferred config: enable auto-pair-mode the moment auto-pair loads.
            if p.name == "auto-pair" {
                modes::enable_mode("auto-pair-mode");
                // …and SET one of its options, which is the other half of the
                // documented deferred shape and the half that was never driven.
                // `enable-mode` reaches the bus; `set-option` reaches the config
                // registry, and the events store did not carry one — so this
                // call warned and no-oped while the test above still passed.
                // Full name, not the short one: `set-option` prefixes with the
                // CALLING plugin's id, so `style` would resolve as `init.style`.
                config::set_option("auto-pair.style", "manual");
            }
        }
    }

    /// OC.2: this fixture arms no wakes, but the world's exports must satisfy
    /// the `events-plugin` bindings the host instantiates against.
    fn on_wake(_id: u32) {}
}

export!(Component);
