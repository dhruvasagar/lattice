//! PL8.C: unload + reload — the loaded-plugin lifecycle. Loads real fixture
//! plugins, then reverses every registry contribution via `PluginTeardown`
//! (unload) and re-instantiates from disk (reload), asserting the registries are
//! left exactly as teardown promises. Skips when the `wasm32-wasip2` fixtures
//! weren't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::source::SourceLayer;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::{PickerRegistry, PickerRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn fixture(rel: &str) -> Option<Vec<u8>> {
    let path = format!(
        "{}/../lattice-plugin-host/tests/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        rel
    );
    std::fs::read(path).ok()
}

fn picker_wasm() -> Option<Vec<u8>> {
    fixture("picker-guest/target/wasm32-wasip2/release/picker_guest.wasm")
}
fn grammar_wasm() -> Option<Vec<u8>> {
    fixture("grammar-guest/target/wasm32-wasip2/release/grammar_guest.wasm")
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

/// A fully-wired loader over hermetic (empty) registries — everything
/// `run_teardown` needs. Returns the loader plus the handles the assertions read.
struct Rig {
    loader: PluginLoader,
    commands: CommandRegistryHandle,
    pickers: PickerRegistryHandle,
    modes: ModeRegistryHandle,
    sink: Arc<RecordingSink>,
}

fn rig(base: &std::path::Path) -> Rig {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers.clone()),
            command_registry: Some(commands.clone()),
            mode_registry: Some(modes.clone()),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(lattice_mode::GutterDecorationSourceRegistry::default()))),
            tracer: None,
        },
    );
    Rig {
        loader,
        commands,
        pickers,
        modes,
        sink,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unload_reverses_picker_and_grammar_contributions() {
    let (Some(picker), Some(grammar)) = (picker_wasm(), grammar_wasm()) else {
        eprintln!("skipping: picker/grammar fixtures not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("plugins");
    write_plugin_dir(&dir, "picker-fixture", "picker-source", &picker);
    write_plugin_dir(&dir, "grammar-fixture", "grammar", &grammar);

    let r = rig(base.path());
    assert_eq!(r.loader.discover_and_load(&dir, TrustTier::Bundled).await, 2);

    // Both contributions are live.
    assert!(r.pickers.load().get("fixture").is_some(), "picker source live");
    assert!(
        r.commands.load().id_by_name("down-n").is_some(),
        "grammar motion live"
    );
    assert_eq!(r.sink.registered.lock().unwrap().len(), 2, "both provenance");

    // Unload the picker plugin — its source is gone, the grammar plugin untouched.
    let report = r.loader.unload("picker-fixture").expect("picker was loaded");
    assert_eq!(report.pickers, 1, "one picker source reversed");
    assert!(r.pickers.load().get("fixture").is_none(), "picker source gone");
    assert!(!r.loader.is_loaded("picker-fixture"), "record removed");
    assert!(
        r.commands.load().id_by_name("down-n").is_some(),
        "grammar plugin untouched by the picker unload"
    );

    // Unload the grammar plugin — its provenance-stamped motion is gone.
    let plugin_id = r.commands.load().id_by_name("down-n").and_then(|id| {
        r.commands.load().lookup(id).map(|m| match m.source.layer {
            SourceLayer::Plugin(p) => p,
            _ => panic!("plugin provenance"),
        })
    });
    let report = r.loader.unload("grammar-fixture").expect("grammar was loaded");
    assert_eq!(
        report.commands, 3,
        "all three grammar contributions (down-n / to-cursor / fails) reversed"
    );
    assert!(
        r.commands.load().id_by_name("down-n").is_none(),
        "grammar motion gone after unload"
    );
    assert!(plugin_id.is_some(), "sanity: motion had plugin provenance");

    // Everything unloaded — the loaded set + provenance sink are empty.
    assert_eq!(r.loader.loaded_count(), 0);
    assert!(r.sink.registered.lock().unwrap().is_empty(), "provenance cleared");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_reinstantiates_from_disk() {
    let Some(picker) = picker_wasm() else {
        eprintln!("skipping: picker fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("plugins");
    write_plugin_dir(&dir, "picker-fixture", "picker-source", &picker);

    let r = rig(base.path());
    assert_eq!(r.loader.discover_and_load(&dir, TrustTier::Bundled).await, 1);
    assert!(r.pickers.load().get("fixture").is_some());

    // Reload: unload + re-load from the recorded source dir. Still loaded, and
    // the source is freshly registered.
    let id = r
        .loader
        .reload("picker-fixture", TrustTier::Bundled)
        .await
        .expect("reload succeeds from the on-disk source");
    assert!(r.loader.is_loaded("picker-fixture"), "loaded again after reload");
    assert_eq!(r.loader.loaded_count(), 1, "no duplicate record");
    assert!(
        r.pickers.load().get("fixture").is_some(),
        "the source is live again after reload"
    );
    assert_eq!(id.0, r.sink.registered.lock().unwrap()[0].0, "fresh id recorded");
    // `modes` handle is unused here but kept so the rig wires a full registry set.
    let _ = &r.modes;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unload_unknown_target_is_none_and_reload_errors() {
    let base = tempfile::tempdir().unwrap();
    let r = rig(base.path());
    assert!(r.loader.unload("nope").is_none(), "no such plugin → None");
    assert!(
        r.loader.reload("nope", TrustTier::Bundled).await.is_err(),
        "reload of an unknown target errors"
    );
}
