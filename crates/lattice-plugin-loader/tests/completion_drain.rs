//! PL8.B end-to-end: a completion plugin discovered on disk loads at boot and
//! its source becomes reachable through a loader-contributed universal carrier
//! mode's `completion_sources()` — the shape the native aggregator
//! (`recompute_active_completion_sources_for`) walks (option A: completion is
//! mode-attached, like LSP / snippet).
//!
//! Uses the canonical `completion-guest` fixture the runtime crate builds to a
//! `wasm32-wasip2` component (source id `keywords`). Skips when that component
//! was not built. The async-produce path itself (guest `generate` → candidates
//! → native `match_and_rank`) is proven by `lattice-plugin-host`'s
//! `completion_source.rs`; this test proves the *drain* — that the source is
//! wrapped as a native async contribution and rides a mode the aggregator reads.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_completion::CompletionSourceKind;
use lattice_mode::{ActivationPolicy, ModeId, ModeKind, ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;

fn completion_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/completion-guest/target/wasm32-wasip2/release/completion_guest.wasm"
    );
    std::fs::read(path).ok()
}

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

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_completion_plugin_rides_a_universal_carrier_mode() {
    let Some(wasm) = completion_guest_wasm() else {
        eprintln!("skipping: completion-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "completion-fixture", "completion-source", &wasm);

    let mode_registry = empty_mode_registry();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            mode_registry: Some(mode_registry.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(lattice_mode::GutterDecorationSourceRegistry::default()))),
            ..Default::default()
        },
    );

    assert_eq!(discover(&plugins_dir).len(), 1, "discovery finds the plugin");

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the completion plugin loads");
    assert!(loader.is_loaded("completion-fixture"), "loader tracks it loaded");

    // The carrier mode is registered — universal minor, `<id>-completion-mode`.
    let modes = mode_registry.load();
    let carrier = ModeId::new("completion-fixture-completion-mode");
    let mode = modes
        .get(carrier)
        .expect("the loader registered a carrier mode for the completion source");
    assert_eq!(mode.kind(), ModeKind::Minor);
    assert!(
        matches!(mode.activation_policy(), ActivationPolicy::Universal),
        "the carrier mode is universal so the source contributes on every buffer"
    );

    // The mode surfaces the plugin's source through `completion_sources()` — the
    // exact shape `recompute_active_completion_sources_for` walks. Wrapped as a
    // native async contribution at the documented plugin default priority.
    let sources = mode.completion_sources();
    assert_eq!(sources.len(), 1, "the carrier mode contributes exactly the plugin source");
    let contribution = &sources[0];
    assert_eq!(contribution.id.0, "keywords", "the guest-declared source id");
    assert_eq!(contribution.default_priority, 100, "the plugin-source default bucket");
    assert!(contribution.auto_trigger, "auto-triggers on identifier chars");
    assert!(
        matches!(contribution.kind, CompletionSourceKind::Async(_)),
        "a WASM source is async (produce runs off the keystroke path)"
    );

    // Provenance recorded — `:list-plugins` would show it.
    let recorded = sink.registered.lock().unwrap();
    assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
    assert_eq!(recorded[0].1, "completion-fixture", "under its manifest id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_plugin_without_a_wired_mode_registry_is_skipped_not_fatal() {
    let Some(wasm) = completion_guest_wasm() else {
        eprintln!("skipping: completion-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "completion-fixture", "completion-source", &wasm);

    // No mode registry wired — the carrier-mode registration has nowhere to land,
    // so the drain hits `NotWired("completion-source")`, which `discover_and_load`
    // logs + skips (graceful degradation, never a boot abort or panic).
    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            mode_registry: None,
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 0, "the completion plugin is skipped, not loaded");
    assert!(!loader.is_loaded("completion-fixture"), "nothing recorded");
}
