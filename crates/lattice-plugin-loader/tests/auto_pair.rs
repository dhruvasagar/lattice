//! AP.1 end-to-end: the bundled `auto-pair` plugin — a single multi-seam
//! component (grammar + modes + config) — is discovered on disk and loaded
//! through the real loader, and ALL THREE seams' contributions register:
//!   - the pairing **actions** land in the command registry under
//!     `SourceLayer::Plugin` provenance,
//!   - `auto-pairs-mode` registers into the mode registry and OWNS its
//!     insert-mode keymap (bindings resolve only when the mode is active),
//!   - the `auto-pairs-style` / `auto-pairs-close-key` **options** register into
//!     the shared config registry.
//!
//! This is the AP.1.0 spike made real through the production loader path: the
//! same `.wasm` is instantiated once per seam (grammar sync / modes+config
//! async) against the superset linkers. Skips when the plugin wasn't built (no
//! `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

/// The auto-pair component, if the `wasm32-wasip2` build produced it. The loader
/// crate can't read the host crate's build-script env var, so resolve by the
/// known path and skip if absent (the mode/config-drain precedent).
fn auto_pair_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
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

fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    // `grammar` BEFORE `modes`: the mode keymap binds to the plugin's own grammar
    // actions by name, resolved at bind time — so the grammar drain must run
    // first (the real `plugin.toml` orders them the same way).
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"auto-pair\"\nprovides = [\"grammar\", \"modes\", \"config\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

fn empty_command_registry() -> CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_auto_pair_registers_grammar_modes_and_config_through_the_loader() {
    let Some(wasm) = auto_pair_wasm() else {
        eprintln!("skipping: auto-pair wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let command_registry = empty_command_registry();
    let mode_registry = empty_mode_registry();
    let config_registry = Arc::new(ConfigRegistry::default());
    let keymap = KeymapHandle::new();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let host =
        Arc::new(PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap());
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry.clone()),
            mode_registry: Some(mode_registry.clone()),
            config_registry: Some(config_registry.clone()),
            keymap: Some(keymap.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the multi-seam auto-pair plugin loads");
    assert!(loader.is_loaded("auto-pair"), "loader tracks it loaded");

    // 1. GRAMMAR — the pairing actions registered (sync drain).
    let commands = command_registry.load();
    for action in ["auto-pair-open-round", "auto-pair-close-round", "auto-pair-backspace"] {
        assert!(
            commands.id_by_name(action).is_some(),
            "the grammar action `{action}` registered from the plugin"
        );
    }

    // 2. MODES — the minor mode registered (async drain).
    let modes = mode_registry.load();
    assert!(
        modes.is_registered(ModeId::new("auto-pairs-mode")),
        "auto-pairs-mode registered into the published mode registry"
    );

    // The mode OWNS its insert-mode keymap: `(` resolves only when the mode is
    // active, never globally (mode-ownership — a gated MinorMode layer).
    let open = lattice_protocol::parse_chord_sequence("(").expect("chord parses");
    let mode = ModeId::new("auto-pairs-mode");
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Insert, &open, &[mode.clone()]),
            LookupResult::Bound { .. }
        ),
        "`(` binds to the plugin's open action when auto-pairs-mode is active"
    );
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Insert, &open, &[]),
            LookupResult::Unbound
        ),
        "the gated binding does not fire when the mode is inactive"
    );

    // 3. CONFIG — the options registered (async drain).
    for option in ["auto-pairs-style", "auto-pairs-close-key"] {
        assert!(
            config_registry.lookup(option).is_some(),
            "the option `{option}` registered from the plugin"
        );
    }

    // Provenance recorded for `:list-plugins`.
    let recorded = sink.registered.lock().unwrap();
    assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
    assert_eq!(recorded[0].1, "auto-pair", "under its manifest id");
}
