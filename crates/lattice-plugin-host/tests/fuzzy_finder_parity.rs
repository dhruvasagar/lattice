//! PH7.4d — the Phase-7 exit-gate proof: a WASM plugin replicates the native
//! `files` picker with candidate parity, using only generic seams (no bespoke
//! host code).
//!
//! Both sources walk the SAME temp tree: the native `FilesSource` via
//! `walk_files_for_picker` directly, the `fuzzy-finder` guest via the
//! capability-gated `host-services` `walk` (which reuses that same walk). This
//! test asserts the resulting candidate sets — the `OpenFile` routing paths and
//! the relative display strings — are identical. `accept` parity is checked too.
//!
//! Validation only: the plugin is never registered in the shipping editor
//! (built-ins stay native). Skips when the `wasm32-wasip2` plugin wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use lattice_core::Buffer;
use lattice_mode::CapabilitySet;
use lattice_picker::context::{ActiveBufferSnapshot, PickerContext};
use lattice_picker::outcome::PickerAcceptOutcome;
use lattice_picker::picker_sources::FilesSource;
use lattice_picker::{CandidateBatch, PickerInitResult, PickerSourceGenerator, RoutingPayload};
use lattice_plugin_host::{
    Capability, PluginBudget, PluginHost, PluginManifest, TrustTier, WasmPickerSource,
};
use lattice_protocol::Position;
use tempfile::TempDir;

fn plugin_wasm() -> Option<&'static str> {
    let path = env!("FUZZY_FINDER_WASM");
    (!path.is_empty()).then_some(path)
}

/// A temp tree with a handful of files at two depths, returned canonicalized so
/// both sources walk exactly the same root (native `FilesSource` canonicalizes
/// its root; the guest does not, so we hand both the already-canonical path).
fn temp_tree() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    std::fs::write(dir.path().join("b.txt"), "").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/c.rs"), "").unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    (dir, canonical)
}

fn with_ctx<R>(workspace_root: &str, f: impl FnOnce(&PickerContext<'_>) -> R) -> R {
    let buffer = Buffer::empty();
    let ctx = PickerContext {
        active_buffer: ActiveBufferSnapshot {
            buffer_id: 0,
            path: None,
            language: None,
            cursor: Position::new(0, 0),
            selection: None,
            buffer: &buffer,
            syntax_symbols: Vec::new(),
            syntax_highlights: Vec::new(),
        },
        workspace_root: workspace_root.into(),
        recent_files: &[],
        position_history: Vec::new(),
        buffers: Vec::new(),
        marks: Vec::new(),
        registers: Vec::new(),
    };
    f(&ctx)
}

/// `(display, open-file-path)` per candidate, sorted — the parity key.
fn parity_key(batch: &CandidateBatch) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = batch
        .iter()
        .map(|(cand, routing)| {
            let path = match routing {
                RoutingPayload::OpenFile { path } => path.to_string_lossy().into_owned(),
                other => panic!("expected OpenFile routing, got {other:?}"),
            };
            (cand.text.clone(), path)
        })
        .collect();
    rows.sort();
    rows
}

/// Native `files` candidate set for `root`.
fn native_files(root: &str) -> CandidateBatch {
    let source = FilesSource::new();
    let result = with_ctx(root, |ctx| source.init(ctx, &[root.to_string()])).unwrap();
    match result {
        PickerInitResult::Inline(pairs) => pairs,
        other => panic!("native files is Inline, got {other:?}"),
    }
}

/// The `fuzzy-finder` plugin's candidate set for `root`, driven through the
/// `WasmPickerSource` adapter (init Future resolved off-thread).
async fn plugin_files(root: &str, grant_root: PathBuf) -> CandidateBatch {
    let tmp = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();
    let component = host
        .compile(&std::fs::read(plugin_wasm().unwrap()).unwrap())
        .unwrap();
    // Grant fs:read on the walked root so the guest's `walk` is permitted.
    let manifest = PluginManifest::new(
        "fuzzy-finder",
        vec![Capability::FsRead(grant_root)],
        CapabilitySet::empty(),
    );
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());
    let source = WasmPickerSource::connect(client).await.unwrap();
    let init = with_ctx(root, |ctx| source.init(ctx, &[root.to_string()])).unwrap();
    match init {
        PickerInitResult::Future(fut) => fut.await.expect("guest produced candidates"),
        other => panic!("adapter init is a Future, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_fuzzy_finder_matches_native_files_candidates() {
    let Some(_) = plugin_wasm() else {
        eprintln!("SKIP: fuzzy_finder_parity — plugin not built (add wasm32-wasip2)");
        return;
    };
    let (_dir, root) = temp_tree();
    let root_str = root.to_str().unwrap();

    let native = native_files(root_str);
    let plugin = plugin_files(root_str, root.clone()).await;

    assert_eq!(native.len(), 3, "native walked the three files");
    assert_eq!(
        parity_key(&native),
        parity_key(&plugin),
        "the WASM plugin's candidate set (display + OpenFile path) matches native `files`"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_fuzzy_finder_accept_matches_native() {
    let Some(_) = plugin_wasm() else {
        return;
    };
    let (_dir, root) = temp_tree();
    let target = root.join("a.rs");
    let routing = RoutingPayload::OpenFile {
        path: target.clone(),
    };

    // Native accept.
    let native = FilesSource::new();
    let native_outcome = with_ctx(root.to_str().unwrap(), |ctx| native.accept(ctx, &routing))
        .expect("native accept");

    // Plugin accept (async seam).
    let tmp = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();
    let component = host
        .compile(&std::fs::read(plugin_wasm().unwrap()).unwrap())
        .unwrap();
    let manifest = PluginManifest::new(
        "fuzzy-finder",
        vec![Capability::FsRead(root.clone())],
        CapabilitySet::empty(),
    );
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());
    let source = WasmPickerSource::connect(client).await.unwrap();
    let fut = with_ctx(root.to_str().unwrap(), |ctx| {
        source.accept_async(ctx, &routing)
    })
    .expect("plugin resolves accept via accept_async");
    let plugin_outcome = fut.await.expect("plugin accept");

    match (native_outcome, plugin_outcome) {
        (PickerAcceptOutcome::OpenFile { path: n }, PickerAcceptOutcome::OpenFile { path: p }) => {
            assert_eq!(n, p, "accept resolves the same OpenFile outcome")
        }
        other => panic!("expected matching OpenFile outcomes, got {other:?}"),
    }
}
