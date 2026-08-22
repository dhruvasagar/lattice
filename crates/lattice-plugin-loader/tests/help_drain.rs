//! CR.3 end-to-end: a help plugin discovered on disk loads and its pages
//! become reachable through the SAME `HelpTopicRegistry` the builtin docs
//! live in — openable by name, enumerable by `:help <Tab>` completion,
//! cross-linkable from `:describe-command`, and reversed on unload.
//!
//! Uses the canonical `help-guest` fixture the plugin-host crate builds to a
//! `wasm32-wasip2` component. Skips when that component was not built.
//!
//! The point of the seam is that a plugin page is INDISTINGUISHABLE from a
//! builtin, so the assertions are about the shared surfaces (`lookup`,
//! `names`, `topics_for_command`) rather than a plugin-specific side table —
//! if any of them needed a special case, the seam would have failed at its
//! purpose.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_completion::traits::CandidateGenerator;
use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_help::topics::{HelpTopicRegistryHandle, HelpTopicsGenerator};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn help_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/help-guest/target/wasm32-wasip2/release/help_guest.wasm"
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
    help: HelpTopicRegistryHandle,
}

/// Seeded with the REAL builtin topic set, not an empty registry: the
/// namespacing assertion below is only meaningful if `buffers` actually
/// exists to be shadowed.
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
async fn a_help_plugins_pages_are_indistinguishable_from_builtins() {
    let Some(wasm) = help_guest_wasm() else {
        eprintln!("skipping: help-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "help-guest", "help", &wasm);

    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the help plugin loads");

    let topics = rig.help.load();

    // Opens by name through the ordinary lookup `:help` uses, and the body is
    // the markdown that was compiled into the component.
    let usage = topics
        .lookup("help-guest.usage")
        .expect("the namespaced page resolves through the normal lookup");
    assert!(usage.body.render().contains("include_str"));

    // The one-page spelling works too.
    assert!(topics.lookup("help-guest").is_some());

    // Namespaced: the plugin tried to register `buffers` and could not take it.
    assert!(
        topics.lookup("help-guest.buffers").is_some(),
        "the colliding name was namespaced"
    );
    let builtin = topics.lookup("buffers").expect("the builtin still exists");
    assert!(
        !builtin.body.render().contains("Impostor"),
        "`:help buffers` must still be the real page"
    );

    // `:describe-command` cross-links reach it, via the same
    // `related_command_patterns` walk a builtin doc's frontmatter drives.
    let linked: Vec<&str> = topics
        .topics_for_command("help-guest-something")
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        linked.contains(&"help-guest.usage"),
        "cross-link walk finds the plugin page: {linked:?}"
    );
}

/// The gap CR.1 called "the same gap one level down": a page that opens by
/// exact name but never appears in completion is discoverable only by someone
/// who already knows it exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_page_appears_in_help_tab_completion() {
    let Some(wasm) = help_guest_wasm() else {
        eprintln!("skipping: help-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "help-guest", "help", &wasm);

    let rig = rig(base.path());
    // The generator is built BEFORE the load, exactly as boot builds it — so
    // this fails against a generator holding a boot-time snapshot.
    let generator = HelpTopicsGenerator {
        topics: rig.help.clone(),
    };
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    // `gen:help-topics` produces everything and lets the matcher filter, so
    // the surrounding buffer / registry / prefix are irrelevant here.
    let buffer = lattice_core::Buffer::empty();
    let registry = CommandRegistry::new();
    let ctx = lattice_completion::traits::GenerateContext {
        prefix: "",
        buffer: &buffer,
        registry: &registry,
        case_sensitive: false,
    };
    let names: Vec<String> = generator
        .generate(&ctx)
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(
        names.iter().any(|n| n == "help-guest.usage"),
        "`:help <Tab>` enumerates the plugin's pages"
    );
    assert!(
        names.iter().any(|n| n == "index"),
        "and still enumerates the builtins"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_a_help_plugin_withdraws_its_pages() {
    let Some(wasm) = help_guest_wasm() else {
        eprintln!("skipping: help-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "help-guest", "help", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let before = rig.help.load().len();
    assert!(rig.help.load().lookup("help-guest.usage").is_some());

    let report = rig.loader.unload("help-guest").expect("unload succeeds");
    assert_eq!(
        report.help_topics, 3,
        "the teardown report counts the pages it withdrew"
    );

    let topics = rig.help.load();
    assert!(
        topics.lookup("help-guest.usage").is_none(),
        "the plugin's pages are gone"
    );
    assert!(topics.lookup("help-guest").is_none());
    assert!(topics.lookup("help-guest.buffers").is_none());
    assert_eq!(topics.len(), before - 3, "exactly its three pages went");
    // Removal is by provenance, so a builtin cannot be caught in it.
    assert!(
        topics.lookup("buffers").is_some(),
        "builtin pages survive an unload"
    );
}
