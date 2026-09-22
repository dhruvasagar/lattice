//! A plugin is instantiated once per seam, and each instance gets its own host
//! id. The loader recorded the manifest name against the FIRST only, so a
//! contribution stamped by any later seam could not be named:
//! `:describe-key <C-c>a` showed org's binding as `<plugin:29>` while org's
//! grammar, from its first seam, rendered as `plugin:org`.
//!
//! The loader now tells the meta sink every seam id the plugin owns. This test
//! takes a real binding from a NON-first seam and follows its stamped id back
//! to the name — asserting on the binding the user would ask about, not on
//! the list of ids, so it fails if the aliases are recorded but are not the ids
//! bindings actually carry.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::{CommandRegistryHandle, SourceLayer};
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn auto_pair_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
    );
    std::fs::read(path).ok()
}

/// Resolves exactly as the host's registry does: a primary id names itself,
/// any other id goes through its alias.
#[derive(Default)]
struct ResolvingSink {
    names: Mutex<HashMap<u32, String>>,
    aliases: Mutex<HashMap<u32, u32>>,
}

impl ResolvingSink {
    fn name_of(&self, id: u32) -> Option<String> {
        let primary = self.aliases.lock().unwrap().get(&id).copied().unwrap_or(id);
        self.names.lock().unwrap().get(&primary).cloned()
    }
}

impl PluginMetaSink for ResolvingSink {
    fn register_plugin(&self, id: u32, name: String, _doc: String) {
        self.names.lock().unwrap().insert(id, name);
    }
    fn unregister_plugin(&self, id: u32) {
        self.names.lock().unwrap().remove(&id);
        self.aliases.lock().unwrap().retain(|_, p| *p != id);
    }
    fn register_seam_ids(&self, primary: u32, seam_ids: &[u32]) {
        let mut aliases = self.aliases.lock().unwrap();
        for &s in seam_ids.iter().filter(|&&s| s != primary) {
            aliases.insert(s, primary);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_binding_from_a_later_seam_is_named_as_its_plugin() {
    let Some(wasm) = auto_pair_wasm() else {
        eprintln!("skipping: auto-pair wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let dir = plugins_dir.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    // `grammar` first, so the `modes` seam — which binds `(` — is NOT the
    // plugin's primary id. That ordering is the whole bug.
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"auto-pair\"\nprovides = [\"grammar\", \"modes\", \"config\"]\neditor_capabilities = [\"tree-sitter\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    let keymap = KeymapHandle::new();
    let sink: Arc<ResolvingSink> = Arc::new(ResolvingSink::default());
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            help_topics: Some(lattice_help::topics::builtin_topics().into_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(keymap.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );
    assert_eq!(
        loader
            .discover_and_load(&plugins_dir, TrustTier::Bundled)
            .await,
        1
    );

    let open = lattice_protocol::parse_chord_sequence("(").unwrap();
    let LookupResult::Bound { command, .. } =
        keymap.lookup_with_context(BindingMode::Insert, &open, &[ModeId::new("auto-pair-mode")])
    else {
        panic!("the modes seam binds `(` in auto-pair-mode");
    };
    let SourceLayer::Plugin(stamped) = command.source.layer else {
        panic!("a plugin's binding is stamped as the plugin's")
    };

    // Precondition, so the assertion below is about the bug and not a
    // coincidence: the binding really does carry a non-primary id.
    assert!(
        !sink.names.lock().unwrap().contains_key(&stamped),
        "`(` came from the modes seam, which is not the primary id — if this \
         fails, the fixture no longer exercises a later seam"
    );
    assert_eq!(
        sink.name_of(stamped).as_deref(),
        Some("auto-pair"),
        "the id the binding carries resolves to the plugin's name, not \
         `<plugin:{stamped}>`"
    );
}
