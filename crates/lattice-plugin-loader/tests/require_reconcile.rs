//! PMF.14: `require` reconciles with what is loaded instead of loading every
//! spec.
//!
//! Driven through `PluginLoader::apply_required` with pipeline results, so the
//! decisions are tested without git, network or cargo: at boot nothing loads
//! (the scan does, once); after boot a new plugin loads, a rebuilt one
//! reloads, and an unchanged one is left alone.
//!
//! Uses the `config-guest` fixture as the required plugin, staged into a user
//! root the way the install pipeline stages one.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{Install, LoaderServices, PluginLoader, RequirePhase};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use tokio::sync::mpsc::UnboundedReceiver;

fn config_guest_wasm() -> Option<Vec<u8>> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/config-guest/target/wasm32-wasip2/release/config_guest.wasm"
    ))
    .ok()
}

struct Rig {
    loader: Arc<PluginLoader>,
    user_root: PathBuf,
    loads: UnboundedReceiver<Event>,
    modes: UnboundedReceiver<Event>,
    _base: tempfile::TempDir,
}

/// A loader, and a user root holding `demo` as the pipeline would stage it.
fn rig(wasm: &[u8]) -> Rig {
    let base = tempfile::tempdir().unwrap();
    let user_root = base.path().join("plugins");
    let dir = user_root.join("demo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"demo\"\nprovides = [\"config\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("demo.wasm"), wasm).unwrap();

    let bus = Arc::new(EventBus::new());
    let (loads_tx, loads) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::PluginLoaded),
        SubscriptionTarget::Channel(loads_tx),
    );
    let (modes_tx, modes) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::ModeEnablementRequested),
        SubscriptionTarget::Channel(modes_tx),
    );
    let loader = Arc::new(PluginLoader::with_services(
        Arc::new(
            PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
        ),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            ..Default::default()
        },
    ));
    Rig {
        loader,
        user_root,
        loads,
        modes,
        _base: base,
    }
}

fn ready(user_root: &Path, rebuilt: bool) -> Install {
    Install::Ready {
        name: "demo".to_string(),
        artifact: user_root.join("demo").join("demo.wasm"),
        enable_mode: Some("demo-mode".to_string()),
        stale: None,
        rebuilt,
    }
}

/// How many of `rx`'s events arrive within a short window.
async fn count(rx: &mut UnboundedReceiver<Event>) -> usize {
    let mut n = 0;
    while tokio::time::timeout(Duration::from_millis(300), rx.recv())
        .await
        .is_ok_and(|e| e.is_some())
    {
        n += 1;
    }
    n
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_boot_require_installs_and_the_scan_loads_once() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let mut rig = rig(&wasm);
    let pending = rig
        .loader
        .apply_required(
            vec![ready(&rig.user_root, true)],
            &rig.user_root,
            RequirePhase::Boot,
        )
        .await;
    assert!(
        !rig.loader.is_loaded("demo"),
        "require does not load at boot"
    );
    assert_eq!(pending, [("demo".to_string(), "demo-mode".to_string())]);
    assert_eq!(
        count(&mut rig.modes).await,
        0,
        "no mode before its plugin loads"
    );

    // Boot's next step: the scan, then the deferred modes.
    rig.loader
        .discover_and_load(&rig.user_root, TrustTier::UserInstalled)
        .await;
    rig.loader.enable_required_modes(pending);
    assert_eq!(count(&mut rig.loads).await, 1, "loaded once, by the scan");
    assert_eq!(count(&mut rig.modes).await, 1, "and its mode enabled after");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_boot_a_new_plugin_loads_and_an_unchanged_one_is_left_alone() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let mut rig = rig(&wasm);

    // A `require` added since boot: not loaded yet, so it loads — no restart.
    rig.loader
        .apply_required(
            vec![ready(&rig.user_root, false)],
            &rig.user_root,
            RequirePhase::AfterBoot,
        )
        .await;
    assert!(rig.loader.is_loaded("demo"));
    assert_eq!(count(&mut rig.loads).await, 1);
    assert_eq!(count(&mut rig.modes).await, 1, "its enable_mode applies");

    // The same `require` again, nothing rebuilt: a no-op.
    rig.loader
        .apply_required(
            vec![ready(&rig.user_root, false)],
            &rig.user_root,
            RequirePhase::AfterBoot,
        )
        .await;
    assert_eq!(
        count(&mut rig.loads).await,
        0,
        "an unchanged plugin is not reloaded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_boot_a_rebuilt_plugin_is_reloaded() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let mut rig = rig(&wasm);
    rig.loader
        .load_path(&rig.user_root.join("demo"), TrustTier::UserInstalled)
        .await
        .unwrap();
    assert_eq!(count(&mut rig.loads).await, 1);

    rig.loader
        .apply_required(
            vec![ready(&rig.user_root, true)],
            &rig.user_root,
            RequirePhase::AfterBoot,
        )
        .await;
    assert_eq!(
        count(&mut rig.loads).await,
        1,
        "reloaded onto the new build"
    );
    assert!(rig.loader.is_loaded("demo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spec_with_nothing_to_load_is_recorded_and_its_mode_dropped() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let mut rig = rig(&wasm);
    let pending = rig
        .loader
        .apply_required(
            vec![Install::Skipped {
                name: "broken".to_string(),
                error: "clone failed".to_string(),
            }],
            &rig.user_root,
            RequirePhase::Boot,
        )
        .await;
    assert!(pending.is_empty());
    let failed = rig.loader.failed_loads();
    assert_eq!(failed.len(), 1, "visible in :plugins: {failed:?}");
    assert_eq!(failed[0].name, "broken");

    // A deferred mode whose plugin never loaded asks for nothing.
    rig.loader
        .enable_required_modes(vec![("broken".to_string(), "broken-mode".to_string())]);
    assert_eq!(count(&mut rig.modes).await, 0);
}
