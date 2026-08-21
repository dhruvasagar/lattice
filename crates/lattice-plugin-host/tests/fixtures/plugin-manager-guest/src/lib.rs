//! PM.7 plugin-manager fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the
//! `plugin-manager-plugin` world. From `register-plugins` it declares one
//! plugin of each source kind, plus one with a path-traversal name the host
//! must refuse.
//!
//! Declaring through the raw WIT calls (no SDK) is deliberate: this is the
//! CANONICAL, language-agnostic surface a `init.rs` in any component-model
//! language reaches, and a fixture that went through a Rust SDK would only
//! prove the SDK works.

wit_bindgen::generate!({
    world: "plugin-manager-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::plugin_manager;
use lattice::plugin_host::plugin_manager::{GitSource, PluginSource, PluginSpec};

struct Component;

impl Guest for Component {
    fn register_plugins() {
        // A local source with a mode to enable — the use-package shape.
        let ok = plugin_manager::require(&PluginSpec {
            name: "local-demo".to_string(),
            source: PluginSource::Local("/tmp/lattice-demo".to_string()),
            enable_mode: Some("demo-mode".to_string()),
            pinned: false,
        });
        assert!(ok, "a well-formed local spec must be accepted");

        // A pinned git source.
        plugin_manager::require(&PluginSpec {
            name: "git_demo".to_string(),
            source: PluginSource::Git(GitSource {
                url: "https://example.invalid/demo.git".to_string(),
                rev: Some("abc123".to_string()),
            }),
            enable_mode: None,
            pinned: true,
        });

        // A prebuilt download — no build, no toolchain.
        plugin_manager::require(&PluginSpec {
            name: "prebuilt-demo".to_string(),
            source: PluginSource::Prebuilt("https://example.invalid/d.wasm".to_string()),
            enable_mode: None,
            pinned: false,
        });

        // A path-traversal name. The host must reject it and keep going —
        // one bad entry cannot take the whole config down.
        let rejected = plugin_manager::require(&PluginSpec {
            name: "../../escape".to_string(),
            source: PluginSource::Local("/tmp/evil".to_string()),
            enable_mode: None,
            pinned: false,
        });
        assert!(!rejected, "an unsafe name must be refused");
    }
}

export!(Component);
