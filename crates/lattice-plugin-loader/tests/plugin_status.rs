//! PL8.H.1 — the loader's plugin **status data model**: `plugin_status()`
//! reflects a loaded plugin's identity, trust tier, and capabilities
//! granted/denied; `Event::PluginCrashed` flips its health to quarantined; and
//! unload drops it from the status set. This is the read model the `:plugins`
//! manager view (PL8.H.2/.3) renders.
//!
//! Uses the canonical `modes-guest` fixture (a loadable `modes`-world component)
//! with a manifest that additionally requests `proc:spawn` — a bundled-only OS
//! capability — so the `UserInstalled` tier denies it and the `Bundled` tier
//! grants it, exercising both sides of the grant split. Skips when the fixture
//! was not built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{Capability, PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginHealth, PluginLoader, discover};
use lattice_protocol::Event;
use lattice_runtime::EventBus;

fn modes_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/modes-guest/target/wasm32-wasip2/release/modes_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// Write a plugin dir whose manifest declares `provides` AND requests one OS
/// `capability` (wire form, e.g. `proc:spawn`), so the status test can observe
/// the tier-gated grant split.
fn write_plugin_dir_with_cap(
    root: &std::path::Path,
    id: &str,
    provides: &str,
    capability: &str,
    wasm: &[u8],
) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"{provides}\"]\ncapabilities = [\"{capability}\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn temp_host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

fn command_registry_with_builtins() -> CommandRegistryHandle {
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    Arc::new(arc_swap::ArcSwap::from_pointee(commands))
}

/// A loader wired for the modes seam + a bus (health subscription needs it).
fn modes_loader(base: &std::path::Path, bus: Arc<EventBus>) -> PluginLoader {
    PluginLoader::with_services(
        temp_host(base),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus),
            command_registry: Some(command_registry_with_builtins()),
            mode_registry: Some(empty_mode_registry()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reflects_capabilities_denied_under_user_tier() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir_with_cap(&plugins_dir, "cap-fixture", "modes", "proc:spawn", &wasm);

    let loader = modes_loader(base.path(), Arc::new(EventBus::new()));
    assert_eq!(
        loader
            .discover_and_load(&plugins_dir, TrustTier::UserInstalled)
            .await,
        1,
        "the plugin loads (a withheld capability is degraded, never fatal)"
    );

    let status = loader.plugin_status();
    assert_eq!(status.len(), 1, "one plugin in the status set");
    let s = &status[0];
    assert_eq!(s.name, "cap-fixture");
    assert_eq!(s.tier, TrustTier::UserInstalled);
    assert_eq!(
        s.denied,
        vec![Capability::ProcSpawn],
        "proc:spawn is bundled-only, so a user-installed plugin is denied it"
    );
    assert!(
        s.granted.is_empty(),
        "the only requested capability was denied, so nothing was granted"
    );
    assert_eq!(s.health, PluginHealth::Healthy, "loaded plugins start healthy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reflects_capabilities_granted_under_bundled_tier() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir_with_cap(&plugins_dir, "cap-fixture", "modes", "proc:spawn", &wasm);

    let loader = modes_loader(base.path(), Arc::new(EventBus::new()));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    let s = &loader.plugin_status()[0];
    assert_eq!(s.tier, TrustTier::Bundled);
    assert_eq!(
        s.granted,
        vec![Capability::ProcSpawn],
        "the bundled tier grants proc:spawn"
    );
    assert!(s.denied.is_empty(), "nothing denied at the bundled tier");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mark_quarantined_flips_health_and_is_ignored_for_unknown_ids() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir_with_cap(&plugins_dir, "cap-fixture", "modes", "proc:spawn", &wasm);

    let loader = modes_loader(base.path(), Arc::new(EventBus::new()));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let id = loader.plugin_status()[0].id;

    // An unknown crash id is a benign no-op — the plugin stays healthy.
    loader.mark_quarantined(id.wrapping_add(999), "on-event".into(), "trap".into());
    assert_eq!(loader.plugin_status()[0].health, PluginHealth::Healthy);

    // A crash for the real id flips its health, carrying the provenance.
    loader.mark_quarantined(id, "on-event".into(), "fuel".into());
    assert_eq!(
        loader.plugin_status()[0].health,
        PluginHealth::Quarantined {
            func: "on-event".into(),
            kind: "fuel".into()
        },
        "the crashed plugin reports quarantined with its trap provenance"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_crashed_event_flips_health_through_the_subscription() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir_with_cap(&plugins_dir, "cap-fixture", "modes", "proc:spawn", &wasm);

    let bus = Arc::new(EventBus::new());
    // The full path: an Arc-wrapped loader subscribes to `PluginCrashed`; a
    // publish drains through the runtime and marks the plugin quarantined.
    let loader = Arc::new(modes_loader(base.path(), bus.clone()));
    loader.subscribe_health();
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let id = loader.plugin_status()[0].id;

    bus.publish(Event::PluginCrashed {
        plugin: id,
        func: "generate".into(),
        kind: "epoch".into(),
    });

    // The drain is async (bus → channel → runtime task); poll with a bounded
    // budget until health flips rather than sleeping a fixed interval.
    let mut flipped = false;
    for _ in 0..200 {
        if loader.plugin_status()[0].health.is_quarantined() {
            flipped = true;
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert!(
        flipped,
        "the PluginCrashed subscription flipped the plugin to quarantined"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unload_drops_the_plugin_from_the_status_set() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir_with_cap(&plugins_dir, "cap-fixture", "modes", "proc:spawn", &wasm);

    let loader = modes_loader(base.path(), Arc::new(EventBus::new()));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loader.plugin_status().len(), 1);
    assert_eq!(discover(&plugins_dir).len(), 1, "discovery sanity");

    loader.unload("cap-fixture").expect("unloads");
    assert!(
        loader.plugin_status().is_empty(),
        "unload removes the plugin from the status set"
    );
}
