//! CI.5 end-to-end: the deferred-config chain (`with-eval-after-load`).
//!
//! An `init.rs`-shape plugin (init-guest) loads FIRST and subscribes to
//! `plugin-loaded`. Then the real `auto-pair` plugin loads — registering
//! `auto-pair-mode` **available-but-off** (CI.3) — and fires `PluginLoaded`.
//! init-guest's handler reacts by calling `enable-mode("auto-pair-mode")`, which
//! publishes `Event::ModeEnablementRequested`. This test observes that request on
//! the bus, proving the guest chain (subscribe-first → react-on-load →
//! enable-mode). CI.4 separately proves the request drives activation, and the
//! emacs-keys test proves re-activation — together the milestone.
//!
//! Skips when either fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn init_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/init-guest/target/wasm32-wasip2/release/init_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn auto_pair_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{provides}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn handle<T: 'static + Send + Sync>(v: T) -> Arc<arc_swap::ArcSwap<T>> {
    Arc::new(arc_swap::ArcSwap::from_pointee(v))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_rs_enables_auto_pairs_mode_when_auto_pair_loads() {
    let (Some(init_wasm), Some(ap_wasm)) = (init_guest_wasm(), auto_pair_wasm()) else {
        eprintln!("skipping: init-guest and/or auto-pair wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    // The init dir IS the plugin dir (loaded directly via `load_path`), so
    // plugin.toml + the wasm go straight in it — no per-plugin subdir.
    let init_dir = base.path().join("init");
    std::fs::create_dir_all(&init_dir).unwrap();
    std::fs::write(
        init_dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"events\"]\n",
    )
    .unwrap();
    std::fs::write(init_dir.join("component.wasm"), &init_wasm).unwrap();
    // The plugins dir is a scanned tree — each plugin in its own subdir.
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(
        &plugins_dir,
        "auto-pair",
        "\"grammar\", \"modes\", \"config\"",
        &ap_wasm,
    );

    let bus = Arc::new(EventBus::new());
    // Observe the enablement REQUEST the init handler will publish.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kind(EventKind::ModeEnablementRequested),
        SubscriptionTarget::Channel(tx),
    );

    let command_registry: CommandRegistryHandle = {
        let mut c = CommandRegistry::new();
        let _ = lattice_grammar::ex_commands::populate(&mut c);
        handle(c)
    };
    let mode_registry: ModeRegistryHandle = handle(ModeRegistry::default());
    let config_registry = Arc::new(ConfigRegistry::default());

    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            command_registry: Some(command_registry.clone()),
            mode_registry: Some(mode_registry.clone()),
            config_registry: Some(config_registry.clone()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    );

    // 1. init.rs FIRST — subscribes to `plugin-loaded` (CI.2 ordering).
    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init.rs loads");

    // 2. Then auto-pair — registers auto-pair-mode available-but-off + fires
    //    PluginLoaded("auto-pair"), which the init handler reacts to.
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::UserInstalled)
        .await;
    assert_eq!(n, 1, "auto-pair loads");

    // Off-by-default: the mode is registered but NOT enabled (only the init
    // handler enables it — via the request below, which the Editor would drain).
    let modes = mode_registry.load();
    assert!(
        modes.is_registered(ModeId::new("auto-pair-mode")),
        "auto-pair-mode is registered (available)"
    );
    assert!(
        !modes.is_minor_enabled(&ModeId::new("auto-pair-mode")),
        "auto-pair-mode is OFF by default (the loader does not enable it)"
    );

    // 3. The init handler ran on `plugin-loaded` and called enable-mode — assert
    //    the enablement request was published (the guest chain end to end).
    let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the init handler published an enablement request within 5s")
        .expect("channel delivered");
    match ev {
        Event::ModeEnablementRequested { mode, enabled } => {
            assert_eq!(mode, "auto-pair-mode", "init enabled auto-pair-mode");
            assert!(enabled, "it enabled (not disabled) the mode");
        }
        other => panic!("expected ModeEnablementRequested, got {other:?}"),
    }
}
