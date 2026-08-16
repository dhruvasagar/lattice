//! PL8.D.2 end-to-end: a keymap plugin discovered on disk binds user keybindings
//! into `KeymapLayer::User` at load, and unloading it unbinds them (the teardown
//! surface `PluginTeardown` grew for the keymap seam). Skips when the fixture
//! wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn keymap_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/keymap-guest/target/wasm32-wasip2/release/keymap_guest.wasm"
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

/// A command registry (behind the handle) with builtin ex-commands populated, so
/// the fixture's `<C-s>` → `ex:write` binding resolves.
fn commands_with_builtins() -> CommandRegistryHandle {
    let mut r = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut r);
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_keymap_plugin_binds_then_unbinds_on_unload() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("plugins");
    write_plugin_dir(&dir, "keymap-fixture", "keymap", &wasm);

    let keymap = KeymapHandle::new();
    let commands = commands_with_builtins();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands.clone()),
            keymap: Some(keymap.clone()),
            // The teardown driver needs the full registry set.
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                PickerRegistry::new(),
            ))),
            mode_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
                    as ModeRegistryHandle,
            ),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::GutterDecorationSourceRegistry::default(),
            ))),
            context_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::ContextSourceRegistry::new(),
            ))),
            tracer: None,
        },
    );

    assert_eq!(loader.discover_and_load(&dir, TrustTier::Bundled).await, 1);
    assert!(loader.is_loaded("keymap-fixture"));

    // The fixture's `<C-s>` → `ex:write` binding is live in the User layer (the
    // `gq` → unregistered one was gracefully skipped, so exactly one binding).
    assert_eq!(keymap.binding_count(), 1, "one user binding landed");
    let chord = lattice_protocol::parse_chord_sequence("<C-s>").unwrap();
    assert!(
        matches!(
            keymap.lookup(BindingMode::Normal, &chord),
            LookupResult::Bound { .. }
        ),
        "the plugin's user keybinding resolves"
    );

    // Unload reverses it: the User-layer binding is unbound.
    let report = loader.unload("keymap-fixture").expect("was loaded");
    assert_eq!(
        report.keymap_bindings, 1,
        "the one user binding was unbound"
    );
    assert_eq!(keymap.binding_count(), 0, "no binding remains after unload");
    assert!(
        matches!(
            keymap.lookup(BindingMode::Normal, &chord),
            LookupResult::Unbound
        ),
        "the binding no longer resolves"
    );
    assert!(!loader.is_loaded("keymap-fixture"));
}
