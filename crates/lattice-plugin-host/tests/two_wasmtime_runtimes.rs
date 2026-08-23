//! LG.0 — the two-wasmtime gate for plugin-contributed languages.
//!
//! Design: [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §3.
//!
//! tree-sitter's `wasm` feature depends on `wasmtime-c-api 36`; this crate
//! runs `wasmtime 46`. Cargo links both happily, and that is the concern
//! rather than the reassurance: **both install `SIGSEGV`/`SIGBUS` handlers**
//! for guard-page traps, and two independently-initialised runtimes fighting
//! over them fails as a mysterious crash under load months later — not as a
//! compile error now.
//!
//! So this does not merely check that the two link. It stands both up and
//! then **forces a real guest trap while the other runtime is live**, which
//! is the only version of the question worth answering: a test that creates
//! two engines and asserts nothing crashed would pass against the broken
//! world.
//!
//! **Runs unconditionally.** It was gated behind a spike feature while the
//! question was open; LG.3b made `tree-sitter/wasm` a plain dependency
//! (+5.7 MiB, no runtime cost when unused), so this now guards the default
//! CI path rather than waiting for someone to opt in — which is the only
//! version of a regression guard worth having.
//!
//! **This file stays after the gate closes.** A wasmtime bump on either side
//! re-opens the question, and this is what re-answers it.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Buffer;
use lattice_core::BufferId;
use lattice_grammar::dispatcher::execute_motion_only;
use lattice_grammar::error::CommandError;
use lattice_grammar::registry::GrammarEnv;
use lattice_grammar::{CancellationToken, CommandInvocation, CommandRegistry};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::position::Position;
use tempfile::TempDir;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("GRAMMAR_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Stand tree-sitter's wasm runtime up. Creating the store is what
/// initialises `wasmtime-c-api 36` and installs its signal handlers — the
/// event this gate is about.
fn tree_sitter_store() -> tree_sitter::WasmStore {
    let engine = tree_sitter::wasmtime::Engine::default();
    tree_sitter::WasmStore::new(&engine).expect("tree-sitter wasm store stands up")
}

#[test]
fn both_runtimes_initialise_in_one_process() {
    let store = tree_sitter_store();
    assert_eq!(
        store.language_count(),
        0,
        "a fresh store holds no languages"
    );

    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("the plugin host's own wasmtime 46 engine stands up beside it");
    drop(host);
    drop(store);
}

/// The one that matters.
///
/// A guest trap in wasmtime 46 must still be caught by wasmtime 46 and turned
/// into the host's typed `CommandError::Plugin` — not swallowed, not
/// mis-routed, and above all not a process abort — **while tree-sitter's
/// wasmtime 36 is live with its own handlers installed.**
///
/// Order is deliberate: tree-sitter's runtime initialises FIRST, so its
/// handlers are registered before the plugin host's. If the later
/// registration clobbers the earlier one (or vice versa), this is where it
/// surfaces.
#[test]
fn a_guest_trap_is_still_caught_with_tree_sitters_runtime_live() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar-guest fixture not built");
        return;
    }
    let store = tree_sitter_store();
    let err = trap_a_guest();
    assert!(
        matches!(err, CommandError::Plugin(_)),
        "the guest's trap was caught and quarantined by wasmtime 46 while \
         tree-sitter's wasmtime 36 was live. Got {err:?}. If this fails — or \
         the process dies instead of reaching the assertion — the two runtimes \
         do NOT coexist, and plugin-languages.md §6's fallback applies."
    );
    drop(store);
}

/// The other clobbering direction.
///
/// Signal-handler registration is order-dependent: whichever runtime
/// initialises second may or may not chain to the first's handler. The test
/// above proves tree-sitter-then-host; this proves host-then-tree-sitter, and
/// between them they cover both ways the two can be brought up in a real
/// session (a plugin loading before any file is opened, or after).
#[test]
fn a_guest_trap_is_still_caught_when_tree_sitters_runtime_starts_second() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar-guest fixture not built");
        return;
    }
    // Host first this time; tree-sitter's engine comes up afterwards, then the
    // trap is forced.
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    drop(host);
    let store = tree_sitter_store();

    let err = trap_a_guest();
    assert!(
        matches!(err, CommandError::Plugin(_)),
        "a trap is still caught when tree-sitter's runtime initialised AFTER \
         the plugin host's. Got {err:?}"
    );
    drop(store);
}

/// Instantiate the grammar fixture and drive its `traps` motion, returning the
/// error the host produced. Panics if the trap did not produce one — a
/// successful dispatch here would mean the guest never trapped, which would
/// make every assertion above vacuous.
fn trap_a_guest() -> CommandError {
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile grammar fixture");
    let manifest = PluginManifest::new("lg0-fixture", Vec::new(), CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
            None,
        )
        .expect("instantiate + register-grammar");

    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    let traps_id = registry
        .id_by_name("traps")
        .expect("the fixture's trapping motion");

    let buffer = Buffer::from_text("l0\nl1\nl2\n");
    let cancel = CancellationToken::never();
    execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(traps_id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect_err("a trapping motion is a typed error, not a success or a panic")
}
