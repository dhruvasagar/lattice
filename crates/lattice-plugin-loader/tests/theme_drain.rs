//! TC.4 end-to-end: a theme plugin discovered on disk loads at boot and its
//! elements become reachable through the SAME `ThemeRegistry` builtins use —
//! resolvable by name, listed for `:customize`, overridable by a theme, and
//! reversed on unload.
//!
//! Uses the canonical `theme-guest` fixture the plugin-host crate builds to a
//! `wasm32-wasip2` component. Skips when that component was not built.
//!
//! The point of the seam is that a plugin element is INDISTINGUISHABLE from a
//! builtin, so the assertions here are about the shared surfaces (`id`,
//! `element_names`, `describe`, override) rather than about a plugin-specific
//! side table — if any of them needed a special case, the seam would have
//! failed at its purpose.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ContextSourceRegistry, GutterDecorationSourceRegistry, ModeRegistry, ModeRegistryHandle,
    PluginMetaSink,
};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;
use lattice_theme::{ElementName, ThemeRegistryHandle};

fn theme_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/theme-guest/target/wasm32-wasip2/release/theme_guest.wasm"
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
    theme: ThemeRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    let theme: ThemeRegistryHandle = Arc::new(lattice_theme::InMemoryThemeRegistry::new(
        lattice_theme::default_palette(),
    ));
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
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
            decoration_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                GutterDecorationSourceRegistry::new(),
            ))),
            context_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                ContextSourceRegistry::new(),
            ))),
            theme_registry: Some(theme.clone()),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
        },
    );
    Rig { loader, theme }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_theme_plugins_elements_are_indistinguishable_from_builtins() {
    let Some(wasm) = theme_guest_wasm() else {
        eprintln!("skipping: theme-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "treesitter-context", "theme", &wasm);

    let rig = rig(base.path());
    assert_eq!(discover(&plugins_dir).len(), 1);
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the theme plugin loads");

    // Namespaced by the plugin's manifest id — a plugin cannot squat a bare
    // name or shadow a builtin.
    let bg = ElementName::from("treesitter-context.background");
    assert!(
        rig.theme.id(&bg).is_some(),
        "the element resolves by name through the normal lookup"
    );
    assert!(
        rig.theme.id(&ElementName::from("background")).is_none(),
        "the un-namespaced name is not squatted"
    );

    // It appears on the surfaces `:customize` / `:describe-element` read.
    let names = rig.theme.element_names();
    assert!(names.iter().any(|n| n == "treesitter-context.background"));
    let info = rig
        .theme
        .describe(&bg)
        .expect("describe works on a plugin element exactly as on a builtin");
    assert!(
        info.doc.contains("backdrop"),
        "the plugin's own doc string survived the crossing: {:?}",
        info.doc
    );

    // The palette reference resolved against the ACTIVE palette rather than
    // being baked — this is what makes a plugin element re-colour on
    // `:colorscheme`, and it is the whole reason the seam passes a key.
    let resolved = rig.theme.resolved();
    let style = resolved.get(rig.theme.id(&bg).unwrap());
    assert!(
        style.fg.is_some(),
        "the `overlay` palette key resolved to a concrete colour"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_a_theme_plugin_withdraws_its_elements() {
    let Some(wasm) = theme_guest_wasm() else {
        eprintln!("skipping: theme-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "treesitter-context", "theme", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let bg = ElementName::from("treesitter-context.background");
    assert!(rig.theme.id(&bg).is_some(), "precondition: registered");

    let report = rig
        .loader
        .unload("treesitter-context")
        .expect("the plugin was loaded, so unload reports");
    assert_eq!(
        report.theme_elements, 3,
        "all three declared elements are withdrawn"
    );
    assert!(
        rig.theme.id(&bg).is_none(),
        "the name stops resolving, so the element leaves `:customize` and \
         `:describe-element` with it"
    );
    assert!(
        !rig.theme
            .element_names()
            .iter()
            .any(|n| n.starts_with("treesitter-context.")),
        "no plugin element survives in the listing"
    );
}
