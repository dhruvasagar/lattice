//! TC.5 end-to-end: the real `treesitter-context` bundled plugin loads as a
//! MULTI-SEAM component (theme + config + context from one `.wasm`) and each
//! seam lands in the registry it belongs to.
//!
//! This is the slice where the feature stops being scaffolding: a genuine
//! tree-sitter query runs inside WASM against a real parse tree, and the scopes
//! it returns are the ones the host resolver will pin. So the assertions are
//! about the actual structure of actual Rust source, not canned data — a
//! fixture returning constants would prove the plumbing and none of the query.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::dispatcher::execute_with_env;
use lattice_grammar::registry::GrammarEnv;
use lattice_grammar::{CancellationToken, Effect};
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
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;
use lattice_syntax::{Lang, Syntax};
use lattice_theme::{ElementName, ThemeRegistryHandle};

fn plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/treesitter-context/target/wasm32-wasip2/release/treesitter_context.wasm"
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
    let dir = root.join("treesitter-context");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/treesitter-context/plugin.toml"
    ))
    .unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

struct Rig {
    loader: PluginLoader,
    contexts: ContextSourceRegistryHandle,
    theme: ThemeRegistryHandle,
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
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let commands_for_rig = commands.clone();
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let modes_for_rig = modes.clone();
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
            config_registry: Some(config.clone()),
            keymap: Some(KeymapHandle::new()),
            decoration_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                GutterDecorationSourceRegistry::new(),
            ))),
            context_registry: Some(contexts.clone()),
            theme_registry: Some(theme.clone()),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
        },
    );
    Rig {
        loader,
        contexts,
        theme,
        config,
        commands: commands_for_rig,
        modes: modes_for_rig,
    }
}

/// Real Rust with genuine nesting: an `impl` containing a `fn` containing an
/// `if`. Line numbers are 0-based.
///
///   0 impl Renderer for Tui {
///   1     fn paint(&mut self) {
///   2         if self.dirty {
///   3             self.blit();
///   4         }
///   5     }
///   6 }
const SRC: &str = "\
impl Renderer for Tui {
    fn paint(&mut self) {
        if self.dirty {
            self.blit();
        }
    }
}
";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bundled_plugin_registers_all_three_seams() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: treesitter-context wasm not built (no wasm32-wasip2 target)");
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

    // context: the producer is registered and callable.
    assert_eq!(
        rig.contexts.load().sources().len(),
        1,
        "the context producer registered"
    );
    // theme: four elements, namespaced.
    for name in ["background", "separator", "line-number", "active"] {
        let full = format!("treesitter-context.{name}");
        assert!(
            rig.theme.id(&ElementName::from(full.clone())).is_some(),
            "{full} is registered so a theme can restyle it"
        );
    }
    // config: the options land in the same registry core options use.
    for name in ["max-lines", "anchor", "trim-scope", "max-file-lines"] {
        let full = format!("treesitter-context.{name}");
        assert!(
            rig.config.lookup(&full).is_some(),
            "{full} is registered so `:set` and `:describe-option` reach it"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_query_finds_the_real_nesting_in_real_source() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: treesitter-context wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let sources = rig.contexts.load().sources();

    let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syn.parse(SRC);
    let snapshot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(syn.snapshot_owned());

    let scopes = sources[0]
        .produce(
            7,
            Some(std::path::PathBuf::from("src/render.rs")),
            SRC.lines().count() as u32,
            Some(snapshot),
        )
        .await
        .expect("the query runs and returns scopes");

    // The three nested constructs, each spanning more than one line.
    let spans: Vec<(u32, u32)> = scopes
        .iter()
        .map(|s| (s.scope_start, s.scope_end))
        .collect();
    assert!(
        spans.contains(&(0, 6)),
        "the impl block spans the whole file: {spans:?}"
    );
    assert!(
        spans.contains(&(1, 5)),
        "the fn spans lines 1..=5: {spans:?}"
    );
    assert!(
        spans.contains(&(2, 4)),
        "the if spans lines 2..=4 — branch arms are captured deliberately, \
         because knowing which branch you are in is what a long function hides \
         and what folds cannot tell you: {spans:?}"
    );

    // Single-line scopes are dropped by the plugin: their header can never
    // scroll away while the cursor is inside them, so caching them would only
    // lengthen the host's scan.
    assert!(
        scopes.iter().all(|s| s.scope_end > s.scope_start),
        "no single-line scopes survive: {spans:?}"
    );

    // Headers are single-line here (no wrapped signatures), and every one
    // starts at its scope.
    for s in &scopes {
        assert_eq!(s.header_start, s.scope_start);
        assert!(s.header_end >= s.header_start);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_language_with_no_query_yields_no_scopes_rather_than_an_error() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: treesitter-context wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let sources = rig.contexts.load().sources();

    // YAML ships no `@context` query. That must read as "nothing to show",
    // NOT as a failure — an `Err` would make the host keep the previous
    // buffer's scopes rather than clearing them.
    let mut syn = Syntax::for_language(Lang::Yaml).unwrap().unwrap();
    syn.parse("a: 1\nb:\n  c: 2\n");
    let snapshot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(syn.snapshot_owned());

    let scopes = sources[0]
        .produce(7, None, 3, Some(snapshot))
        .await
        .expect("a language with no query is not an error");
    assert!(scopes.is_empty());
}

/// A file deep enough that the jump has somewhere to go. Line numbers 0-based:
///
///   0 impl Renderer for Tui {
///   1     fn paint(&mut self) {
///   2         if self.dirty {
///   3             self.blit();
///   4         }
///   5     }
///   6 }
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mode_and_its_chord_are_registered() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: treesitter-context wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    // The action the chord and the ex-command both resolve to.
    assert!(
        rig.commands.load().lookup_by_name("context-up").is_some(),
        "the plugin's own action is registered, which is what `grammar` \
         preceding `modes` in `provides` guarantees is true by bind time"
    );
    // `:context-toggle` exists; `:context-up` deliberately does NOT. An
    // ex-command gets no tree handle from the seam, so it cannot compute a
    // jump target — and a command that silently does nothing is worse than an
    // absent one. The chord is the surface for the jump.
    assert!(
        rig.commands
            .load()
            .lookup_by_name("context-toggle")
            .is_some()
    );

    // A MINOR mode — `[u` must never reach the builtin layer, where it would
    // fire in buffers that have no tree at all.
    let modes = rig.modes.load();
    let id = lattice_mode::ModeId::new("treesitter-context-mode");
    let mode = modes.get(id).expect("the minor mode is registered");
    assert_eq!(mode.kind(), lattice_mode::ModeKind::Minor);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_up_walks_outward_and_terminates() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: treesitter-context wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let commands = rig.commands.load();
    let up = commands.id_by_name("context-up").unwrap();

    let snapshot: Arc<dyn std::any::Any + Send + Sync> = {
        let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        syn.parse(SRC);
        Arc::new(syn.snapshot_owned())
    };

    // Fire from inside the `if` body (line 3). Each press should land on the
    // next header out: 2 (the if), then 1 (the fn), then 0 (the impl), then
    // nothing.
    let expected = [Some(2u32), Some(1), Some(0), None];
    let mut at = 3u32;
    for (i, want) in expected.iter().enumerate() {
        let mut doc = lattice_core::Document::from_text(SRC);
        let env = GrammarEnv {
            syntax: Some(&snapshot),
            ..Default::default()
        };
        let effect = execute_with_env(
            &commands,
            &mut doc,
            lattice_core::buffers::BufferId(1),
            Position::new(at, 0),
            CommandInvocation::of(up),
            &CancellationToken::never(),
            env,
        )
        .expect("context-up dispatches");

        match (want, &effect) {
            (Some(line), Effect::Many(effects)) => {
                // `RecordJump` FIRST — the ring must capture where the cursor
                // was BEFORE the move, which is what makes `<C-o>` return.
                assert!(
                    matches!(effects[0], Effect::RecordJump),
                    "step {i}: the jump is recorded before the move, got {effects:?}"
                );
                assert!(
                    matches!(effects[1], Effect::CursorMove(p) if p.line == *line),
                    "step {i}: lands on header line {line}, got {effects:?}"
                );
                at = *line;
            }
            (None, e) => {
                // At top level there is no enclosing header above the cursor.
                // A no-op that CONSUMES the chord, not `Declined` — the user
                // asked for the action and it simply had nowhere to go, so
                // falling through to another binding would surprise them.
                assert!(
                    matches!(e, Effect::None),
                    "step {i}: top level is a quiet no-op, got {e:?}"
                );
            }
            (want, got) => panic!("step {i}: expected {want:?}, got {got:?}"),
        }
    }
}
