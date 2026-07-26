//! AP.3 (also TS.3) — the auto-pair `manual` style, end to end through the real
//! loaded plugin. The pair keys self-insert; a single close key
//! (`auto-pair-close-manual`) closes the nearest unmatched opener, found by
//! scanning the enclosing lexical scope backward (`find_pair`) — bounded via the
//! tree-sitter seam's `enclosing` query, with a line-capped fallback where
//! there's no parse tree. This makes auto-pair the first end-to-end consumer of
//! the tree-sitter seam.
//!
//! The style is read live from `auto-pair.style` — the grammar guest reads the
//! SHARED config registry (wired into the grammar store at instantiate time,
//! AP.3), so `parse_and_set_command("auto-pair.style=manual")` after load flips
//! behavior with no re-registration. Skips when the plugin wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::any::Any;
use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_core::Document;
use lattice_core::buffers::BufferId;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::dispatcher::{execute, execute_with_env};
use lattice_grammar::registry::{CommandRegistry, TextObjectEnv};
use lattice_grammar::{CancellationToken, CommandRegistryHandle, Effect};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::edit::EditKind;
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;
use lattice_syntax::{Lang, Syntax};

fn auto_pair_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
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

/// Load auto-pair through the real loader, keeping the loader alive (it holds the
/// grammar guest the trampoline dispatches into). Returns the command + config
/// registries.
async fn load(
    base: &std::path::Path,
) -> Option<(PluginLoader, CommandRegistryHandle, Arc<ConfigRegistry>)> {
    let wasm = auto_pair_wasm()?;
    let plugins_dir = base.join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let command_registry = empty_command_registry();
    let config_registry = Arc::new(ConfigRegistry::default());
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry.clone()),
            mode_registry: Some(empty_mode_registry()),
            config_registry: Some(config_registry.clone()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    );
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "auto-pair loads");
    Some((loader, command_registry, config_registry))
}

/// A parsed Rust snapshot, type-erased for the per-dispatch env.
fn rust_snapshot(src: &str) -> Arc<dyn Any + Send + Sync> {
    let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    s.parse(src);
    Arc::new(s.snapshot_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_close_key_closes_the_nearest_unmatched_opener_in_scope() {
    let base = tempfile::tempdir().unwrap();
    let Some((_loader, commands, config)) = load(base.path()).await else {
        eprintln!("skipping: auto-pair wasm not built");
        return;
    };
    config
        .parse_and_set_command("auto-pair.style=manual")
        .unwrap();
    let commands = commands.load();
    let close = commands.id_by_name("auto-pair-close-manual").unwrap();

    // `fn m() {\n    foo(\n}\n` — the caret sits right after the unmatched `(`.
    // The tree-sitter `enclosing` scope query (the plugin holds the `tree-sitter`
    // grant) bounds the backward scan to the block; `find_pair` returns `)`.
    let src = "fn m() {\n    foo(\n}\n";
    let snapshot = rust_snapshot(src);
    let mut doc = Document::from_text(src);
    let env = TextObjectEnv {
        syntax: Some(&snapshot),
        ..Default::default()
    };
    let effect = execute_with_env(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(1, 8), // just after `(`
        CommandInvocation::of(close),
        &CancellationToken::never(),
        env,
    )
    .expect("manual close dispatches");
    match effect {
        Effect::ApplyEdit { edit, cursor, .. } => {
            assert!(
                matches!(&edit.kind, EditKind::Replace { text } if text == ")"),
                "closes the unmatched ( with ), got {:?}",
                edit.kind
            );
            assert_eq!(
                cursor,
                Some(Position::new(1, 9)),
                "caret after the inserted )"
            );
        }
        other => panic!("expected ApplyEdit inserting ), got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_close_key_falls_through_when_nothing_is_unmatched() {
    let base = tempfile::tempdir().unwrap();
    let Some((_loader, commands, config)) = load(base.path()).await else {
        return;
    };
    config
        .parse_and_set_command("auto-pair.style=manual")
        .unwrap();
    let commands = commands.load();
    let close = commands.id_by_name("auto-pair-close-manual").unwrap();

    // No unmatched opener above the caret → decline (fall through, §6). No tree
    // → the line-capped fallback scan of `abc` finds nothing open.
    let mut doc = Document::from_text("abc\n");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 3),
        CommandInvocation::of(close),
        &CancellationToken::never(),
    )
    .expect("manual close dispatches");
    assert!(
        matches!(effect, Effect::Declined),
        "nothing unmatched → declines, got {effect:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_style_pair_keys_self_insert() {
    let base = tempfile::tempdir().unwrap();
    let Some((_loader, commands, config)) = load(base.path()).await else {
        return;
    };
    config
        .parse_and_set_command("auto-pair.style=manual")
        .unwrap();
    let commands = commands.load();
    let open = commands.id_by_name("auto-pair-open-round").unwrap();

    // In manual style, `(` self-inserts — the action DECLINES so the builtin
    // handles it (no auto-pairing).
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
    assert!(
        matches!(effect, Effect::Declined),
        "manual style: the open key declines (self-insert), got {effect:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_style_close_key_declines() {
    let base = tempfile::tempdir().unwrap();
    let Some((_loader, commands, _config)) = load(base.path()).await else {
        return;
    };
    // Default style is `auto`; the manual close key does nothing → declines, so
    // `<C-j>` does whatever else it's bound to.
    let commands = commands.load();
    let close = commands.id_by_name("auto-pair-close-manual").unwrap();
    let mut doc = Document::from_text("foo(\n");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 4),
        CommandInvocation::of(close),
        &CancellationToken::never(),
    )
    .expect("close dispatches");
    assert!(
        matches!(effect, Effect::Declined),
        "auto style: the manual close key declines, got {effect:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backspace_deletes_an_empty_pair_else_declines() {
    let base = tempfile::tempdir().unwrap();
    let Some((_loader, commands, _config)) = load(base.path()).await else {
        return;
    };
    let commands = commands.load();
    let bs = commands.id_by_name("auto-pair-backspace").unwrap();

    // Caret between `()` → delete BOTH chars, caret to the start.
    let mut doc = Document::from_text("()");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 1),
        CommandInvocation::of(bs),
        &CancellationToken::never(),
    )
    .expect("backspace dispatches");
    match effect {
        Effect::ApplyEdit { edit, cursor, .. } => {
            assert!(
                matches!(&edit.kind, EditKind::Replace { text } if text.is_empty()),
                "deletes the empty pair, got {:?}",
                edit.kind
            );
            assert_eq!(cursor, Some(Position::new(0, 0)));
        }
        other => panic!("expected ApplyEdit deleting the pair, got {other:?}"),
    }

    // Not inside a pair (`ab`, caret at 1) → decline to the builtin backspace.
    let mut doc = Document::from_text("ab");
    let effect = execute(
        &commands,
        &mut doc,
        BufferId(1),
        Position::new(0, 1),
        CommandInvocation::of(bs),
        &CancellationToken::never(),
    )
    .expect("backspace dispatches");
    assert!(
        matches!(effect, Effect::Declined),
        "not in a pair → declines, got {effect:?}"
    );
}
