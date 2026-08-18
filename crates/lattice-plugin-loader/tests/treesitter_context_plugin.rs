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
    keymap: KeymapHandle,
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
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
        },
    );
    Rig {
        loader,
        keymap,
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
async fn the_bundled_plugin_registers_its_seams() {
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
    // TC.11: theme — the plugin registers NOTHING, and that is the point.
    //
    // It used to register four elements that no renderer ever read: the strip
    // is host chrome, styled by the host's `sticky.context.*` set. An element
    // that resolves in `:describe-element` but never paints is worse than an
    // absent one, because the user cannot tell it apart from a theme bug.
    for name in ["background", "separator", "line-number", "active"] {
        let full = format!("treesitter-context.{name}");
        assert!(
            rig.theme.id(&ElementName::from(full.clone())).is_none(),
            "{full} must NOT be registered — nothing paints it"
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

/// TC.10: a wrapped signature must still pin as many lines as it occupies.
///
/// This is the behaviour the `@context.end` switch had to preserve. The body
/// position used to come from a guest-side `child_by_field("body")` call; it
/// now comes from a query capture paired by match index. If the pairing is
/// wrong — captures grouped across matches, or the `end` dropped — every
/// header silently collapses to one line, and a wrapped signature pins `fn
/// wrapped(` with its arguments cut off. That reads as a truncation bug, not
/// as a missing capture, which is why it is asserted on real source rather
/// than left to the query test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrapped_signature_yields_a_multi_line_header() {
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

    //   0  fn wrapped(
    //   1      a: u32,
    //   2      b: u32,
    //   3  ) -> u32 {
    //   4      a + b
    //   5  }
    const WRAPPED: &str = "fn wrapped(\n    a: u32,\n    b: u32,\n) -> u32 {\n    a + b\n}\n";
    let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syn.parse(WRAPPED);
    let snapshot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(syn.snapshot_owned());

    let scopes = sources[0]
        .produce(
            7,
            Some(std::path::PathBuf::from("src/wrapped.rs")),
            WRAPPED.lines().count() as u32,
            Some(snapshot),
        )
        .await
        .expect("the query runs");

    let f = scopes
        .iter()
        .find(|s| s.scope_start == 0)
        .expect("the fn is captured");
    assert_eq!(f.header_start, 0);
    assert_eq!(
        f.header_end, 3,
        "the header runs to the line the body opens on, so all four signature \
         lines pin: {scopes:?}"
    );
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

    // The mode must require NO capabilities. Declaring `TREE_SITTER` here made
    // it fail to activate on every buffer including Rust files: the gate's
    // enforcement half exists but nothing populates a buffer's capability set
    // — every activation site passes `CapabilitySet::empty()` — so any
    // requirement at all is unsatisfiable today.
    //
    // It would also be the wrong requirement if the buffer side were built: a
    // buffer gains its tree when the first parse lands, so gating on it would
    // make `[u` unavailable until then. The manifest's `editor_capabilities`
    // is what actually gates the tree handle.
    assert_eq!(
        mode.required_capabilities(),
        lattice_mode::CapabilitySet::empty(),
        "a mode requiring capabilities can never activate: no buffer is ever \
         granted any"
    );
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

/// The staged artefact under `runtime/plugins` — what a real editor boots from
/// — must parse and declare every seam. The in-process tests above write their
/// own plugin dir, so they would pass even if the STAGED copy were stale or
/// its manifest wrong, which is the gap between "the tests are green" and "it
/// works when I run the editor".
#[test]
fn the_staged_runtime_plugin_is_discoverable_and_complete() {
    let staged = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/plugins"
    ));
    if !staged.exists() {
        eprintln!("skipping: runtime/plugins not staged (`cargo xtask build-core-plugins`)");
        return;
    }
    let found = lattice_plugin_loader::discover(staged);
    let ctx = found
        .iter()
        .find(|p| p.manifest.id == "treesitter-context")
        .expect("the staged core-plugin set includes treesitter-context");

    use lattice_plugin_host::PluginSeam;
    for seam in [
        PluginSeam::Config,
        PluginSeam::Grammar,
        PluginSeam::Modes,
        PluginSeam::Context,
    ] {
        assert!(
            ctx.manifest.provides.contains(&seam),
            "the staged manifest declares {seam}; a missing seam silently \
             drops that half of the plugin at boot"
        );
    }
}

/// A REAL large file, not a toy. `dispatch.rs` is ~36k lines, which is the
/// file the strip was reported not to render on.
///
/// It used to render nothing here: the producer minted a resource handle per
/// capture plus two more host calls each, went superlinear, and past ~20k
/// lines TRAPPED — which quarantines the plugin so it never produces again for
/// ANY buffer. `max-file-lines` existed to keep users the far side of that
/// cliff, and this file sat past it.
///
/// TC.10's ranges API removed the cliff, so the assertion inverts: a file this
/// size must now produce REAL context, under the guard rather than skipped by
/// it. The timing bound is deliberately loose (a debug-build CI box is slow);
/// it is there to catch a return to superlinear cost, not to measure it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_very_large_real_file_still_produces_scopes() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: treesitter-context wasm not built");
        return;
    };
    let src = match std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-host/src/dispatch.rs"
    )) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("skipping: dispatch.rs not readable");
            return;
        }
    };
    let lines = src.lines().count() as u32;

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);
    let rig = rig(base.path());
    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    let sources = rig.contexts.load().sources();

    let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syn.parse(&src);
    let snapshot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(syn.snapshot_owned());

    let started = std::time::Instant::now();
    let result = sources[0].produce(7, None, lines, Some(snapshot)).await;
    let elapsed = started.elapsed();

    // The point is that it does NOT trap. A trap quarantines the plugin, so
    // one oversized file kills the strip for every buffer until reload — far
    // worse than this file simply having no context.
    let scopes = result.unwrap_or_else(|e| {
        panic!(
            "a {lines}-line file must not trap the producer (that quarantines \
             the plugin editor-wide); failed after {elapsed:?}: {e}"
        )
    });
    assert!(
        !scopes.is_empty(),
        "a {lines}-line file is under the 100k guard and must produce real \
         context — an empty set here means the guard is back to skipping the \
         files that need the strip most"
    );
    assert!(
        scopes.iter().all(|s| s.scope_end > s.scope_start),
        "every surviving scope spans more than one line"
    );
    assert!(
        scopes.iter().any(|s| s.header_end > s.header_start),
        "a file this size has wrapped signatures, so at least one header must \
         span more than one line — all-single-line headers would mean the \
         `@context.end` pairing silently degraded"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "{lines} lines took {elapsed:?} — measured at ~52 ms release / well \
         under a second debug, so this bound only trips on a return to \
         superlinear cost"
    );
}

/// TC.12: `context.disabled-languages` must actually disable a language.
///
/// It was registered from TC.5 and read by nobody — the only occurrence of the
/// name in the whole tree was its own registration. An option that appears in
/// `:customize`, answers `:set …?`, and changes nothing is worse than an
/// absent one, because the editor reports a setting it is ignoring.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_language_produces_no_scopes() {
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
    let snapshot = || -> Arc<dyn std::any::Any + Send + Sync> {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(SRC);
        Arc::new(s.snapshot_owned())
    };
    let lines = SRC.lines().count() as u32;

    // Baseline: rust produces scopes.
    let before = sources[0]
        .produce(7, None, lines, Some(snapshot()))
        .await
        .expect("the query runs");
    assert!(!before.is_empty(), "precondition: rust has context");

    // Switch it off the way `:set` does, through the same registry the guest
    // reads via `get-option`.
    rig.config
        .parse_and_set_command("treesitter-context.disabled-languages=rust")
        .expect("the option is registered");

    let after = sources[0]
        .produce(7, None, lines, Some(snapshot()))
        .await
        .expect("still not an error — a disabled language is a normal state");
    assert!(
        after.is_empty(),
        "a disabled language yields no scopes, so no strip: {after:?}"
    );
}

/// TC.12: `context.max-file-lines` must skip a file when the USER lowers it.
///
/// The guard was only ever exercised through the plugin's compiled default,
/// because the context seam's store had no config registry — every
/// `get-option` in the producer returned `None`. So the option was reachable
/// from `:set`, reported a value, and did nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lowering_max_file_lines_skips_the_query() {
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
    let snapshot = || -> Arc<dyn std::any::Any + Send + Sync> {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(SRC);
        Arc::new(s.snapshot_owned())
    };
    let lines = SRC.lines().count() as u32;

    assert!(
        !sources[0]
            .produce(7, None, lines, Some(snapshot()))
            .await
            .unwrap()
            .is_empty(),
        "precondition: under the default guard this file has context"
    );

    // One line is below this fixture's line count, so the guard must fire.
    rig.config
        .parse_and_set_command("treesitter-context.max-file-lines=1")
        .expect("the option is registered");

    let after = sources[0]
        .produce(7, None, lines, Some(snapshot()))
        .await
        .expect("skipping is not an error — the host caches an empty set");
    assert!(
        after.is_empty(),
        "past the guard the query is skipped: {after:?}"
    );
}

/// TC.6's headline regression, finally asserted: `[u` must live in the mode's
/// OWN layer and never in `Builtin`.
///
/// `Builtin` is universal vim grammar — a chord there fires in every buffer,
/// including ones with no tree-sitter grammar at all, where `[u` means
/// nothing. The slice named this "the regression that matters" and shipped no
/// test for it: the test that sounded like it (`the_mode_and_its_chord_are_
/// registered`) never touched a keymap, and the rig dropped its handle so it
/// could not have.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chord_lives_in_the_modes_layer_not_the_builtin_one() {
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

    let cmd = rig
        .commands
        .load()
        .lookup_by_name("context-up")
        .expect("the action is registered")
        .id;
    let bindings = rig.keymap.reverse_entries(cmd);
    assert!(
        !bindings.is_empty(),
        "`context-up` is bound to something — an action with no chord is a \
         command the user can only reach by name"
    );

    let layers: Vec<String> = bindings
        .iter()
        .map(|(chord, layer)| format!("{chord:?} in {layer:?}"))
        .collect();
    // `reverse_entries` flattens a SEQUENCE into its individual chords, so the
    // two-key `[u` comes back as `[` then `u`.
    let keys: String = bindings
        .iter()
        .filter_map(|(chord, _)| match chord.key {
            lattice_protocol::chord::KeyKind::Char(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(keys, "[u", "the chord is `[u`: {layers:?}");
    assert!(
        bindings
            .iter()
            .all(|(_, layer)| !matches!(layer, lattice_keymap::KeymapLayer::Builtin)),
        "nothing may land in Builtin — that layer fires in every buffer, \
         including ones with no grammar: {layers:?}"
    );
    assert!(
        bindings
            .iter()
            .any(|(_, layer)| matches!(layer, lattice_keymap::KeymapLayer::MinorMode(_))),
        "and it must be in the mode's own layer, so K.1.c's per-keystroke \
         filter can scope it to buffers where the mode is active: {layers:?}"
    );
}

/// TC.6: a COUNT jumps N levels in one press, and a count past the top clamps
/// to the outermost rather than falling off.
///
/// Claimed by the slice, never written: nothing in the suite ever set a count
/// on the invocation, so `ctx.count` could have been ignored entirely and
/// every test would still pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_count_jumps_several_levels_and_clamps_at_the_top() {
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

    // From line 3 (inside the `if`): count 2 skips the `if` header and lands
    // on the `fn`; count 9 clamps to the outermost `impl` rather than running
    // out of scopes.
    for (count, want) in [(2u32, 1u32), (9, 0)] {
        let mut doc = lattice_core::Document::from_text(SRC);
        let env = GrammarEnv {
            syntax: Some(&snapshot),
            ..Default::default()
        };
        let mut inv = CommandInvocation::of(up);
        inv.count = Some(lattice_grammar::command::Count(count));
        let effect = execute_with_env(
            &commands,
            &mut doc,
            lattice_core::buffers::BufferId(1),
            Position::new(3, 0),
            inv,
            &CancellationToken::never(),
            env,
        )
        .expect("context-up dispatches");
        match &effect {
            Effect::Many(effects) => assert!(
                matches!(effects[1], Effect::CursorMove(p) if p.line == want),
                "count {count} lands on {want}, got {effects:?}"
            ),
            other => panic!("count {count}: expected a jump, got {other:?}"),
        }
    }
}

/// TC.6: `[u` in a buffer with no tree-sitter grammar is a quiet no-op.
///
/// This is why the mode declares no capabilities: rather than gate the chord
/// on `TREE_SITTER` and withhold it until a parse lands, the handler simply
/// has nowhere to go and consumes the keystroke. Claimed by the slice and
/// never exercised — the guest's `None`-tree branch had no test at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chord_is_a_quiet_no_op_without_a_grammar() {
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

    let mut doc = lattice_core::Document::from_text(SRC);
    // No syntax handle at all — a plain-text buffer.
    let env = GrammarEnv::default();
    let effect = execute_with_env(
        &commands,
        &mut doc,
        lattice_core::buffers::BufferId(1),
        Position::new(3, 0),
        CommandInvocation::of(up),
        &CancellationToken::never(),
        env,
    )
    .expect("dispatch succeeds even with no tree");
    assert!(
        matches!(effect, Effect::None),
        "no tree, no jump, no error — and it CONSUMES the chord rather than \
         declining, so it cannot fall through to another binding: {effect:?}"
    );
}

/// TC.5: every shipped language actually captures something.
///
/// The slice claimed "a representative file per language produces the expected
/// scopes"; only Rust was ever exercised end to end. The query test compiles
/// each query against its grammar, which catches a bad node kind or field
/// name, but a query that compiles and captures NOTHING passes it — and that
/// is exactly what a wrong-but-valid node kind produces.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_shipped_language_captures_a_nested_scope() {
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

    // Each sample nests an inner construct inside an outer one, so a query
    // that only captures top-level items fails too.
    let cases: &[(Lang, &str)] = &[
        (
            Lang::Python,
            "class C:\n    def m(self):\n        if x:\n            pass\n",
        ),
        (
            Lang::Go,
            "func f() {\n\tif x {\n\t\ty := 1\n\t\t_ = y\n\t}\n}\n",
        ),
        (
            Lang::JavaScript,
            "class C {\n  m() {\n    if (x) {\n      y();\n    }\n  }\n}\n",
        ),
        (
            Lang::TypeScript,
            "interface I {\n  a: number;\n}\nclass C {\n  m(): void {\n    if (x) {\n      y();\n    }\n  }\n}\n",
        ),
        (
            Lang::C,
            "struct S {\n  int a;\n};\nint f(void) {\n  if (a) {\n    return 1;\n  }\n  return 0;\n}\n",
        ),
        (
            Lang::Markdown,
            "# One\n\ntext\n\n## Two\n\nmore text\n\n### Three\n\nbody\n",
        ),
    ];

    for (lang, text) in cases {
        let mut syn = match Syntax::for_language(*lang) {
            Ok(Some(s)) => s,
            _ => {
                eprintln!("skipping {lang:?}: no grammar in this build");
                continue;
            }
        };
        syn.parse(text);
        let snapshot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(syn.snapshot_owned());
        let scopes = sources[0]
            .produce(7, None, text.lines().count() as u32, Some(snapshot))
            .await
            .unwrap_or_else(|e| panic!("{lang:?}: producer errored: {e}"));

        assert!(
            !scopes.is_empty(),
            "{lang:?}: the bundled query captured nothing. It compiles (the \
             query test proves that), so this is a query that names valid \
             node kinds which never match — the failure mode compilation \
             cannot catch."
        );
        assert!(
            scopes.len() >= 2,
            "{lang:?}: the sample nests, so at least two scopes must be \
             captured; got {scopes:?}"
        );
        assert!(
            scopes.iter().all(|s| s.scope_end > s.scope_start),
            "{lang:?}: single-line scopes are dropped by the producer"
        );
    }
}
