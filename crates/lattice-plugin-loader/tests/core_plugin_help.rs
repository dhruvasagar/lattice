//! The core plugins ship their own `:help` pages (CR.3, dogfooded).
//!
//! `auto-pair` and `treesitter-context` are the first real consumers of the
//! `help` seam. Their manuals used to live in `docs/user/core-plugins.md`,
//! which meant a *plugin's* documentation was compiled into the *editor's*
//! binary — the exact coupling CR.3 exists to break. Now each plugin
//! `include_str!`s its own `doc/` into its component and registers it at load.
//!
//! This test loads the REAL plugin `.wasm`s, not fixtures. That matters: a
//! fixture would prove the seam works (which `help_drain.rs` already does) and
//! say nothing about whether the shipping plugins actually use it. A core
//! plugin that silently stopped registering its page would be invisible
//! otherwise, because nothing else reads it.
//!
//! Skips when the plugins were not built for `wasm32-wasip2`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_help::topics::HelpTopicRegistryHandle;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

/// Each core plugin: manifest id, its built artefact, and a phrase its own
/// manual contains. The phrase is checked so the assertion fails if the page
/// registers but is empty or is somebody else's.
const CORE_PLUGINS: &[(&str, &str, &str)] = &[
    (
        "auto-pair",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
        ),
        "auto-pair.style",
    ),
    (
        "treesitter-context",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/treesitter-context/target/wasm32-wasip2/release/treesitter_context.wasm"
        ),
        "sticky",
    ),
];

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

fn write_plugin_dir(root: &std::path::Path, id: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    // Only the `help` seam is exercised here; the plugin's other seams have
    // their own tests, and naming just this one keeps the harness small.
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"help\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

struct Rig {
    loader: PluginLoader,
    help: HelpTopicRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    let help: HelpTopicRegistryHandle = lattice_help::topics::builtin_topics().into_handle();
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let host = Arc::new(
        lattice_plugin_host::PluginHost::with_dirs(base.join("cache"), base.join("data"))
            .expect("host builds"),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            help_topics: Some(help.clone()),
            ..Default::default()
        },
    );
    Rig { loader, help }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_core_plugin_ships_its_own_help_page() {
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");

    let mut loaded = Vec::new();
    for (id, path, _) in CORE_PLUGINS {
        match std::fs::read(path) {
            Ok(wasm) => {
                write_plugin_dir(&plugins_dir, id, &wasm);
                loaded.push(*id);
            }
            Err(_) => eprintln!("skipping {id}: not built for wasm32-wasip2"),
        }
    }
    if loaded.is_empty() {
        eprintln!("skipping: no core plugin built");
        return;
    }

    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, loaded.len(), "every built core plugin loaded");

    let topics = rig.help.load();
    for (id, _, phrase) in CORE_PLUGINS {
        if !loaded.contains(id) {
            continue;
        }
        // The BARE plugin id — a core plugin's page answers to `:help
        // auto-pair`, not `:help auto-pair.auto-pair`.
        let topic = topics.lookup(id).unwrap_or_else(|| {
            panic!(
                "core plugin `{id}` registered no `:help {id}` page; registered: {:?}",
                topics.names().collect::<Vec<_>>()
            )
        });
        let body = topic.body.render();
        assert!(
            body.to_lowercase().contains(&phrase.to_lowercase()),
            "`:help {id}` does not contain `{phrase}` — the page registered but \
             is empty or is the wrong doc"
        );
        assert!(
            !topic.summary.trim().is_empty(),
            "`:help {id}` has no summary, so it reads as a blank row in \
             `:help <Tab>` and in the topic index"
        );
    }
}

/// The coupling CR.3 broke, pinned so it cannot come back: a core plugin's
/// manual must NOT also be a lattice-owned doc, or it is embedded in the
/// editor binary as well as in the plugin and the two drift.
#[test]
fn a_core_plugins_manual_is_not_also_embedded_in_the_editor() {
    let builtins = lattice_help::topics::builtin_topics();
    for (id, _, _) in CORE_PLUGINS {
        assert!(
            builtins.lookup(id).is_none(),
            "`{id}` is a builtin topic AND a core-plugin topic — the plugin's \
             manual is being compiled into the editor binary too. Move it to \
             the plugin's own `doc/`."
        );
    }
}
