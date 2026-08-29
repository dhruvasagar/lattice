//! OR.1 end-to-end — one plugin, one store, N `wasmtime::Store`s.
//!
//! The unit tests in `plugin_store.rs` cover the store's own behaviour. What
//! they cannot cover is the claim the slice actually exists for: that a `put`
//! made by **one seam instance** of a plugin is visible to a `get` made by
//! **another**. Every seam builds its own `wasmtime::Store` with its own linear
//! memory, so a store scoped per instance would pass every unit test and still
//! leave org-roam's picker offering a node its own `<CR>` could not open —
//! silently, because each instance stays internally consistent.
//!
//! So these tests write through the ASYNC config seam and read back through the
//! SYNC grammar seam, on the same component, and assert the bytes crossed.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::manifest::Capability;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Dispatch a fixture grammar action through the real sync trampoline and
/// return the text it echoed.
fn run_grammar_action(
    host: &PluginHost,
    component: &wasmtime::component::Component,
    manifest: &PluginManifest,
    action: &str,
) -> String {
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(component, manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name(action).unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the store action dispatches through the sync trampoline");

    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => text,
        other => panic!("expected an Echo carrying the store answer, got {other:?}"),
    }
}

/// Drain the async config seam, which is where the fixture's `store-put`
/// happens.
async fn drain_config(
    host: &PluginHost,
    component: &wasmtime::component::Component,
    manifest: &PluginManifest,
) {
    let registry = Arc::new(ConfigRegistry::default());
    host.spawn_config_plugin(
        component,
        manifest,
        TrustTier::Bundled,
        PluginBudget::event(),
        &registry,
    )
    .await
    .expect("config drain instantiates");
}

fn granted(id: &str) -> PluginManifest {
    PluginManifest::new(id, vec![Capability::StateWrite], CapabilitySet::empty())
}

/// **The test that matters.** Two instances of one plugin, built on two
/// different linkers, sharing one store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_seams_write_is_another_seams_read() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = granted("multiseam");

    // Before anything wrote: an empty store, and a generation that has not moved.
    assert_eq!(
        run_grammar_action(&host, &component, &manifest, "multiseam-store-read"),
        "0:none",
        "nothing is stored yet, and reading does not move the generation"
    );

    // The ASYNC config seam writes…
    drain_config(&host, &component, &manifest).await;

    // …and the SYNC grammar seam — a different `wasmtime::Store`, different
    // linear memory, instantiated after the fact — reads it back.
    assert_eq!(
        run_grammar_action(&host, &component, &manifest, "multiseam-store-read"),
        "1:from-config",
        "the bytes crossed between two instances of one plugin, and the \
         generation the writer bumped is the one the reader sees"
    );
}

/// The reverse direction, and `keys`. A write from the keystroke path is
/// visible to the next instance too — the roam `id-create` shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_grammar_seam_can_write_too() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = granted("multiseam");

    drain_config(&host, &component, &manifest).await;
    assert_eq!(
        run_grammar_action(&host, &component, &manifest, "multiseam-store-write"),
        "ok:multiseam/from-grammar,multiseam/probe",
        "the grammar seam's own write lands beside the config seam's, and \
         `keys` returns the prefix sorted"
    );
}

/// Two plugins must not read each other's bytes. Scoping is by manifest id, so
/// a second id is a second store even though it is the same component.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_plugins_are_isolated_from_each_other() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();

    // `alpha` writes. `beta` never does.
    drain_config(&host, &component, &granted("alpha")).await;

    assert_eq!(
        run_grammar_action(&host, &component, &granted("alpha"), "multiseam-store-read"),
        "1:from-config",
        "alpha sees its own write"
    );
    assert_eq!(
        run_grammar_action(&host, &component, &granted("beta"), "multiseam-store-read"),
        "0:none",
        "beta sees nothing of alpha's — same component, different store"
    );
}

/// A store survives the host it was created under. This is the restart, without
/// the process exit: a second `PluginHost` over the same data dir reads back
/// what the first one persisted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_store_survives_a_restart() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let manifest = granted("multiseam");
    let bytes = std::fs::read(path).unwrap();

    {
        let host =
            PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
        let component = host.compile(&bytes).unwrap();
        drain_config(&host, &component, &manifest).await;
        // No explicit flush anywhere: dropping the host drops the last handle,
        // and `Drop` is what an editor exit actually goes through.
    }

    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&bytes).unwrap();
    assert_eq!(
        run_grammar_action(&host, &component, &manifest, "multiseam-store-read"),
        "1:from-config",
        "a fresh host reads back what the previous one persisted, generation included"
    );
}

/// A plugin that did not ask for `state:write` **degrades**: `get` answers
/// nothing, `put` says why, and everything else about the plugin keeps working.
/// Not a failed load, not a trap — the `config_registry` / `event_emit` posture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ungranted_plugin_degrades_rather_than_failing() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    // No `state:write` in the request.
    let manifest = PluginManifest::new("ungranted", Vec::new(), CapabilitySet::empty());

    // The write on the async seam is refused, and the seam still registers its
    // option — the plugin is not taken down by a call it was not allowed.
    drain_config(&host, &component, &manifest).await;

    assert_eq!(
        run_grammar_action(&host, &component, &manifest, "multiseam-store-read"),
        "0:none",
        "an ungranted read answers `none`, not an error the guest has to handle"
    );
    let written = run_grammar_action(&host, &component, &manifest, "multiseam-store-write");
    assert!(
        written.starts_with("err(") && written.contains("state:write"),
        "an ungranted write names the capability to add: {written}"
    );
    assert!(
        written.ends_with(":"),
        "…and `keys` answers empty rather than leaking a store: {written}"
    );
}
