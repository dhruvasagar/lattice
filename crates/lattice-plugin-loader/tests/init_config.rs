//! PL8.D.3 end-to-end: the user's `init.rs` is just a plugin loaded from
//! `<config>/lattice/init/` with a boot-capability (`Bundled`) tier. This drives
//! that spine with the keymap fixture standing in for a real init.rs: load the
//! init dir via `load_path`, assert its keybinding lands, then exercise the
//! `:reload-config` path (reload the `init` plugin) and assert it survives.
//! Skips when the keymap fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, PluginLoaderHandle};
use lattice_runtime::EventBus;

fn keymap_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/keymap-guest/target/wasm32-wasip2/release/keymap_guest.wasm"
    );
    std::fs::read(path).ok()
}

#[derive(Default)]
struct Sink;
impl PluginMetaSink for Sink {
    fn register_plugin(&self, _id: u32, _name: String, _doc: String) {}
    fn unregister_plugin(&self, _id: u32) {}
}

fn commands_with_builtins() -> CommandRegistryHandle {
    let mut r = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut r);
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

/// Write an `init/` dir: `plugin.toml` (`id = "init"`, `provides = ["keymap"]`) +
/// the component — the shape `<config>/lattice/init/` holds.
fn write_init_dir(dir: &std::path::Path, wasm: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"keymap\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("init.wasm"), wasm).unwrap();
}

fn loader(base: &std::path::Path, keymap: KeymapHandle) -> PluginLoaderHandle {
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap(),
    );
    Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands_with_builtins()),
            keymap: Some(keymap),
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()))),
            mode_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default())) as ModeRegistryHandle),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            meta_sink: Some(Arc::new(Sink) as Arc<dyn PluginMetaSink>),
        },
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_loads_as_a_plugin_and_survives_reload_config() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let loader = loader(base.path(), keymap.clone());

    // The boot path: `load_path(<config>/lattice/init, Bundled)`.
    let id = loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init.rs loads from the config dir");
    assert!(loader.is_loaded("init"), "loaded under its `init` manifest id");
    assert_eq!(keymap.binding_count(), 1, "init's keybinding is live");
    let chord = lattice_protocol::parse_chord_sequence("<C-s>").unwrap();
    assert!(matches!(
        keymap.lookup(BindingMode::Normal, &chord),
        LookupResult::Bound { .. }
    ));

    // `:reload-config` → `reload("init")`: unbinds the old binding + re-binds
    // from disk. Still exactly one binding (no accumulation), still loaded.
    let new_id = loader
        .reload("init", TrustTier::Bundled)
        .await
        .expect("reload-config re-instantiates init from disk");
    assert!(loader.is_loaded("init"), "init still loaded after reload-config");
    assert_ne!(new_id.0, id.0, "a fresh host id (fresh Store) after reload");
    assert_eq!(
        keymap.binding_count(),
        1,
        "reload unbound the old binding and re-bound once — no accumulation"
    );
    assert!(matches!(
        keymap.lookup(BindingMode::Normal, &chord),
        LookupResult::Bound { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_init_loads_when_absent_then_reloads_when_present() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let loader = loader(base.path(), keymap.clone());

    // Not loaded → sync_init loads.
    assert!(!loader.is_loaded("init"));
    let id1 = loader.sync_init(&init_dir, TrustTier::Bundled).await.unwrap();
    assert!(loader.is_loaded("init"));
    assert_eq!(keymap.binding_count(), 1);

    // Loaded → sync_init reloads (fresh id, no binding accumulation).
    let id2 = loader.sync_init(&init_dir, TrustTier::Bundled).await.unwrap();
    assert!(loader.is_loaded("init"));
    assert_ne!(id1.0, id2.0, "reload minted a fresh Store/id");
    assert_eq!(keymap.binding_count(), 1, "reload did not accumulate bindings");
}
