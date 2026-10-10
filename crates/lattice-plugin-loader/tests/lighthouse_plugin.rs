//! LH.1.3 — lighthouse through the real loader, with the manifest it ships.
//!
//! `lattice-plugin-host/tests/lighthouse_install.rs` drives the component's
//! seams one at a time, with a manifest the test wrote. This is the other
//! half: the loader reading the REAL `plugin.toml`, draining every seam it
//! lists in the order it lists them, into the registries the editor reads.
//!
//! What only this can show:
//!
//! * the shipped manifest parses and the whole plugin loads — an unwired seam
//!   or a capability the tier refuses fails the load, and the symptom in the
//!   editor would be a set of commands that simply are not there;
//! * `provides` has `grammar` before `modes`. The mode's chords name this
//!   plugin's own actions, and a binding to a name that does not exist yet is
//!   dropped silently, by design;
//! * which LAYER the chords land in. `i`, `u` and `x` are among the most
//!   used keys in the editor; in `Builtin` they would stop inserting,
//!   undoing and deleting everywhere. They must exist in
//!   `lighthouse-servers-mode`'s own layer and nowhere else.
//!
//! Skips when the component was not built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::{KeymapHandle, KeymapLayer};
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

const SERVERS_MODE: &str = "lighthouse-servers-mode";

fn plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/lighthouse/target/wasm32-wasip2/release/lighthouse.wasm"
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

/// Write the plugin out the way it ships: the real manifest, so `provides`
/// ordering and the capability list are the ones under test.
fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("lighthouse");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/lighthouse/plugin.toml"
    ))
    .unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

struct Rig {
    loader: PluginLoader,
    keymap: KeymapHandle,
    commands: CommandRegistryHandle,
    modes: ModeRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let keymap = KeymapHandle::new();
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands.clone()),
            mode_registry: Some(modes.clone()),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(keymap.clone()),
            tracer: None,
            meta_sink: Some(Arc::new(RecordingSink::default()) as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );
    Rig {
        loader,
        keymap,
        commands,
        modes,
    }
}

async fn loaded() -> Option<(tempfile::TempDir, Rig)> {
    let wasm = plugin_wasm()?;
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);
    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "lighthouse loads, whole, from its shipped manifest");
    Some((base, rig))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_command_is_registered() {
    let Some((_base, rig)) = loaded().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let commands = rig.commands.load();
    for name in [
        "lsp-install",
        "lsp-uninstall",
        "lsp-update",
        "lsp-update-all",
        "lsp-servers",
    ] {
        assert!(
            commands.lookup_by_name(name).is_some(),
            "`:{name}` is registered"
        );
    }
}

/// The chords of `*lsp-servers*` are in that buffer's mode, and only there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_row_chords_are_bound_in_the_servers_modes_own_layer() {
    let Some((_base, rig)) = loaded().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let commands = rig.commands.load();
    let own_layer = KeymapLayer::MinorMode(ModeId::new(SERVERS_MODE));

    for action in [
        "lsp-servers-install",
        "lsp-servers-update",
        "lsp-servers-uninstall",
        "lsp-servers-log",
        "lsp-servers-refresh",
    ] {
        let id = commands
            .lookup_by_name(action)
            .unwrap_or_else(|| panic!("the action `{action}` is registered"))
            .id;
        let bindings = rig.keymap.reverse_entries(id);
        let shown: Vec<String> = bindings
            .iter()
            .map(|(chord, layer)| format!("{chord:?} in {layer:?}"))
            .collect();
        assert!(
            !bindings.is_empty(),
            "`{action}` has a chord — a binding to a name that did not exist \
             when the mode registered is dropped without a word, which is \
             what `provides` listing `modes` before `grammar` would do"
        );
        assert!(
            bindings.iter().all(|(_, layer)| *layer == own_layer),
            "`{action}` is bound only in {SERVERS_MODE}'s layer — anywhere \
             else and `i` / `u` / `x` stop being vim's: {shown:?}"
        );
    }

    let bound = rig
        .keymap
        .layer_bindings(own_layer, lattice_keymap::BindingMode::Normal);
    assert_eq!(
        bound.len(),
        5,
        "five chords and no more: i, u, x, <CR>, gr — {bound:?}"
    );
}

/// Manual, so the chords exist on the one buffer the plugin activates the
/// mode on. A universal or global minor would put `x` = uninstall in every
/// buffer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_servers_mode_is_a_manual_minor() {
    let Some((_base, rig)) = loaded().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let modes = rig.modes.load();
    let mode = modes
        .get(ModeId::new(SERVERS_MODE))
        .expect("lighthouse-servers-mode is registered");
    assert_eq!(mode.kind(), lattice_mode::ModeKind::Minor);
    assert!(
        matches!(
            mode.activation_policy(),
            lattice_mode::ActivationPolicy::Manual
        ),
        "activated only where the plugin asks: {:?}",
        mode.activation_policy()
    );
}
