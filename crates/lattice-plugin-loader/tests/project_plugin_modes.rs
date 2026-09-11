//! PC.6 — `project-mode`'s chords and the `project.switch-commands` option,
//! through the real bundled component and the real loader.
//!
//! Design: `docs/dev/architecture/project-commands.md` §7–§8. Slice plan:
//! `docs/dev/operations/slice-plans/project-commands.md` PC.6.
//!
//! ## What this asserts that a guest unit test cannot
//!
//! Which LAYER the chords land in. `<leader>p`/`<C-x>p` must be in the mode's
//! own `MinorMode` layer and never in `Builtin` — Builtin is universal vim
//! grammar that fires in every buffer, and a project prefix there would shadow
//! the grammar everywhere. The guest declares the bindings; only the host can
//! say where they ended up.
//!
//! It also pins the `<C-x>p` decision recorded in design §8: the chord is bound
//! UNCONDITIONALLY, because a plugin registers keymaps at load and there is no
//! unregister. If a dynamic keymap seam ever lands, this test is what will fail
//! and point at the gate that should come back.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ContextSourceRegistry, ContextSourceRegistryHandle, GutterDecorationSourceRegistry,
    ModeRegistry, ModeRegistryHandle, PluginMetaSink,
};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;
use lattice_theme::ThemeRegistryHandle;

fn plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/project/target/wasm32-wasip2/release/project.wasm"
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
/// ordering and the capability declaration are the ones under test.
fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("project");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/project/plugin.toml"
    ))
    .unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// Only what the assertions read. The context and theme registries are still
/// WIRED into `LoaderServices` below — an unwired seam fails the whole load —
/// but nothing here asserts on them, so holding them would be an orphan.
struct Rig {
    loader: PluginLoader,
    keymap: KeymapHandle,
    config: Arc<ConfigRegistry>,
    commands: CommandRegistryHandle,
    modes: ModeRegistryHandle,
}

fn rig(base: &std::path::Path) -> Rig {
    let contexts: ContextSourceRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ContextSourceRegistry::new()));
    let theme: ThemeRegistryHandle = Arc::new(lattice_theme::InMemoryThemeRegistry::new(
        lattice_theme::default_palette(),
    ));
    let config = Arc::new(ConfigRegistry::default());
    // PK.1: `magit-status` stands in the registry before the plugin loads,
    // because `<C-x>pv` binds to MAGIT's command and a `mode-keymap-binding`
    // resolves its command name against the `CommandRegistry` AT REGISTRATION
    // — an unresolvable name is dropped, silently and by design.
    //
    // **Production correctness additionally depends on boot ORDER**, which
    // this rig cannot assert and which is worth naming here:
    // `lattice_magit::install` runs at `editor_boot.rs:720` and
    // `lattice_plugin_loader::install` at `:2058`, so the command exists by
    // the time the project plugin's modes drain. Reorder those two and this
    // chord goes dead with no error anywhere — the menu row for the same verb
    // has a greyed-with-reason fallback and a chord has none.
    let commands: CommandRegistryHandle = Arc::new(arc_swap::ArcSwap::from_pointee({
        let mut reg = CommandRegistry::new();
        reg.register_ex_command(
            "magit-status",
            "test stand-in for magit's own command",
            lattice_grammar::registry::ExCommandSpec {
                latency_class: lattice_grammar::command::LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|rest: &str, _bang: bool| {
                    Ok(lattice_grammar::Args::String(rest.to_string()))
                }),
                apply: Arc::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
                surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
            },
        );
        reg
    }));
    let commands_for_rig = commands.clone();
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let modes_for_rig = modes.clone();
    // Retained, unlike before: TC.6's headline claim is about WHICH LAYER the
    // chord lands in, and a keymap constructed inline and dropped makes that
    // unassertable — which is exactly why the claim went untested.
    let keymap = KeymapHandle::new();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            // The core plugins now `provides = [… "help"]` (CR.3), and an
            // unwired seam fails the WHOLE load — so a harness that loads a
            // real core plugin has to wire this or it silently gets zero
            // plugins.
            help_topics: Some(lattice_help::topics::builtin_topics().into_handle()),
            parser_factories: Some(lattice_compilation::CompilationParserFactories::new_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(config.clone()),
            keymap: Some(keymap.clone()),
            decoration_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                GutterDecorationSourceRegistry::new(),
            ))),
            context_registry: Some(contexts.clone()),
            theme_registry: Some(theme.clone()),
            // TR.2b: the plugin `provides = [… "transient-source"]`, and an
            // UNWIRED seam fails the whole load rather than that one seam — so
            // a harness missing this silently discovers zero plugins.
            transient_registry: Some(Arc::new(lattice_picker::TransientSourceRegistry::new())),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );
    Rig {
        loader,
        keymap,
        config,
        commands: commands_for_rig,
        modes: modes_for_rig,
    }
}

/// Both prefixes reach `project-switch`, and neither lands in Builtin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_prefixes_are_bound_in_the_modes_own_layer() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: project plugin wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "one component, loaded once");

    let cmd = rig
        .commands
        .load()
        .lookup_by_name("project-switch")
        .expect("`:project-switch` is registered")
        .id;
    let bindings = rig.keymap.reverse_entries(cmd);
    assert!(
        !bindings.is_empty(),
        "the verb is bound to something — a project picker reachable only by \
         name is the friction this plugin exists to remove"
    );

    let layers: Vec<String> = bindings
        .iter()
        .map(|(chord, layer)| format!("{chord:?} in {layer:?}"))
        .collect();
    assert!(
        bindings
            .iter()
            .all(|(_, layer)| !matches!(layer, lattice_keymap::KeymapLayer::Builtin)),
        "nothing may land in Builtin — that layer is universal vim grammar and \
         fires in every buffer: {layers:?}"
    );
    assert!(
        bindings
            .iter()
            .any(|(_, layer)| matches!(layer, lattice_keymap::KeymapLayer::MinorMode(_))),
        "the chords ride the mode's own layer, so K.1.c's per-keystroke filter \
         can scope them: {layers:?}"
    );

    // `layer_bindings`, not `reverse_entries`, and the difference matters:
    // the reverse cache holds ONE path per command, so a command bound under
    // two prefixes looks singly-bound through it. This asks the layer what it
    // actually holds, which is the question being asked.
    let layer_bound = rig.keymap.layer_bindings(
        lattice_keymap::KeymapLayer::MinorMode(lattice_mode::ModeId::new("project-mode")),
        lattice_keymap::BindingMode::Normal,
    );
    let paths: Vec<String> = layer_bound
        .iter()
        .map(|(path, _)| {
            path.iter()
                .map(|c| match c {
                    lattice_keymap::ChordPattern::Literal(k) => format!("{k:?}"),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();

    // The SUFFIX SET, not a count.
    //
    // This asserted `== 3` until PK.1, and a count is the wrong assertion for a
    // list that grows: PB.1 added `b` and turned it red with a message that
    // said only `4 != 3`, naming neither the letter that appeared nor the
    // letter that should have. A set says which verb arrived, and adding one
    // means writing it down here — which is the review this list wants.
    let suffix_after = |prefix: &str| -> Vec<String> {
        let mut out: Vec<String> = paths
            .iter()
            .filter(|p| p.starts_with(prefix))
            .filter_map(|p| p.rsplit_once("Char('").map(|(_, tail)| tail.to_string()))
            .filter_map(|tail| tail.split_once('\'').map(|(c, _)| c.to_string()))
            .collect();
        out.sort();
        out
    };

    let expected = vec!["b", "d", "f", "g", "p", "s", "v"];
    assert_eq!(
        suffix_after("KeyChord { key: Char(' ')"),
        expected,
        "<leader>p{{…}} — the always-live home. PK.1: every switch-menu row has \
         a chord, so this set and `switch.rs`'s defaults move together: {paths:?}"
    );

    // Design §8: `<C-x>p` is bound UNCONDITIONALLY. The design first wanted it
    // gated on the `emacs-keys` option so `:set noemacs-keys` fully reclaimed
    // `<C-x>`, and that is not buildable — a plugin registers keymaps at load
    // and there is no unregister and no runtime push/pop. If a dynamic keymap
    // seam ever lands, this assertion is what should fail and point at the gate
    // that ought to come back.
    assert_eq!(
        suffix_after("KeyChord { key: Char('x'), mods: KeyMods(1)"),
        expected,
        "<C-x>p{{…}} — project.el's own prefix, bound unconditionally and to the \
         same verbs as the leader prefix: {paths:?}"
    );
}

/// The mode itself is declared as a UNIVERSAL minor — the verbs are global, and
/// a project picker that only worked inside a project would be useless for the
/// case it exists for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mode_is_a_universal_minor() {
    let Some(wasm) = plugin_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);
    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    let modes = rig.modes.load();
    let mode = modes
        .get(lattice_mode::ModeId::new("project-mode"))
        .expect("project-mode is registered");
    assert_eq!(mode.kind(), lattice_mode::ModeKind::Minor);
}

/// The option is a real structured one, registered in the same registry core
/// options live in — not a string carrying TOML.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_switch_commands_option_registers() {
    let Some(wasm) = plugin_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);
    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    assert!(
        rig.config.lookup("project.switch-commands").is_some(),
        "`project.switch-commands` lands in the same registry core options use"
    );
}
