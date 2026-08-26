//! TR.2b end-to-end: a transient plugin discovered on disk loads, and its menu
//! becomes reachable through the SAME `TransientSourceRegistry` magit's menus
//! live in — built through the same `Effect::OpenTransient` name, and reversed
//! on unload.
//!
//! Two things are load-bearing here and neither is provable from the host-side
//! seam test:
//!
//! 1. **The menu is registered under the name the GUEST chose.** The loader
//!    asks `id()` once and keys the registry entry on the answer, so
//!    `Effect::OpenTransient { source: "fixture-capture" }` reaches a plugin
//!    menu without the host knowing anything about it.
//! 2. **Unload really unregisters it.** The entry holds a `TransientClient`
//!    whose actor ends with the plugin; leaving the name registered would turn
//!    a later chord into a host error ("plugin gone") instead of the honest
//!    "unknown source". That reversal lives on the loader side — the registry
//!    is `Arc`-shared, not one of `TeardownRegistries`' `&mut` snapshots —
//!    which is exactly the placement that is easy to forget.
//!
//! Skips when the `transient-guest` fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::source::PickerRegistry;
use lattice_picker::{
    PickerRegistryHandle, TransientBuild, TransientContext, TransientItemKind,
    TransientSourceRegistry, TransientSourceRegistryHandle,
};
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

/// The name the fixture's `id()` returns.
const MENU: &str = "fixture-capture";
/// The command its good rows name; the ghost row's is deliberately absent.
const COMMAND: &str = "fixture-capture-key";

fn transient_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/transient-guest/target/wasm32-wasip2/release/transient_guest.wasm"
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

struct Rig {
    loader: PluginLoader,
    transients: TransientSourceRegistryHandle,
    commands: CommandRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    // The command the fixture's rows name, registered natively. The ghost row's
    // command is deliberately NOT here, so the drop-a-row rule is exercised.
    let mut cmd = CommandRegistry::new();
    cmd.register_action(
        COMMAND,
        "fixture capture (test)",
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: Arc::new(|_| Ok(lattice_grammar::Effect::None)),
        },
    );
    let commands: CommandRegistryHandle = Arc::new(arc_swap::ArcSwap::from_pointee(cmd));

    let transients: TransientSourceRegistryHandle = Arc::new(TransientSourceRegistry::new());
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
            command_registry: Some(commands.clone()),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            transient_registry: Some(transients.clone()),
            ..Default::default()
        },
    );
    Rig {
        loader,
        transients,
        commands,
    }
}

fn in_org() -> TransientContext {
    TransientContext {
        major_mode: Some("org-mode".into()),
        minor_modes: vec!["org-global-mode".into()],
        buffer: None,
    }
}

/// Build the registered menu the way `Editor::open_named_transient` does: ask
/// the registry, expect a `Future`, drive it.
async fn open(rig: &Rig, name: &str) -> Option<Result<lattice_picker::TransientSpec, String>> {
    match rig.transients.build(name, &in_org())? {
        TransientBuild::Future(fut) => Some(fut.await),
        TransientBuild::Ready(_) => {
            panic!("a guest-backed menu must answer Future, not Ready")
        }
    }
}

/// A discovered transient plugin registers its menu under the name its own
/// `id()` returned, and the menu builds through the registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_menu_registers_under_the_name_the_guest_chose() {
    let Some(wasm) = transient_guest_wasm() else {
        eprintln!("skipping: transient-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "transient-guest", "transient-source", &wasm);

    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the transient plugin loads");

    let spec = open(&rig, MENU)
        .await
        .expect("the menu is registered")
        .expect("and it builds");

    // The title echoes the projection, so this proves the OPEN CONTEXT reached
    // the guest through the registry — not merely that a menu appeared.
    assert_eq!(spec.title, "org-mode (1 minors)");

    // Two rows naming the same command with different args — the property the
    // seam exists for — plus the dismiss, and NOT the ghost row.
    let keys: Vec<&str> = spec.groups[0]
        .items
        .iter()
        .map(|i| i.key[0].as_str())
        .collect();
    assert_eq!(
        keys,
        vec!["t", "n", "q"],
        "the row naming an unregistered command was dropped and the rest kept"
    );

    let expected = rig.commands.load().id_by_name(COMMAND).unwrap();
    let args: Vec<String> = spec.groups[0]
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            TransientItemKind::Action { command, args } => {
                assert_eq!(*command, expected, "the row's command name resolved");
                Some(format!("{args:?}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(args.len(), 2);
    assert!(args[0].contains("todo") && args[1].contains("note"));
}

/// Unload really withdraws the name. Without the loader-side reversal the entry
/// would survive holding a client whose actor has ended, and the chord would
/// report a host error rather than "unknown source".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_withdraws_the_menu_name() {
    let Some(wasm) = transient_guest_wasm() else {
        eprintln!("skipping: transient-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "transient-guest", "transient-source", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert!(open(&rig, MENU).await.is_some(), "registered while loaded");

    let report = rig
        .loader
        .unload("transient-guest")
        .expect("unload succeeds");
    assert_eq!(
        report.transient_sources, 1,
        "the teardown report counts the menu it withdrew"
    );

    assert!(
        rig.transients.build(MENU, &in_org()).is_none(),
        "the name stops resolving, so the chord says `unknown source` rather \
         than reaching a dead actor"
    );
}

/// A name nobody registered still answers `None` with a plugin loaded — the
/// plugin's menu must not become a catch-all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrelated_name_is_still_unknown() {
    let Some(wasm) = transient_guest_wasm() else {
        eprintln!("skipping: transient-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "transient-guest", "transient-source", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    assert!(rig.transients.build("no-such-menu", &in_org()).is_none());
}
