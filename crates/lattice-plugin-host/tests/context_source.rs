//! TC.2 — the sticky-context producer seam, driven through a real guest.
//!
//! Instantiates the `context-guest` fixture (a `wasm32-wasip2` `context-plugin`
//! component) via [`PluginHost::spawn_context_source`], drives its
//! `context-scopes` producer through the [`WasmContextSource`] adapter +
//! `ContextActor` bridge, and asserts the native result — proving the whole seam
//! end to end OFF the render path:
//!
//!   - the owned `context-request` projection crosses in,
//!   - **a `borrow<tree-snapshot>` survives an ASYNC guest suspension** and is
//!     navigable on the far side. This is the load-bearing one: every other
//!     `borrow<>` in the repo is in the SYNC grammar world, so this seam is the
//!     first to lend a host resource across a suspension. If it did not work,
//!     the fallback would be a host import that hands the guest an *owned*
//!     snapshot by buffer id — a wider capability (any buffer's tree, any time)
//!     that call-scoped lending avoids. The fixture walks the tree for real, so
//!     a dead borrow fails the assertion rather than silently returning
//!     constants.
//!   - the `list<context-scope>` crosses back and converts to native
//!     `ContextScope`s,
//!   - a buffer with no parse yields an EMPTY set rather than an error ("no tree
//!     yet" is a normal state),
//!   - an empty buffer degrades to a guest `err` the adapter surfaces (the
//!     caller keeps the prior cached scopes — no blanking, §8).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier, WasmContextSource};
use lattice_syntax::{Lang, Syntax, SyntaxSnapshot};
use tempfile::TempDir;

/// The fixture context component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("CONTEXT_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Instantiate the fixture + spawn its actor; returns the host-facing producer.
async fn source(host: &PluginHost) -> WasmContextSource {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile context fixture");
    let manifest = PluginManifest::new("context-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_context_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::context(),
            &Arc::new(lattice_runtime::EventBus::new()),
            // No config registry: this fixture reads no options, and passing
            // `None` keeps the seam test hermetic from the option layer.
            None,
        )
        .await
        .expect("spawn context source");
    tokio::spawn(actor.run());
    WasmContextSource::new(client)
}

/// Three top-level items, so the fixture's "one scope per named child of root"
/// walk has something unambiguous to find.
const SRC: &str = "fn a() {\n    let x = 1;\n}\n\nstruct S {\n    f: u32,\n}\n\nfn b() {}\n";

fn parsed() -> Arc<SyntaxSnapshot> {
    let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    s.parse(SRC);
    Arc::new(s.snapshot_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_walks_the_borrowed_tree_and_returns_scopes() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: context fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    let scopes = src
        .context_scopes(
            7,
            Some(std::path::Path::new("src/lib.rs")),
            SRC.lines().count() as u32,
            Some(parsed()),
        )
        .await
        .expect("producer returns scopes");

    // Three top-level items → three named children of the root.
    assert_eq!(
        scopes.len(),
        3,
        "one scope per named child of the root; got {scopes:?}"
    );
    // `fn a` spans rows 0..=2, `struct S` rows 4..=6, `fn b` row 8. Asserting
    // the actual line numbers is what proves the borrow was LIVE — a dead or
    // empty handle could not have produced them.
    assert_eq!((scopes[0].scope_start, scopes[0].scope_end), (0, 2));
    assert_eq!((scopes[1].scope_start, scopes[1].scope_end), (4, 6));
    assert_eq!((scopes[2].scope_start, scopes[2].scope_end), (8, 8));
    // Header defaults to the scope's first line for every one of them.
    for s in &scopes {
        assert_eq!(s.header_start, s.scope_start);
        assert_eq!(s.header_end, s.scope_start);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffer_with_no_parse_yields_an_empty_set_not_an_error() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: context fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    // `None` tree = plain text, or a parse still pending. That is a normal
    // state: the host should cache "no scopes", NOT keep a stale set, so the
    // guest must return `Ok(empty)` rather than `Err`.
    let scopes = src
        .context_scopes(7, None, 12, None)
        .await
        .expect("no parse is not a failure");
    assert!(scopes.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_buffer_degrades_gracefully_to_a_guest_err() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: context fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    // line_count == 0 → the guest returns a WIT `err` (not a trap); the adapter
    // surfaces it as `Err`, and a boot-wired host keeps the buffer's prior
    // cached scopes rather than clearing them (no blanking, §8).
    let err = src
        .context_scopes(7, None, 0, Some(parsed()))
        .await
        .expect_err("empty buffer yields a graceful guest err");
    assert!(
        err.contains("empty buffer"),
        "graceful guest err surfaced, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_produce_calls_reuse_the_same_store() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: context fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    // The actor pushes an owned tree resource into the store's table per call
    // and deletes it after. Three calls in a row prove the reclaim actually
    // happens — a leak would grow the table and, more importantly, a missing
    // delete after a failed call would poison the next one.
    for _ in 0..3 {
        let scopes = src
            .context_scopes(7, None, SRC.lines().count() as u32, Some(parsed()))
            .await
            .expect("each call succeeds independently");
        assert_eq!(scopes.len(), 3);
    }

    // Interleave a failing call: the table entry must still be reclaimed, so
    // the call after it behaves exactly like the ones before.
    let _ = src.context_scopes(7, None, 0, Some(parsed())).await;
    let after = src
        .context_scopes(7, None, SRC.lines().count() as u32, Some(parsed()))
        .await
        .expect("a guest err must not poison the next call");
    assert_eq!(after.len(), 3);
}
