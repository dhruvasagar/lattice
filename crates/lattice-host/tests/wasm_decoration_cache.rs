//! PL8.E — the host-side decoration drive: `maybe_refresh_wasm_decorations`
//! spawns registered producers OFF the render path, writes the per-buffer cache
//! the renderer reads, bumps the paint generation (so the gutter repaints
//! off-keystroke), and — critically — keeps the last-good snapshot when a
//! producer errs (zero flicker, §8).
//!
//! No WASM here: a native stub `AsyncGutterDecorationSource` stands in for a
//! plugin producer, so this pins the host cache mechanics in isolation. The WASM
//! producer + the drain that registers it are proven by
//! `lattice-plugin-loader`'s `decoration_drain.rs`; the renderer read/merge is
//! the two lockstep partition points in the TUI/GPUI peers.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::per_buffer_cache::PerBufferCacheExt;
use lattice_host::wasm_decorations::{WasmDecorationState, WasmGutterDecorationCache};
use lattice_mode::{
    AsyncGutterDecorationSource, DecorationFuture, GutterDecoration,
    GutterDecorationSourceRegistry, GutterDecorationSourceRegistryHandle,
};

/// SG.4b: every gutter decoration is a `Sign` now, so a test that wants a
/// "diff/change mark" asks the boot-registered registry for the built-in of
/// that name. `Editor::boot` registers them, so this resolves in any test
/// built on a real boot — which is also what makes these assertions mean
/// something rather than comparing two `Default` ids.
fn builtin_sign(editor: &Editor, name: &str) -> lattice_mode::SignId {
    editor
        .services
        .get::<lattice_mode::SignRegistryHandle>()
        .expect("boot registers the sign registry")
        .load()
        .id_of(name)
        .unwrap_or_else(|| panic!("`{name}` is a registered built-in sign"))
}

/// A native decoration producer standing in for a WASM one. Either yields a
/// fixed mark set or errs — enough to exercise the write path and the
/// keep-prior (no-flicker) path. Counts calls so a test can assert the producer
/// ran (or didn't).
#[derive(Debug)]
struct StubProducer {
    id: u64,
    result: Result<Vec<GutterDecoration>, String>,
    calls: Arc<AtomicU64>,
}

impl AsyncGutterDecorationSource for StubProducer {
    fn source_id(&self) -> u64 {
        self.id
    }
    fn produce(
        &self,
        _buffer: u64,
        _path: Option<std::path::PathBuf>,
        _lines: u32,
    ) -> DecorationFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

fn registry_with(producer: StubProducer) -> GutterDecorationSourceRegistryHandle {
    let mut r = GutterDecorationSourceRegistry::new();
    r.register(Arc::new(producer));
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
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

#[tokio::test]
async fn refresh_populates_the_cache_off_the_render_path_and_wakes_paint() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;
    let marks = vec![
        GutterDecoration::Sign {
            line: 0,
            sign: builtin_sign(&editor, "diff.change"),
        },
        GutterDecoration::Sign {
            line: 1,
            sign: builtin_sign(&editor, "diagnostic.error"),
        },
    ];
    let calls = Arc::new(AtomicU64::new(0));
    editor.wasm_decorations = WasmDecorationState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(marks.clone()),
        calls: calls.clone(),
    }));
    settle(&editor).await;
    let gen_before = editor.wasm_decorations.generation.load(Ordering::Relaxed);

    editor.maybe_refresh_wasm_decorations();

    // The producer ran off the actor thread and its write fired `async_landed`
    // (the off-keystroke paint wake).
    assert!(
        landed_within(&editor, 2).await,
        "a decoration write must fire async_landed so the gutter repaints without a keystroke"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the producer was polled once"
    );
    let cache = editor
        .wasm_decorations
        .cache
        .get_for(buffer)
        .expect("the refresh wrote this buffer's decorations");
    assert_eq!(
        cache.decorations, marks,
        "the cached marks are the producer's output"
    );
    assert!(
        editor.wasm_decorations.generation.load(Ordering::Relaxed) > gen_before,
        "the paint generation bumped (folded into compute_paint_revision)"
    );

    // Idempotent: a second refresh at the same version + producer set does NOT
    // re-poll (version + single-flight gated).
    editor.maybe_refresh_wasm_decorations();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "an unchanged (buffer, version, producers) refresh does not re-run the producer"
    );
}

#[tokio::test]
async fn erroring_producer_keeps_the_prior_snapshot_zero_flicker() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\n"));
    let buffer = editor.document_buffer_id;

    // Seed a prior good snapshot (as if an earlier refresh landed).
    let prior = WasmGutterDecorationCache {
        document_version: 0,
        decorations: vec![GutterDecoration::Sign {
            line: 0,
            sign: builtin_sign(&editor, "diff.add"),
        }],
    };
    let calls = Arc::new(AtomicU64::new(0));
    editor.wasm_decorations = WasmDecorationState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Err("boom".to_string()),
        calls: calls.clone(),
    }));
    // Insert the prior snapshot AFTER swapping in the registry so the cache slot
    // is the one the refresh writes through.
    editor
        .wasm_decorations
        .cache
        .insert_for(buffer, prior.clone());
    settle(&editor).await;

    editor.maybe_refresh_wasm_decorations();
    // The erroring producer must NOT fire async_landed (no write happened).
    assert!(
        !landed_within(&editor, 1).await,
        "an all-error refresh writes nothing, so it fires no paint wake"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the producer was polled and errored"
    );
    let cache = editor
        .wasm_decorations
        .cache
        .get_for(buffer)
        .expect("the prior snapshot is untouched");
    assert_eq!(
        cache.decorations, prior.decorations,
        "an erroring producer keeps the last-good marks — no clear, no flicker"
    );
}

/// OA.30 — a guest saying "my answer changed" re-runs the producer, though the
/// document and the producer set are both unchanged.
///
/// **The counterpart of the idempotence assertion above, and the reason the
/// seam exists.** That test pins the good behaviour — an unchanged refresh does
/// not re-poll — and this pins its cost: a producer whose output depends on its
/// OWN state was cached forever, because a read-only view's version never moves
/// and the registry never changes while nothing loads. The agenda's bulk marks
/// are exactly that shape: a mark is guest state over an unchanged buffer.
///
/// Without the epoch this test fails on the second `calls` assertion — the
/// producer is never asked again and the new marks never paint, with no error
/// anywhere.
#[tokio::test]
async fn a_guest_refresh_request_re_runs_the_producer_at_the_same_version() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\n"));
    let calls = Arc::new(AtomicU64::new(0));
    let epoch: lattice_mode::DecorationEpochHandle =
        Arc::new(lattice_mode::DecorationEpoch::default());
    editor.wasm_decorations = WasmDecorationState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![GutterDecoration::Sign {
            line: 0,
            sign: builtin_sign(&editor, "diff.change"),
        }]),
        calls: calls.clone(),
    }))
    .with_decoration_epoch(epoch.clone());
    settle(&editor).await;

    editor.maybe_refresh_wasm_decorations();
    assert!(landed_within(&editor, 2).await, "the first refresh landed");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    // Unchanged: still gated, which is the property the epoch must not break.
    editor.maybe_refresh_wasm_decorations();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "no bump, no document change — still no re-poll"
    );

    // The guest toggled something its producer reads.
    epoch.bump();
    editor.maybe_refresh_wasm_decorations();
    assert!(
        landed_within(&editor, 2).await,
        "a guest-requested refresh must fire async_landed too — the mark has to \
         reach the gutter without a keystroke"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "the producer is asked again after `refresh-decorations`"
    );

    // And one bump is one refresh: the pump records the value it acted on, so a
    // tick with no new bump does not re-poll forever.
    editor.maybe_refresh_wasm_decorations();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "one bump is one refresh, not a permanent re-poll"
    );
}

/// SG.1 — `<C-l>` re-asks every producer.
///
/// `:redraw` is the escape hatch for a display that has gone wrong, and a
/// producer's signs are part of that display. Neither of the pump's ordinary
/// triggers moves on a redraw — the registry is unchanged and the document
/// version is unchanged — so without an explicit request the one thing the user
/// pressed the key to fix is the one thing that survives it.
#[tokio::test]
async fn a_redraw_re_asks_the_producer() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\n"));
    let calls = Arc::new(AtomicU64::new(0));
    let epoch: lattice_mode::DecorationEpochHandle =
        Arc::new(lattice_mode::DecorationEpoch::default());
    editor.wasm_decorations = WasmDecorationState::with_registry(registry_with(StubProducer {
        id: 1,
        result: Ok(vec![GutterDecoration::Sign {
            line: 0,
            sign: builtin_sign(&editor, "diff.change"),
        }]),
        calls: calls.clone(),
    }))
    .with_decoration_epoch(epoch.clone());
    settle(&editor).await;

    editor.maybe_refresh_wasm_decorations();
    assert!(landed_within(&editor, 2).await);
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    editor.do_redraw_screen();
    editor.maybe_refresh_wasm_decorations();
    assert!(
        landed_within(&editor, 2).await,
        "the redraw's refetch must land without a keystroke"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "`<C-l>` must re-ask the producer — a stale sign is exactly what it is \
         pressed to fix"
    );
}
