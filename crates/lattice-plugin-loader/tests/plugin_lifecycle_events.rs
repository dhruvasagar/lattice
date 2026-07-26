//! CI.1 end-to-end: the loader publishes `Event::PluginLoaded` after a plugin's
//! full drain and `Event::PluginUnloaded` after teardown — the signal an
//! `init.rs` subscribes to for deferred config (`with-eval-after-load`;
//! config-and-init.md). Subscribes a native channel to the bus, loads a plugin,
//! and asserts the load event carries the plugin's manifest name; then unloads
//! and asserts the unload event. Skips when the fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn modes_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/modes-guest/target/wasm32-wasip2/release/modes_guest.wasm"
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

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

fn command_registry_with_builtins() -> CommandRegistryHandle {
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    Arc::new(arc_swap::ArcSwap::from_pointee(commands))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loader_publishes_plugin_loaded_and_unloaded() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "lifecycle-fixture", "modes", &wasm);

    let bus = Arc::new(EventBus::new());
    // Subscribe a channel to the lifecycle kinds BEFORE loading — exactly the
    // ordering an init.rs relies on (it loads first, subscribes, then the plugin
    // it cares about loads and fires the event).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kinds(vec![EventKind::PluginLoaded, EventKind::PluginUnloaded]),
        SubscriptionTarget::Channel(tx),
    );

    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            command_registry: Some(command_registry_with_builtins()),
            mode_registry: Some(empty_mode_registry()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the plugin loads");

    // PluginLoaded fired, carrying the manifest id, AFTER the drain completed.
    let ev = rx.recv().await.expect("a lifecycle event was published");
    match ev {
        Event::PluginLoaded { name, id: _ } => {
            assert_eq!(name, "lifecycle-fixture", "carries the manifest id");
            // `id` is the host-issued numeric plugin id (0 for the first plugin —
            // valid); handlers match on `name`, not `id`.
        }
        other => panic!("expected PluginLoaded, got {other:?}"),
    }

    // Unload → PluginUnloaded, same identity.
    let report = loader.unload("lifecycle-fixture");
    assert!(report.is_some(), "the plugin was loaded");
    let ev = rx.recv().await.expect("an unload event was published");
    match ev {
        Event::PluginUnloaded { name, .. } => {
            assert_eq!(name, "lifecycle-fixture", "unload carries the manifest id");
        }
        other => panic!("expected PluginUnloaded, got {other:?}"),
    }
}
