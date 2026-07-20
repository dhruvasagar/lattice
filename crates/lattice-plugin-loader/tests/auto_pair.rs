//! AP.1 end-to-end: the bundled `auto-pair` plugin — a single multi-seam
//! component (grammar + modes + config) — is discovered on disk and loaded
//! through the real loader, and ALL THREE seams' contributions register:
//!   - the pairing **actions** land in the command registry under
//!     `SourceLayer::Plugin` provenance,
//!   - `auto-pairs-mode` registers into the mode registry and OWNS its
//!     insert-mode keymap (bindings resolve only when the mode is active),
//!   - the `auto-pair.style` / `auto-pair.close-key` **options** register into
//!     the shared config registry.
//!
//! This is the AP.1.0 spike made real through the production loader path: the
//! same `.wasm` is instantiated once per seam (grammar sync / modes+config
//! async) against the superset linkers. Skips when the plugin wasn't built (no
//! `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_core::Document;
use lattice_core::buffers::BufferId;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::dispatcher::execute;
use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::{CancellationToken, CommandRegistryHandle, Effect};
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_protocol::edit::EditKind;
use lattice_protocol::position::Position;
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

/// The auto-pair component, if the `wasm32-wasip2` build produced it. The loader
/// crate can't read the host crate's build-script env var, so resolve by the
/// known path and skip if absent (the mode/config-drain precedent).
fn auto_pair_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
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

fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    // `grammar` BEFORE `modes`: the mode keymap binds to the plugin's own grammar
    // actions by name, resolved at bind time — so the grammar drain must run
    // first (the real `plugin.toml` orders them the same way).
    std::fs::write(
        dir.join("plugin.toml"),
        // AP.3: `editor_capabilities = ["tree-sitter"]` matches the real manifest
        // so the manual style's `enclosing` scope query gets its tree handle.
        "id = \"auto-pair\"\nprovides = [\"grammar\", \"modes\", \"config\"]\neditor_capabilities = [\"tree-sitter\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

fn empty_command_registry() -> CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_auto_pair_registers_grammar_modes_and_config_through_the_loader() {
    let Some(wasm) = auto_pair_wasm() else {
        eprintln!("skipping: auto-pair wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let command_registry = empty_command_registry();
    let mode_registry = empty_mode_registry();
    let config_registry = Arc::new(ConfigRegistry::default());
    let keymap = KeymapHandle::new();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let host =
        Arc::new(PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap());
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry.clone()),
            mode_registry: Some(mode_registry.clone()),
            config_registry: Some(config_registry.clone()),
            keymap: Some(keymap.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the multi-seam auto-pair plugin loads");
    assert!(loader.is_loaded("auto-pair"), "loader tracks it loaded");

    // 1. GRAMMAR — the pairing actions registered (sync drain).
    let commands = command_registry.load();
    for action in [
        "auto-pair-open-round",
        "auto-pair-open-square",
        "auto-pair-open-curly",
        "auto-pair-close-round",
        "auto-pair-close-square",
        "auto-pair-close-curly",
        "auto-pair-quote-double",
        "auto-pair-quote-single",
        "auto-pair-quote-backtick",
    ] {
        assert!(
            commands.id_by_name(action).is_some(),
            "the grammar action `{action}` registered from the plugin"
        );
    }

    // 2. MODES — the minor mode registered (async drain).
    let modes = mode_registry.load();
    assert!(
        modes.is_registered(ModeId::new("auto-pairs-mode")),
        "auto-pairs-mode registered into the published mode registry"
    );

    // The mode OWNS its insert-mode keymap: `(` resolves only when the mode is
    // active, never globally (mode-ownership — a gated MinorMode layer).
    let open = lattice_protocol::parse_chord_sequence("(").expect("chord parses");
    let mode = ModeId::new("auto-pairs-mode");
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Insert, &open, &[mode.clone()]),
            LookupResult::Bound { .. }
        ),
        "`(` binds to the plugin's open action when auto-pairs-mode is active"
    );
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Insert, &open, &[]),
            LookupResult::Unbound
        ),
        "the gated binding does not fire when the mode is inactive"
    );
    // The full pair set is bound: a quote chord resolves in the mode's layer too.
    let quote = lattice_protocol::parse_chord_sequence("\"").expect("quote chord parses");
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Insert, &quote, &[mode.clone()]),
            LookupResult::Bound { .. }
        ),
        "`\"` binds to the plugin's quote action when auto-pairs-mode is active"
    );

    // 3. CONFIG — the options registered (async drain).
    for option in ["auto-pair.style", "auto-pair.close-key"] {
        assert!(
            config_registry.lookup(option).is_some(),
            "the option `{option}` registered from the plugin"
        );
    }

    // Provenance recorded for `:list-plugins`.
    {
        let recorded = sink.registered.lock().unwrap();
        assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
        assert_eq!(recorded[0].1, "auto-pair", "under its manifest id");
    }

    // ── AP.2 behavior — the `auto` style, dispatched through the loaded plugin ──
    // The trampoline holds the grammar guest alive (the loader keeps it loaded),
    // so dispatching the registered actions fires the real guest sync.
    let open = commands.id_by_name("auto-pair-open-round").unwrap();
    let close = commands.id_by_name("auto-pair-close-round").unwrap();

    // open `(` on an empty buffer → insert `()` with the caret BETWEEN.
    let mut doc = Document::from_text("");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 0),
        CommandInvocation::of(open),
        &CancellationToken::never(),
    )
    .expect("open dispatches");
    match effect {
        Effect::ApplyEdit { target, edit, cursor } => {
            assert_eq!(target, BufferId(1), "targets the active buffer");
            assert!(
                matches!(&edit.kind, EditKind::Replace { text } if text == "()"),
                "inserts the pair, got {:?}",
                edit.kind
            );
            assert_eq!(cursor, Some(Position::new(0, 1)), "caret parked between the pair");
        }
        other => panic!("open: expected ApplyEdit, got {other:?}"),
    }

    // close `)` with a `)` right after the caret → STEP OVER (pure caret move,
    // no text change). Buffer `()`, caret between at (0,1).
    let mut doc = Document::from_text("()");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 1),
        CommandInvocation::of(close),
        &CancellationToken::never(),
    )
    .expect("close (skip) dispatches");
    match effect {
        Effect::SelectionChange(set) => {
            assert_eq!(
                set.primary().head,
                Position::new(0, 2),
                "caret stepped past the existing )"
            );
        }
        other => panic!("close-skip: expected SelectionChange, got {other:?}"),
    }

    // close `)` with a non-`)` after the caret → INSERT `)`. Buffer `ab`, caret
    // at (0,1) (before `b`).
    let mut doc = Document::from_text("ab");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 1),
        CommandInvocation::of(close),
        &CancellationToken::never(),
    )
    .expect("close (insert) dispatches");
    match effect {
        Effect::ApplyEdit { edit, cursor, .. } => {
            assert!(
                matches!(&edit.kind, EditKind::Replace { text } if text == ")"),
                "inserts a close paren, got {:?}",
                edit.kind
            );
            assert_eq!(cursor, Some(Position::new(0, 2)), "caret after the inserted )");
        }
        other => panic!("close-insert: expected ApplyEdit, got {other:?}"),
    }

    // A BRACKET pair behaves like round: `[` on an empty buffer → `[]` caret-between.
    let open_sq = commands.id_by_name("auto-pair-open-square").unwrap();
    let mut doc = Document::from_text("");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 0),
        CommandInvocation::of(open_sq),
        &CancellationToken::never(),
    )
    .expect("open-square dispatches");
    match effect {
        Effect::ApplyEdit { edit, cursor, .. } => {
            assert!(
                matches!(&edit.kind, EditKind::Replace { text } if text == "[]"),
                "inserts the bracket pair, got {:?}",
                edit.kind
            );
            assert_eq!(cursor, Some(Position::new(0, 1)), "caret between the brackets");
        }
        other => panic!("open-square: expected ApplyEdit, got {other:?}"),
    }

    // A QUOTE (same-char pair) OPENS on an empty buffer → `""` caret-between.
    let quote_dbl = commands.id_by_name("auto-pair-quote-double").unwrap();
    let mut doc = Document::from_text("");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 0),
        CommandInvocation::of(quote_dbl),
        &CancellationToken::never(),
    )
    .expect("quote (open) dispatches");
    match effect {
        Effect::ApplyEdit { edit, cursor, .. } => {
            assert!(
                matches!(&edit.kind, EditKind::Replace { text } if text == "\"\""),
                "inserts the quote pair, got {:?}",
                edit.kind
            );
            assert_eq!(cursor, Some(Position::new(0, 1)), "caret between the quotes");
        }
        other => panic!("quote-open: expected ApplyEdit, got {other:?}"),
    }

    // The SAME quote before a matching quote STEPS OVER (closing a just-opened
    // pair). Buffer `""`, caret between at (0,1).
    let mut doc = Document::from_text("\"\"");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 1),
        CommandInvocation::of(quote_dbl),
        &CancellationToken::never(),
    )
    .expect("quote (skip) dispatches");
    match effect {
        Effect::SelectionChange(set) => {
            assert_eq!(
                set.primary().head,
                Position::new(0, 2),
                "caret stepped past the existing quote"
            );
        }
        other => panic!("quote-skip: expected SelectionChange, got {other:?}"),
    }
}
