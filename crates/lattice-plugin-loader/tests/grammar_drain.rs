//! PL8.B end-to-end: a grammar plugin discovered on disk loads at boot and its
//! motions / text-objects become reachable in the (runtime-mutable) command
//! registry, dispatchable through the real grammar dispatcher, with provenance
//! recorded for `:list-plugins`.
//!
//! Uses the canonical `grammar-guest` fixture the runtime crate builds to a
//! `wasm32-wasip2` component (registers `down-n` + `to-cursor` + `fails`). Skips
//! when that component was not built (no `wasm32-wasip2` target) — the same
//! graceful skip the runtime crate's grammar tests use.
//!
//! The grammar seam is the **synchronous** one: registration puts the guest's
//! sync trampolines into the command registry, and `execute_motion_only` fires
//! them on the (test-driven) keystroke path. This proves B3a/B3b end to end — a
//! plugin motion registered at runtime dispatches through the wait-free `.load()`
//! snapshot the `DocumentActor` reads.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_core::buffer::Buffer;
use lattice_core::buffers::BufferId;
use lattice_grammar::command::{CommandInvocation, Count};
use lattice_grammar::dispatcher::execute_motion_only;
use lattice_grammar::registry::{CommandRegistry, GrammarEnv};
use lattice_grammar::source::SourceLayer;
use lattice_grammar::{CancellationToken, CommandRegistryHandle};
use lattice_mode::PluginMetaSink;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;

/// The grammar fixture component, if the `wasm32-wasip2` build produced it. The
/// loader crate can't read the runtime crate's build-script env var, so resolve
/// the artifact by its known path and skip if absent.
fn grammar_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/grammar-guest/target/wasm32-wasip2/release/grammar_guest.wasm"
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

/// A fresh (empty) runtime-mutable command registry so the plugin's grammar is
/// the only *plugin*-sourced entries (builtins are still present — the guest's
/// specs stamp `SourceLayer::Plugin`, which the assertions key on).
fn empty_registry_handle() -> CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_grammar_plugin_registers_and_dispatches_through_the_registry() {
    let Some(wasm) = grammar_guest_wasm() else {
        eprintln!("skipping: grammar-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "grammar-fixture", "grammar", &wasm);

    let command_registry = empty_registry_handle();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::GutterDecorationSourceRegistry::default(),
            ))),
            context_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::ContextSourceRegistry::new(),
            ))),
            ..Default::default()
        },
    );

    // Sanity: discovery finds exactly the one plugin dir.
    assert_eq!(
        discover(&plugins_dir).len(),
        1,
        "discovery finds the plugin"
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the grammar plugin loads");
    assert!(
        loader.is_loaded("grammar-fixture"),
        "loader tracks it loaded"
    );

    // Provenance recorded — `:list-plugins` would show it.
    let plugin_id = {
        let recorded = sink.registered.lock().unwrap();
        assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
        assert_eq!(recorded[0].1, "grammar-fixture", "under its manifest id");
        recorded[0].0
    };

    // The guest's three contributions are now live in the registry, each stamped
    // with unforgeable host-issued `SourceLayer::Plugin` provenance.
    let snapshot = command_registry.load();
    for name in ["down-n", "to-cursor", "fails"] {
        let id = snapshot
            .id_by_name(name)
            .unwrap_or_else(|| panic!("{name} registered into the live registry"));
        assert_eq!(
            snapshot.lookup(id).unwrap().source.layer,
            SourceLayer::Plugin(plugin_id),
            "{name} stamped Plugin({plugin_id}) provenance"
        );
    }

    // The plugin motion dispatches through the sync trampoline off the wait-free
    // snapshot — exactly the `DocumentActor`'s per-keystroke read path (B3b).
    // `down-n` returns cursor.line + count; from line 1 with count 3 → line 4.
    let motion_id = snapshot.id_by_name("down-n").unwrap();
    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    let target = execute_motion_only(
        &snapshot,
        &buffer,
        BufferId(1),
        Position { line: 1, byte: 0 },
        CommandInvocation::of(motion_id).with_count(Count(3)),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("plugin motion dispatches through the sync trampoline");
    assert_eq!(target, Position { line: 4, byte: 0 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grammar_plugin_without_a_wired_command_registry_is_skipped_not_fatal() {
    let Some(wasm) = grammar_guest_wasm() else {
        eprintln!("skipping: grammar-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "grammar-fixture", "grammar", &wasm);

    // A loader with the event bus wired but NO command registry — the grammar
    // drain hits `PluginLoaderError::NotWired("grammar")`, which
    // `discover_and_load` logs + skips (never a panic, never a boot abort). This
    // is the graceful-degradation contract: a missing registry service degrades
    // to "grammar plugin skipped", not a crash.
    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: None,
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 0, "the grammar plugin is skipped, not loaded");
    assert!(!loader.is_loaded("grammar-fixture"), "nothing recorded");
}
