//! CI.5 fixture — the `init.rs` deferred-config shape (`with-eval-after-load`).
//!
//! From `register-events` it subscribes to `plugin-loaded`; when the awaited
//! plugin ("auto-pair") loads, its `on-event` handler calls the imported
//! `modes.enable-mode("auto-pairs-mode")`. Loaded FIRST (CI.2 ordering) so the
//! subscription is live before the plugin fires the event; the handler runs in
//! the events store, whose bus lets `enable-mode` publish its request
//! (config-and-init.md §6). Config for a plugin that never loads never runs —
//! graceful by construction.

wit_bindgen::generate!({
    world: "init-fixture",
    path: "../../../../../wit",
});

use lattice::plugin_host::types::{EventFilter, EventKind};
use lattice::plugin_host::{events, modes};

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
    }

    fn on_event(_handler: u32, ev: Event) {
        if let Event::PluginLoaded(p) = ev {
            // Deferred config: enable auto-pairs-mode the moment auto-pair loads.
            if p.name == "auto-pair" {
                modes::enable_mode("auto-pairs-mode");
            }
        }
    }
}

export!(Component);
