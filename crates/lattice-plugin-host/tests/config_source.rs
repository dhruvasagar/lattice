//! PH7.10 — the config/options seam, driven through a real guest.
//!
//! Instantiates the `config-guest` fixture (a `wasm32-wasip2` `config-plugin`
//! component) via [`PluginHost::spawn_config_plugin`], which drives its
//! `register-options` export against a native [`ConfigRegistry`]. Proves the seam
//! end to end:
//!   - the guest's imported `register-option` calls land three typed options
//!     (bool / integer / string) in the SAME registry core options use,
//!   - the guest's `get-option` reads a value back through the registry (written
//!     to its data-dir mount, `/data/option.log`),
//!   - a plugin option is a first-class registry entry: `:set` parses + sets it
//!     uniformly, and the value round-trips.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

const PLUGIN_ID: &str = "config-fixture";

/// The fixture config component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("CONFIG_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// The host path the guest's `/data/option.log` maps to for a given data base.
fn option_log(data_base: &std::path::Path) -> PathBuf {
    data_base.join(PLUGIN_ID).join("data").join("option.log")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_declares_options_into_the_shared_registry_end_to_end() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: config fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile config fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    // A fresh registry (no linkme core options) — the plugin options are the only
    // entries, so assertions are hermetic.
    let registry = Arc::new(ConfigRegistry::default());

    let names = host
        .spawn_config_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &registry,
        )
        .await
        .expect("spawn config plugin");

    // The three declared options were reported (drain of the plugin's
    // contributions) and landed in the shared registry with their mapped types.
    assert_eq!(names.len(), 3, "three options declared: {names:?}");
    assert!(names.iter().any(|n| n == "config-fixture.enabled"));

    let enabled = registry.lookup("config-fixture.enabled").expect("registered");
    assert_eq!(enabled.type_label(), "boolean");
    assert_eq!(enabled.get_formatted(), "true");

    let count = registry.lookup("config-fixture.count").expect("registered");
    assert_eq!(count.type_label(), "integer");
    assert_eq!(count.get_formatted(), "3");

    let label = registry.lookup("config-fixture.label").expect("registered");
    assert_eq!(label.type_label(), "string");
    assert_eq!(label.get_formatted(), "hello");

    // The guest read `config-fixture.count` back through `get-option` during
    // registration and recorded it — the value crossed the registry round-trip.
    let logged = std::fs::read_to_string(option_log(&data_base)).unwrap_or_default();
    assert_eq!(logged.trim(), "count=3", "get-option returned the default");

    // A plugin option is a first-class registry entry: `:set` works uniformly and
    // the value round-trips (this is what `:set config-fixture.count=7` drives).
    registry
        .parse_and_set_command("config-fixture.count=7")
        .expect(":set on a plugin option works");
    assert_eq!(
        registry.lookup("config-fixture.count").unwrap().get_formatted(),
        "7"
    );
}
