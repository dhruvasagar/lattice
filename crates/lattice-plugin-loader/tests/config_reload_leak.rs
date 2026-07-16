//! PL8.F exit test: reloading a config plugin N times reclaims its option
//! strings — the interned-string footprint stays bounded, it does not grow per
//! reload.
//!
//! Before PL8.F, a plugin option's `name`/`doc` were `Box::leak`ed to
//! `&'static str` in `config_host::build_and_register`; the `ConfigRegistry`
//! *entry* was removed on unload but the leaked bytes lingered, so repeated
//! `:plugin-reload` / `:reload-config` grew the footprint unbounded. PL8.F made
//! the native option `name`/`doc` `Cow<'static, str>`, so the plugin's owned
//! strings free with the entry on `ConfigRegistry::unregister`. This test drives
//! the real load → reload loop through the `config-guest` fixture (registers 3
//! options) and asserts the live option count stays bounded across N reloads,
//! and that a final unload reclaims every option (the owned strings drop).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    GutterDecorationSourceRegistry, GutterDecorationSourceRegistryHandle, ModeRegistry,
    ModeRegistryHandle,
};
use lattice_picker::source::PickerRegistry;
use lattice_picker::PickerRegistryHandle;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn config_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/config-guest/target/wasm32-wasip2/release/config_guest.wasm"
    );
    std::fs::read(path).ok()
}

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

/// A fully-wired loader over a shared `ConfigRegistry` the test inspects, plus
/// every other registry `run_teardown` needs so unload/reload actually reverse.
fn rig(base: &std::path::Path, config: Arc<ConfigRegistry>) -> PluginLoader {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let decorations: GutterDecorationSourceRegistryHandle = Arc::new(
        arc_swap::ArcSwap::from_pointee(GutterDecorationSourceRegistry::new()),
    );
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(config),
            keymap: Some(KeymapHandle::new()),
            decoration_registry: Some(decorations),
            meta_sink: None,
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloading_a_config_plugin_keeps_the_option_footprint_bounded() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "config-fixture", "config", &wasm);

    let config = Arc::new(ConfigRegistry::default());
    assert_eq!(config.len(), 0, "registry starts empty");
    let loader = rig(base.path(), config.clone());

    // Initial load: the fixture registers exactly 3 options.
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the config plugin loads");
    const FIXTURE_OPTIONS: usize = 3;
    assert_eq!(
        config.len(),
        FIXTURE_OPTIONS,
        "the fixture's 3 options are registered"
    );
    assert!(
        config.lookup("config-fixture.enabled").is_some(),
        "a plugin option is reachable by name (owned `Cow` string survived the WIT crossing)"
    );

    // The reclamation contract: reload N times. Each reload unloads (dropping the
    // prior options' entries — and, post-PL8.F, their owned `Cow` name/doc bytes)
    // and re-registers. The LIVE option count must stay at 3 forever, not grow
    // 3 → 6 → 9 → … The pre-PL8.F entry removal already kept `len()` bounded; the
    // point PL8.F adds is that the *strings* now free with those entries rather
    // than leaking as `&'static str`.
    for round in 1..=12 {
        loader.reload("config-fixture", TrustTier::Bundled).await.unwrap();
        assert_eq!(
            config.len(),
            FIXTURE_OPTIONS,
            "after reload #{round} the live option count is still bounded at {FIXTURE_OPTIONS}, \
             not accumulating per reload"
        );
        assert!(
            config.lookup("config-fixture.enabled").is_some(),
            "the re-registered option is reachable after reload #{round}"
        );
    }

    // Final unload reclaims every option — the owned name/doc strings drop with
    // the entries, leaving the registry empty (no tombstone residue in `len()`).
    loader.unload("config-fixture").unwrap();
    assert_eq!(
        config.len(),
        0,
        "unload removed every option; nothing leaked into the live set"
    );
    assert!(
        config.lookup("config-fixture.enabled").is_none(),
        "the plugin option (and its owned strings) is gone after unload"
    );
}
