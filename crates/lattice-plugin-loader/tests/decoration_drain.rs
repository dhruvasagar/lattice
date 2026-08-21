//! PL8.E end-to-end: a decorations plugin discovered on disk loads at boot and
//! its gutter-decoration producer becomes reachable through the runtime-mutable
//! [`GutterDecorationSourceRegistry`] — the exact handle the host's per-tick
//! `maybe_refresh_wasm_decorations` reads to fill the per-buffer cache the
//! renderer paints from.
//!
//! Uses the canonical `decorations-guest` fixture the plugin-host crate builds
//! to a `wasm32-wasip2` component (marks: line 0 → Diff/Change, line 1 →
//! Severity/Error, last line → Diff/Add). Skips when that component was not
//! built. The async-produce path itself (guest `gutter-decorations` → native
//! `GutterDecoration`) is proven by `lattice-plugin-host`; this test proves the
//! *drain* — that the producer is registered into the decoration registry, is
//! callable, and is reversed on unload.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    GutterDecoration, GutterDecorationSourceRegistry, GutterDecorationSourceRegistryHandle,
    GutterDiffKind, GutterSeverityLevel, ModeRegistry, ModeRegistryHandle, PluginMetaSink,
};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;

fn decorations_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/decorations-guest/target/wasm32-wasip2/release/decorations_guest.wasm"
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

/// A fully-wired loader over hermetic (empty) registries — the decoration
/// registry the drain registers into, plus every other registry `run_teardown`
/// needs so `unload` reverses (not the missing-handle no-op path).
struct Rig {
    loader: PluginLoader,
    decorations: GutterDecorationSourceRegistryHandle,
    sink: Arc<RecordingSink>,
}

fn rig(base: &std::path::Path) -> Rig {
    let decorations: GutterDecorationSourceRegistryHandle = Arc::new(
        arc_swap::ArcSwap::from_pointee(GutterDecorationSourceRegistry::new()),
    );
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let loader = PluginLoader::with_services(
        temp_host(base),
        LoaderServices {
            parser_factories: Some(lattice_compilation::CompilationParserFactories::new_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            decoration_registry: Some(decorations.clone()),
            context_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::ContextSourceRegistry::new(),
            ))),
            theme_registry: Some(std::sync::Arc::new(
                lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette()),
            )),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
        },
    );
    Rig {
        loader,
        decorations,
        sink,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_decorations_plugin_registers_a_callable_producer() {
    let Some(wasm) = decorations_guest_wasm() else {
        eprintln!("skipping: decorations-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "deco-fixture", "decorations", &wasm);

    let rig = rig(base.path());
    assert_eq!(
        discover(&plugins_dir).len(),
        1,
        "discovery finds the plugin"
    );

    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the decorations plugin loads");
    assert!(
        rig.loader.is_loaded("deco-fixture"),
        "loader tracks it loaded"
    );

    // The producer is registered into the wait-free decoration registry — the
    // handle the host's per-tick refresh reads.
    let sources = rig.decorations.load().sources();
    assert_eq!(
        sources.len(),
        1,
        "exactly the plugin's producer is registered"
    );

    // It is callable off the render path: for a 5-line buffer the fixture emits
    // Diff/Change@0, Severity/Error@1, Diff/Add@4 (the last line proves
    // `line_count` crossed the boundary). Marks are the native `GutterDecoration`
    // both renderers already paint — no renderer knows this came from WASM.
    let marks = sources[0]
        .produce(7, None, 5)
        .await
        .expect("producer yields marks for a non-empty buffer");
    assert!(
        marks.contains(&GutterDecoration::Diff {
            line: 0,
            kind: GutterDiffKind::Change
        }),
        "line 0 → diff/change; got {marks:?}"
    );
    assert!(
        marks.contains(&GutterDecoration::Severity {
            line: 1,
            level: GutterSeverityLevel::Error
        }),
        "line 1 → severity/error; got {marks:?}"
    );
    assert!(
        marks.contains(&GutterDecoration::Diff {
            line: 4,
            kind: GutterDiffKind::Add
        }),
        "last line → diff/add (line_count crossed); got {marks:?}"
    );

    // Empty-buffer path: the fixture returns `Err` (graceful "no decorations"),
    // which the host treats as "keep the prior snapshot" (no flicker).
    assert!(
        sources[0].produce(7, None, 0).await.is_err(),
        "an empty buffer yields Err (keep-prior contract), not a spurious mark set"
    );

    // Provenance recorded — `:list-plugins` would show it.
    let recorded = rig.sink.registered.lock().unwrap();
    assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
    assert_eq!(recorded[0].1, "deco-fixture", "under its manifest id");
    drop(recorded);

    // Unload reverses the decoration surface: the producer is unregistered and
    // the teardown report counts it.
    let report = rig.loader.unload("deco-fixture").expect("unload finds it");
    assert_eq!(
        report.decoration_sources, 1,
        "unload unregisters exactly the one producer"
    );
    assert!(
        rig.decorations.load().is_empty(),
        "the decoration registry is empty after unload — no dangling producer"
    );
    assert!(
        !rig.loader.is_loaded("deco-fixture"),
        "loader no longer tracks it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decorations_plugin_without_a_wired_registry_is_skipped_not_fatal() {
    let Some(wasm) = decorations_guest_wasm() else {
        eprintln!("skipping: decorations-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "deco-fixture", "decorations", &wasm);

    // No decoration registry wired — the producer has nowhere to register, so the
    // drain hits `NotWired("decorations")`, which `discover_and_load` logs +
    // skips (graceful degradation, never a boot abort or panic).
    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            decoration_registry: None,
            context_registry: None,
            theme_registry: Some(std::sync::Arc::new(
                lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette()),
            )),
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 0, "the decorations plugin is skipped, not loaded");
    assert!(!loader.is_loaded("deco-fixture"), "nothing recorded");
}
