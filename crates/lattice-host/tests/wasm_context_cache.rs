//! TC.3a — the host-side context drive: `maybe_refresh_wasm_context` spawns
//! registered producers OFF the render path, writes the per-buffer scope cache
//! the per-pane resolution reads, bumps the paint generation, and — critically
//! — keeps the last-good scopes when a producer errs (no blanking, §8).
//!
//! No WASM here: a native stub `AsyncContextSource` stands in for a plugin
//! producer, so this pins the host cache mechanics in isolation. The WASM
//! producer + the drain that registers it are proven by
//! `lattice-plugin-loader`'s `context_drain.rs`.
//!
//! The gate that matters most is the **parse-version** key. Decorations key on
//! the document version; scopes describe the tree, so keying them the same way
//! would re-drive the producer on every keystroke — the exact WASM-on-the-hot-
//! path the whole split exists to avoid.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lattice_cells::context::ContextScope;
use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::per_buffer_cache::PerBufferCacheExt;
use lattice_host::wasm_context::{ContextScopeCache, WasmContextState};
use lattice_mode::{
    AsyncContextSource, ContextFuture, ContextSourceRegistry, ContextSourceRegistryHandle,
};

/// A native context producer standing in for a WASM one. Counts calls so a test
/// can assert the producer ran (or, more importantly, did NOT).
#[derive(Debug)]
struct StubProducer {
    id: u64,
    result: Result<Vec<ContextScope>, String>,
    calls: Arc<AtomicU64>,
}

impl AsyncContextSource for StubProducer {
    fn source_id(&self) -> u64 {
        self.id
    }
    fn produce(
        &self,
        _buffer: u64,
        _path: Option<std::path::PathBuf>,
        _lines: u32,
        _syntax: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> ContextFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

fn registry_with(producer: StubProducer) -> ContextSourceRegistryHandle {
    let mut r = ContextSourceRegistry::new();
    r.register(Arc::new(producer));
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

fn scope(start: u32, end: u32) -> ContextScope {
    ContextScope {
        scope_start: start,
        scope_end: end,
        header_start: start,
        header_end: start,
    }
}

/// Drain notifies accumulated during boot so a later `landed_within` measures
/// only the refresh under test.
async fn settle(editor: &Editor) {
    while tokio::time::timeout(Duration::from_millis(100), editor.async_landed.notified())
        .await
        .is_ok()
    {}
}

async fn landed_within(editor: &Editor, secs: u64) -> bool {
    tokio::time::timeout(Duration::from_secs(secs), editor.async_landed.notified())
        .await
        .is_ok()
}

/// Poll until the cache holds scopes, or give up. The producer runs on the
/// background runtime, so the write is not synchronous with the pump call.
async fn wait_for_scopes(
    editor: &Editor,
    buffer: lattice_core::BufferId,
) -> Arc<ContextScopeCache> {
    for _ in 0..200 {
        if let Some(c) = editor.wasm_context.cache.get_for(buffer)
            && !c.scopes.is_empty()
        {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    editor
        .wasm_context
        .cache
        .get_for(buffer)
        .unwrap_or_default()
}

#[tokio::test]
async fn refresh_populates_the_cache_and_wakes_paint_without_a_keystroke() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;
    let calls = Arc::new(AtomicU64::new(0));
    editor.wasm_context = WasmContextState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![scope(0, 4), scope(1, 3)]),
        calls: calls.clone(),
    }));
    settle(&editor).await;
    let gen_before = editor.wasm_context.generation.load(Ordering::Relaxed);

    editor.maybe_refresh_wasm_context();

    // The wake is the point: scopes must reach the screen with NO intervening
    // keypress. A test that dispatched an action first would pass even on the
    // broken version, which is the hole this assertion exists to close.
    assert!(
        landed_within(&editor, 2).await,
        "a scope write must fire async_landed so the strip appears without a keystroke"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the producer ran once");

    let cached = wait_for_scopes(&editor, buffer).await;
    assert_eq!(cached.scopes.len(), 2);
    assert!(
        editor.wasm_context.generation.load(Ordering::Relaxed) > gen_before,
        "the paint generation advances so the repaint is not gated on a keystroke"
    );
}

#[tokio::test]
async fn scopes_are_cached_sorted_so_the_resolver_does_not_pay_for_it() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;
    // A tree-walk returns captures in traversal order, not outermost-first.
    editor.wasm_context = WasmContextState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![scope(3, 4), scope(0, 9), scope(1, 6)]),
        calls: Arc::new(AtomicU64::new(0)),
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_context();
    let cached = wait_for_scopes(&editor, buffer).await;

    let starts: Vec<u32> = cached.scopes.iter().map(|s| s.scope_start).collect();
    assert_eq!(
        starts,
        vec![0, 1, 3],
        "sorted once per reparse here, not once per cursor move in the resolver"
    );
}

#[tokio::test]
async fn a_second_refresh_at_the_same_parse_version_does_not_re_drive_the_producer() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;
    let calls = Arc::new(AtomicU64::new(0));
    editor.wasm_context = WasmContextState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![scope(0, 4)]),
        calls: calls.clone(),
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_context();
    let _ = wait_for_scopes(&editor, buffer).await;
    let after_first = calls.load(Ordering::Relaxed);

    // Ticks keep coming — cursor moves, scrolls, repaints. None of them changed
    // the parse, so none of them may reach the guest. This is the assertion
    // that keeps WASM off the keystroke path.
    //
    // It pins the OUTCOME, not one mechanism. Two independent guards deliver
    // it — the parse-version check and the single-flight `pending` check — and
    // this test still passes with either one defeated (verified by defeating
    // each in turn). Only removing BOTH turns it red. So do not read a green
    // run here as evidence that a guard you just deleted was dead code.
    for _ in 0..20 {
        editor.maybe_refresh_wasm_context();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        calls.load(Ordering::Relaxed),
        after_first,
        "an unchanged parse version must not re-drive the producer — twenty \
         ticks are twenty keystrokes' worth of opportunity to regress this"
    );
}

#[tokio::test]
async fn an_erroring_producer_keeps_the_prior_scopes_rather_than_blanking() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;

    // Seed a good set the way a successful refresh would.
    editor.wasm_context = WasmContextState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![scope(0, 4)]),
        calls: Arc::new(AtomicU64::new(0)),
    }));
    settle(&editor).await;
    editor.maybe_refresh_wasm_context();
    let seeded = wait_for_scopes(&editor, buffer).await;
    assert_eq!(seeded.scopes.len(), 1, "precondition: scopes are cached");

    // Now every producer errs. Swapping the registry changes its epoch, which
    // forces a refresh even though the parse version did not move.
    editor.wasm_context.registry = Some(registry_with(StubProducer {
        id: 2,
        result: Err("query failed".to_string()),
        calls: Arc::new(AtomicU64::new(0)),
    }));
    editor.maybe_refresh_wasm_context();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after = editor
        .wasm_context
        .cache
        .get_for(buffer)
        .unwrap_or_default();
    assert_eq!(
        after.scopes.len(),
        1,
        "a failed refresh keeps the last-good scopes — blanking the strip would \
         make a transient error read as the feature breaking"
    );
}

#[tokio::test]
async fn unloading_every_producer_clears_the_cache() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;
    editor.wasm_context = WasmContextState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![scope(0, 4)]),
        calls: Arc::new(AtomicU64::new(0)),
    }));
    settle(&editor).await;
    editor.maybe_refresh_wasm_context();
    assert_eq!(wait_for_scopes(&editor, buffer).await.scopes.len(), 1);

    // The plugin unloads: the registry swaps to an empty snapshot.
    editor.wasm_context.registry = Some(Arc::new(arc_swap::ArcSwap::from_pointee(
        ContextSourceRegistry::new(),
    )));
    editor.maybe_refresh_wasm_context();

    let after = editor
        .wasm_context
        .cache
        .get_for(buffer)
        .unwrap_or_default();
    assert!(
        after.scopes.is_empty(),
        "an unloaded plugin's scopes must stop painting — this is the one case \
         where clearing is right, and it is distinguishable from a failed \
         refresh because the producer set itself changed"
    );
}
