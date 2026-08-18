//! TC.2 end-to-end: a context plugin discovered on disk loads at boot and its
//! context-scope producer becomes reachable through the runtime-mutable
//! [`ContextSourceRegistry`] — the exact handle the host's reparse-driven
//! refresh will read to fill the per-buffer scope cache the resolver reads.
//!
//! Uses the canonical `context-guest` fixture the plugin-host crate builds to a
//! `wasm32-wasip2` component (one scope per named child of the tree root).
//! Skips when that component was not built. The async-produce path itself
//! (guest `context-scopes` → native `ContextScope`, including the
//! `borrow<tree-snapshot>` crossing an async suspension) is proven by
//! `lattice-plugin-host`; this test proves the **drain** — that `provides =
//! ["context"]` registers the producer into the context registry, that it is
//! callable, and that it is reversed on unload.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ContextSourceRegistry, ContextSourceRegistryHandle, GutterDecorationSourceRegistry,
    ModeRegistry, ModeRegistryHandle, PluginMetaSink,
};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;
use lattice_syntax::{Lang, Syntax};

fn context_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/context-guest/target/wasm32-wasip2/release/context_guest.wasm"
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
        // TS.1: the context seam is gated on `tree-sitter`, so a producer that
        // wants a tree must ASK for it — the real bundled plugin does the same
        // in its own manifest. Without this the producer loads and runs and is
        // simply handed `none`, which is the gate working.
        format!(
            "id = \"{id}\"\nprovides = [\"{provides}\"]\n\
             editor_capabilities = [\"tree-sitter\"]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn temp_host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

/// A fully-wired loader over hermetic (empty) registries — the context registry
/// the drain registers into, plus every other registry `run_teardown` needs so
/// `unload` reverses (not the missing-handle no-op path).
struct Rig {
    loader: PluginLoader,
    contexts: ContextSourceRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    let contexts: ContextSourceRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ContextSourceRegistry::new()));
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
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            decoration_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                GutterDecorationSourceRegistry::new(),
            ))),
            context_registry: Some(contexts.clone()),
            theme_registry: Some(std::sync::Arc::new(
                lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette()),
            )),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
        },
    );
    Rig { loader, contexts }
}

/// Three top-level items → three named children of the root.
const SRC: &str = "fn a() {\n    let x = 1;\n}\n\nstruct S {\n    f: u32,\n}\n\nfn b() {}\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_context_plugin_registers_a_callable_producer() {
    let Some(wasm) = context_guest_wasm() else {
        eprintln!("skipping: context-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "ctx-fixture", "context", &wasm);

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
    assert_eq!(n, 1, "the context plugin loads");
    assert!(
        rig.loader.is_loaded("ctx-fixture"),
        "loader tracks it loaded"
    );

    // The producer is registered into the wait-free context registry — the
    // handle the host's reparse-driven refresh reads.
    let sources = rig.contexts.load().sources();
    assert_eq!(
        sources.len(),
        1,
        "exactly the plugin's producer is registered"
    );

    // It is callable off the render path, through the NATIVE trait — the host
    // never names the WASM type. The type-erased snapshot is downcast by the
    // adapter, so passing a real `SyntaxSnapshot` here proves that hop too.
    let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syn.parse(SRC);
    let snapshot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(syn.snapshot_owned());

    let scopes = sources[0]
        .produce(7, None, SRC.lines().count() as u32, Some(snapshot))
        .await
        .expect("producer yields scopes for a parsed buffer");
    assert_eq!(scopes.len(), 3, "one scope per top-level item: {scopes:?}");
    assert_eq!((scopes[0].scope_start, scopes[0].scope_end), (0, 2));
    assert_eq!((scopes[2].scope_start, scopes[2].scope_end), (8, 8));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_a_context_plugin_reverses_its_producer() {
    let Some(wasm) = context_guest_wasm() else {
        eprintln!("skipping: context-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "ctx-fixture", "context", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(rig.contexts.load().sources().len(), 1);

    let report = rig
        .loader
        .unload("ctx-fixture")
        .expect("the plugin was loaded, so unload reports");
    assert_eq!(
        report.context_sources, 1,
        "teardown reverses exactly the one context producer"
    );
    assert!(
        rig.contexts.load().is_empty(),
        "the registry is empty again — a reload must not accumulate duplicates"
    );
}
