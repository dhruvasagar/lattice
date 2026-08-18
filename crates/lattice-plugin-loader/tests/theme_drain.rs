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

/// TC.4: a theme file OVERRIDES a plugin-registered element, exactly as it
/// overrides a builtin.
///
/// The slice claimed this as a test and the closest thing shipped was an
/// assertion that a palette key resolved to some colour — which is a
/// different property (that registration worked) from the one that matters
/// (that a user can restyle it). "Indistinguishable from a builtin" is the
/// design's promise, and an override is the sharpest way to hold it: if the
/// plugin's element sat outside the override layer, registration would still
/// look fine and restyling would silently do nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_theme_override_reaches_a_plugin_registered_element() {
    let Some(wasm) = theme_guest_wasm() else {
        eprintln!("skipping: theme-guest wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "theme-fixture", "theme", &wasm);
    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    let name = lattice_theme::ElementName::from("theme-fixture.background".to_string());
    let id = rig
        .theme
        .id(&name)
        .expect("the plugin's element is registered");
    let before = rig.theme.resolved().get(id).bg;

    rig.theme.set_override(
        name.clone(),
        lattice_theme::StyleSpec {
            bg: Some(lattice_theme::ColorRef::Literal(lattice_theme::Color::Rgb(
                0x12, 0x34, 0x56,
            ))),
            ..Default::default()
        },
    );

    let after = rig.theme.resolved().get(id).bg;
    assert_ne!(
        before, after,
        "the override changed the resolved style — a plugin element outside \
         the override layer would resolve identically and restyling would \
         silently do nothing"
    );
    assert_eq!(
        after.map(|c| c.to_rgb_u32(0)),
        Some(0x12_34_56),
        "and it resolved to exactly what the theme asked for"
    );
}
