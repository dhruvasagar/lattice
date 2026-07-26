//! TS.1 end-to-end — a grammar plugin queries the parse tree through the
//! `tree-snapshot` seam (plugin-treesitter-seam.md §3). The `multiseam-guest`
//! fixture's `multiseam-enclosing` action calls `enclosing(cursor, ["block"])`
//! on the borrowed handle and echoes `<language>:<kind>:<named-child-count>` —
//! observable proof that the handle crossed, the walk ran host-side, and a node
//! projection came back.
//!
//! The seam is exercised through the FULL dispatch path: `execute_with_env`
//! carries the (type-erased) snapshot on the per-dispatch env → `execute_action`
//! builds `ActionContext::syntax` → the trampoline downcasts it and mints the
//! `option<borrow<tree-snapshot>>` → the guest queries it. A pre-parsed Rust
//! snapshot is injected directly (no editor async-parse timing).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::any::Any;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, TextObjectEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;
use lattice_syntax::{Lang, Syntax};

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Register the multiseam grammar seam into a fresh `CommandRegistry`, granting
/// `editor` capabilities (`TREE_SITTER` to let the tree cross).
fn register_grammar(editor: CapabilitySet) -> (CommandRegistry, tempfile::TempDir) {
    let path = guest_wasm().expect("caller checked the fixture exists");
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), editor);
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    (commands, dirs)
}

/// A parsed Rust snapshot, type-erased for the per-dispatch env (mirrors what the
/// host's Action gate does with the active buffer's `SyntaxHandle::snapshot()`).
fn rust_snapshot(src: &str) -> Arc<dyn Any + Send + Sync> {
    let mut syntax = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syntax.parse(src);
    Arc::new(syntax.snapshot_owned())
}

// `fn m() { let x = 1; }` — cursor on `x`, inside the block.
const SRC: &str = "fn m() { let x = 1; }\n";

fn cursor_on_x() -> Position {
    Position {
        line: 0,
        byte: SRC.find('x').unwrap() as u32,
    }
}

#[test]
fn a_granted_plugin_queries_the_enclosing_scope_through_the_seam() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let (commands, _dirs) = register_grammar(CapabilitySet::TREE_SITTER);
    let id = commands.id_by_name("multiseam-enclosing").unwrap();

    let snapshot = rust_snapshot(SRC);
    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    let env = TextObjectEnv {
        syntax: Some(&snapshot),
        ..Default::default()
    };
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        cursor_on_x(),
        CommandInvocation::of(id),
        &cancel,
        env,
    )
    .expect("the enclosing action dispatches through the sync trampoline");

    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => {
            // The block `{ let x = 1; }` is `block`, with one named child (the
            // `let_declaration`). rust:block:1 proves the handle crossed and the
            // host-side walk + projection ran.
            assert_eq!(text, "rust:block:1");
        }
        other => panic!("expected an Echo from the tree query, got {other:?}"),
    }
}

#[test]
fn a_granted_plugin_runs_a_query_through_the_seam() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    // TS.2: the guest compiles `(function_item name: (identifier) @fname)` and
    // runs it; `fn m() { … }` has one function named `m`, so the echo is
    // `1:fname:identifier` — proof the compiled query crossed, ran host-side, and
    // the capture node projection came back.
    let (commands, _dirs) = register_grammar(CapabilitySet::TREE_SITTER);
    let id = commands.id_by_name("multiseam-query").unwrap();

    let snapshot = rust_snapshot(SRC);
    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    let env = TextObjectEnv {
        syntax: Some(&snapshot),
        ..Default::default()
    };
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        cursor_on_x(),
        CommandInvocation::of(id),
        &cancel,
        env,
    )
    .expect("the query action dispatches");
    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => {
            assert_eq!(text, "1:fname:identifier");
        }
        other => panic!("expected an Echo from the query, got {other:?}"),
    }
}

#[test]
fn a_granted_plugin_walks_the_tree_with_a_cursor() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    // TS.2: the guest walks `root().walk().goto_first_named_child()` and echoes
    // `<moved>:<kind>` → `true:function_item`.
    let (commands, _dirs) = register_grammar(CapabilitySet::TREE_SITTER);
    let id = commands.id_by_name("multiseam-cursor").unwrap();

    let snapshot = rust_snapshot(SRC);
    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    let env = TextObjectEnv {
        syntax: Some(&snapshot),
        ..Default::default()
    };
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        cursor_on_x(),
        CommandInvocation::of(id),
        &cancel,
        env,
    )
    .expect("the cursor action dispatches");
    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => {
            assert_eq!(text, "true:function_item");
        }
        other => panic!("expected an Echo from the cursor walk, got {other:?}"),
    }
}

#[test]
fn a_plugin_without_the_grant_gets_no_tree_handle() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    // No TREE_SITTER grant — the trampoline passes `none` even though the buffer
    // parses, so the guest's `enclosing` query has no handle and returns `err`
    // (graceful no-op, not a trap). Proves the capability gate (design §5).
    let (commands, _dirs) = register_grammar(CapabilitySet::empty());
    let id = commands.id_by_name("multiseam-enclosing").unwrap();

    let snapshot = rust_snapshot(SRC);
    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    let env = TextObjectEnv {
        syntax: Some(&snapshot),
        ..Default::default()
    };
    let err = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        cursor_on_x(),
        CommandInvocation::of(id),
        &cancel,
        env,
    )
    .expect_err("no grant → the guest's tree query is a typed CommandError");
    assert!(
        matches!(err, lattice_grammar::CommandError::Plugin(_)),
        "the ungranted tree query degrades to CommandError::Plugin, got {err:?}"
    );
}

#[test]
fn a_buffer_with_no_parse_gets_no_tree_handle() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    // Granted, but the dispatch carries NO snapshot (env.syntax = None — a
    // plain-text buffer / parse pending). The guest gets `none` and degrades.
    let (commands, _dirs) = register_grammar(CapabilitySet::TREE_SITTER);
    let id = commands.id_by_name("multiseam-enclosing").unwrap();

    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    let err = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        cursor_on_x(),
        CommandInvocation::of(id),
        &cancel,
        TextObjectEnv::default(),
    )
    .expect_err("no snapshot → the guest's tree query is a typed CommandError");
    assert!(matches!(err, lattice_grammar::CommandError::Plugin(_)));
}
