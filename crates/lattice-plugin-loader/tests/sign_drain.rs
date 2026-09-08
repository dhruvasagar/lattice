//! SG.3a end-to-end: a sign plugin discovered on disk loads at boot and its
//! signs become reachable through the SAME `SignRegistry` native producers use
//! — resolvable by name, carrying both palettes, contending for the mark cell
//! by the same priority rule, and reversed on unload.
//!
//! Uses the canonical `sign-guest` fixture the plugin-host crate builds to a
//! `wasm32-wasip2` component. Skips when that component was not built.
//!
//! The point of the seam is that a plugin's sign is INDISTINGUISHABLE from a
//! native producer's, so the assertions here are about the shared surfaces
//! (`id_of`, `glyph`, `sign_beats_severity`) rather than a plugin-specific side
//! table — if any of them needed a special case, the seam would have failed at
//! its purpose.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ContextSourceRegistry, GutterDecorationSourceRegistry, ModeRegistry, ModeRegistryHandle,
    PluginMetaSink, SignRegistry, SignRegistryHandle,
};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;

fn sign_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/sign-guest/target/wasm32-wasip2/release/sign_guest.wasm"
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
    signs: SignRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    let signs: SignRegistryHandle = Arc::new(arc_swap::ArcSwap::from_pointee(SignRegistry::new()));
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
            parser_factories: Some(lattice_compilation::CompilationParserFactories::new_handle()),
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
            sign_registry: Some(signs.clone()),
            // Wired even though nothing here declares a theme element: the
            // loader gates the WHOLE reversal on one all-or-nothing tuple of
            // registry handles, so a rig missing any of them turns unload into
            // a silent no-op and the teardown assertions below would pass
            // vacuously against a registry nothing had touched.
            theme_registry: Some(Arc::new(lattice_theme::InMemoryThemeRegistry::new(
                lattice_theme::default_palette(),
            ))),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );
    Rig { loader, signs }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_plugins_signs_are_indistinguishable_from_a_native_producers() {
    let Some(wasm) = sign_guest_wasm() else {
        eprintln!("skipping: sign-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "sign-guest", "signs", &wasm);

    let rig = rig(base.path());
    assert_eq!(discover(&plugins_dir).len(), 1);
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the sign plugin loads");

    let registry = rig.signs.load();
    // Namespaced by the plugin's manifest id — a plugin cannot squat a bare
    // name or shadow a native producer's sign.
    let id = registry
        .id_of("sign-guest.breakpoint")
        .expect("the sign resolves by name through the normal lookup");
    assert!(
        registry.id_of("breakpoint").is_none(),
        "the un-namespaced name is not squatted"
    );

    // BOTH palettes survived. Dropping the fallback would render tofu for
    // every user without a patched font, and the Nerd Font path alone would
    // never show it.
    let def = registry.get(id).unwrap();
    assert_eq!(
        def.glyph(true).chars().count(),
        def.glyph(false).chars().count(),
        "equal cell width, so toggling `ui.nerd_fonts` cannot shift the gutter"
    );
    assert_ne!(def.glyph(true), def.glyph(false));

    // The contention rule is the shared one, not a plugin-specific path.
    let current = registry
        .get(registry.id_of("sign-guest.current-line").unwrap())
        .unwrap();
    let note = registry
        .get(registry.id_of("sign-guest.note").unwrap())
        .unwrap();
    assert!(
        lattice_mode::sign_beats_severity(current),
        "priority 30 outranks a diagnostic"
    );
    assert!(
        !lattice_mode::sign_beats_severity(note),
        "priority 10 TIES with a diagnostic, and a tie leaves the error visible"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redefinition_across_the_boundary_keeps_the_id() {
    // The guest declares `breakpoint` twice, the second time with a different
    // glyph. If the id moved, every placement a producer had already emitted
    // would resolve to nothing and the marks would vanish on reload.
    let Some(wasm) = sign_guest_wasm() else {
        eprintln!("skipping: sign-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "sign-guest", "signs", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    let registry = rig.signs.load();
    assert_eq!(
        registry.len(),
        3,
        "three NAMES, not four — the redefinition reused its entry"
    );
    let def = registry
        .get(registry.id_of("sign-guest.breakpoint").unwrap())
        .unwrap();
    assert_eq!(
        def.fallback, "◉",
        "and the id now resolves to the LATER definition's glyph"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_a_sign_plugin_withdraws_its_signs() {
    let Some(wasm) = sign_guest_wasm() else {
        eprintln!("skipping: sign-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "sign-guest", "signs", &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let before = rig.signs.load().id_of("sign-guest.breakpoint");
    assert!(before.is_some(), "precondition: declared");

    let report = rig
        .loader
        .unload("sign-guest")
        .expect("the plugin was loaded, so unload reports");
    assert_eq!(report.signs, 3, "all three declared signs are withdrawn");
    assert!(
        rig.signs.load().id_of("sign-guest.breakpoint").is_none(),
        "the name stops resolving"
    );
    // The id RETIRES rather than being reused: a placement produced before the
    // unload must paint nothing, not some later sign's glyph.
    assert!(
        rig.signs.load().get(before.unwrap()).is_none(),
        "the retired id resolves to nothing"
    );
    let mut next: SignRegistry = (**rig.signs.load()).clone();
    let fresh = next.define(lattice_mode::SignDefinition {
        name: "other.mark".into(),
        text: "◆".into(),
        fallback: "◆".into(),
        theme_element: "gutter.sign".into(),
        priority: 5,
        column: lattice_mode::SIGN_COLUMN_MARK.into(),
    });
    assert_ne!(
        fresh,
        before.unwrap(),
        "a later definition must not inherit the retired slot"
    );
}
