//! PL8.B end-to-end: a picker plugin discovered on disk loads at boot and its
//! source becomes reachable in the (runtime-mutable) picker registry, with its
//! provenance recorded for `:list-plugins`.
//!
//! Uses the canonical `picker-guest` fixture the runtime crate builds to a
//! `wasm32-wasip2` component (registers a source with spec id `fixture`).
//! Skips when that component was not built (no `wasm32-wasip2` target) — the
//! same graceful skip the runtime crate's picker tests use.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_mode::PluginMetaSink;
use lattice_picker::{PickerRegistry, PickerRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;

/// The picker fixture component, if the `wasm32-wasip2` build produced it. The
/// loader crate can't read the runtime crate's build-script env var, so resolve
/// the artifact by its known path and skip if absent.
fn picker_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/picker-guest/target/wasm32-wasip2/release/picker_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// A test provenance sink: records every `register_plugin` so the test can
/// assert `:list-plugins` would show the plugin.
#[derive(Default)]
struct RecordingSink {
    registered: Mutex<Vec<(u32, String)>>,
}

impl PluginMetaSink for RecordingSink {
    fn register_plugin(&self, id: u32, name: String, _doc: String) {
        self.registered.lock().unwrap().push((id, name));
    }
    fn unregister_plugin(&self, id: u32) {
        self.registered.lock().unwrap().retain(|(i, _)| *i != id);
    }
}

/// Lay a discoverable plugin dir under `root`: `<root>/<id>/{plugin.toml, *.wasm}`.
fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"{provides}\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn temp_host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_picker_plugin_registers_its_source_and_provenance() {
    let Some(wasm) = picker_guest_wasm() else {
        eprintln!("skipping: picker-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "picker-fixture", "picker-source", &wasm);

    // A fresh (empty) picker registry so the assertion is unambiguous.
    let picker_registry: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(picker_registry.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(lattice_mode::GutterDecorationSourceRegistry::default()))),
            ..Default::default()
        },
    );

    // Sanity: discovery finds exactly the one plugin dir.
    assert_eq!(discover(&plugins_dir).len(), 1, "discovery finds the plugin");

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the picker plugin loads");

    // The source is now live in the registry under its guest-declared spec id.
    let reg = picker_registry.load();
    assert!(
        reg.get("fixture").is_some(),
        "the plugin's picker source is registered (reachable by :picker fixture)"
    );

    // Provenance was recorded — `:list-plugins` would show it.
    let recorded = sink.registered.lock().unwrap();
    assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
    assert_eq!(recorded[0].1, "picker-fixture", "recorded under its manifest id");

    assert!(loader.is_loaded("picker-fixture"), "loader tracks it as loaded");
}

fn config_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/config-guest/target/wasm32-wasip2/release/config_guest.wasm"
    );
    std::fs::read(path).ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_config_plugin_registers_its_options() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "config-fixture", "config", &wasm);

    // A fresh (empty) config registry so the plugin's options are the only entries.
    let config_registry = Arc::new(ConfigRegistry::default());
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            config_registry: Some(config_registry.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(lattice_mode::GutterDecorationSourceRegistry::default()))),
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the config plugin loads");

    // The plugin's typed option is live in the registry (`:set` / `:customize`
    // treat it uniformly with core options).
    assert!(
        config_registry.lookup("config-fixture.enabled").is_some(),
        "the plugin's option registered into the live config registry"
    );

    // Provenance recorded, so `:list-plugins` would show the config plugin.
    assert_eq!(sink.registered.lock().unwrap().len(), 1);
    assert!(loader.is_loaded("config-fixture"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_plugin_dir_is_skipped_without_aborting_discovery() {
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");

    // A dir with a manifest but no `.wasm` — malformed, must be skipped.
    let broken = plugins_dir.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("plugin.toml"), "id = \"broken\"\n").unwrap();

    // Discovery skips it (no component), and load count is zero — no panic.
    assert_eq!(
        discover(&plugins_dir).len(),
        0,
        "a dir with no component is not discovered"
    );

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()))),
            ..Default::default()
        },
    );
    assert_eq!(loader.discover_and_load(&plugins_dir, TrustTier::Bundled).await, 0);
}
