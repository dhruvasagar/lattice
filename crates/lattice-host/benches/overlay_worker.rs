//! Phase 5.8.AF.6 / Slice X2.9 (gut + rename display-line B4.2):
//! overlay worker bench.
//!
//! Simulates a 60Hz scroll burst against the worker's `recompute`
//! decision function and measures three distinct decision paths:
//!
//! - `cache_hit` -- steady-state cursor blink with no input
//!   changes. Should be ~ns: a key compare + Arc swap is the
//!   entire body.
//! - `recompute` -- cache miss, current snapshot. The full
//!   `bucket_static_overlays` walk runs. This is the worker's cost
//!   ceiling per scroll/edit tick.
//! - `stale_snapshot_hold` -- cache miss against a snapshot that
//!   hasn't caught up to the document's `text_version` yet. Holds
//!   prior quads + bumps the key; sub-µs.
//!
//! Each bench seeds an `all_matches` payload so the overlay bucket
//! is non-empty (the steady-state no-overlay path short-circuits to
//! an empty Vec, which would make the recompute bench measure
//! nothing interesting).
//!
//! display-line B4.2: the prior `build_rows` / `weave_row` (deleted
//! span/row cache) benches went away with the cache; only the
//! overlay-bucket decision paths remain.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench overlay_worker

use std::sync::Arc;

use arc_swap::ArcSwap;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_host::overlay_worker::{WorkerDecision, recompute};
use lattice_host::render_state::{
    ActiveDocumentRenderState, InlayHintRow, RenderState, StaticOverlayQuads, SyntaxRenderState,
};

fn rust_corpus(n_fns: usize) -> String {
    let mut s = String::with_capacity(n_fns * 80);
    for i in 0..n_fns {
        s.push_str(&format!(
            "fn handler_{i:04}(input: &str) -> Result<Output, Error> {{\n    let mut acc = 0;\n    if input.is_empty() {{\n        return Ok(acc);\n    }}\n    Ok(acc + 1)\n}}\n\n"
        ));
    }
    s
}

/// Build a payload of `n` single-line search matches across the
/// first `n` lines so the overlay bucket has work to do.
fn matches(n: u32) -> Vec<lattice_protocol::position::Range> {
    (0..n)
        .map(|line| lattice_protocol::position::Range {
            start: lattice_protocol::position::Position { line, byte: 0 },
            end: lattice_protocol::position::Position { line, byte: 2 },
        })
        .collect()
}

fn build_rs(
    text: &str,
    scroll: u32,
    viewport_height: u32,
    fold_hash: u64,
    text_version: u64,
) -> (
    ArcSwap<RenderState>,
    Arc<lattice_syntax::SyntaxHandle>,
    Arc<ArcSwap<StaticOverlayQuads>>,
) {
    let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .expect("rust grammar available in bench build");
    s.parse_at(text, text_version);
    let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
    let overlay_cell = Arc::new(ArcSwap::from_pointee(StaticOverlayQuads::default()));
    let all_matches = matches(viewport_height + scroll + 1);
    let static_overlay_version =
        lattice_host::render_state::static_overlay_state_version(&[], &all_matches, &[]);
    let rs = RenderState {
        active_document: Arc::new(ArcSwap::from_pointee(ActiveDocumentRenderState {
            all_matches: Arc::from(all_matches.into_boxed_slice()),
            ..ActiveDocumentRenderState::default()
        })),
        syntax: Arc::new(SyntaxRenderState {
            syntax_handle: Some(handle.clone()),
            scroll,
            viewport_height,
            end_line_override: None,
            fold_hash,
            text_version,
            inlay_hints: Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice()),
            inlay_version: 0,
            static_overlay_quads: overlay_cell.clone(),
            doc_highlights: Arc::from(
                Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
            ),
            static_overlay_version,
        }),
        ..RenderState::default()
    };
    (ArcSwap::from_pointee(rs), handle, overlay_cell)
}

fn rebuild_rs(
    handle: &Arc<lattice_syntax::SyntaxHandle>,
    overlay_cell: &Arc<ArcSwap<StaticOverlayQuads>>,
    scroll: u32,
    viewport_height: u32,
    fold_hash: u64,
    text_version: u64,
) -> ArcSwap<RenderState> {
    let all_matches = matches(viewport_height + scroll + 1);
    let static_overlay_version =
        lattice_host::render_state::static_overlay_state_version(&[], &all_matches, &[]);
    let rs = RenderState {
        active_document: Arc::new(ArcSwap::from_pointee(ActiveDocumentRenderState {
            all_matches: Arc::from(all_matches.into_boxed_slice()),
            ..ActiveDocumentRenderState::default()
        })),
        syntax: Arc::new(SyntaxRenderState {
            syntax_handle: Some(handle.clone()),
            scroll,
            viewport_height,
            end_line_override: None,
            fold_hash,
            text_version,
            inlay_hints: Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice()),
            inlay_version: 0,
            static_overlay_quads: overlay_cell.clone(),
            doc_highlights: Arc::from(
                Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
            ),
            static_overlay_version,
        }),
        ..RenderState::default()
    };
    ArcSwap::from_pointee(rs)
}

fn cache_hit_bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("overlay_worker_cache_hit");
    for viewport in [24u32, 60, 120] {
        let corpus = rust_corpus(2000);
        let (rs, _h, overlay_cell) = build_rs(&corpus, 0, viewport, 0, 1);
        // Prime once so subsequent calls take the CacheHit path.
        assert_eq!(recompute(&rs, &overlay_cell), WorkerDecision::Recomputed);
        g.bench_with_input(BenchmarkId::from_parameter(viewport), &(), |bencher, _| {
            bencher.iter(|| {
                let d = recompute(black_box(&rs), black_box(&overlay_cell));
                debug_assert_eq!(d, WorkerDecision::CacheHit);
            });
        });
    }
    g.finish();
}

fn recompute_bench(c: &mut Criterion) {
    // Simulates a held-j scroll burst: every iteration the scroll
    // bumps by 1 line, invalidating the cache key so the worker
    // re-buckets the overlay layers for the new range. This is the
    // worst-case 60Hz scroll path.
    let mut g = c.benchmark_group("overlay_worker_recompute_on_scroll");
    for viewport in [24u32, 60, 120] {
        let corpus = rust_corpus(2000);
        let (_rs0, handle, overlay_cell) = build_rs(&corpus, 0, viewport, 0, 1);
        g.bench_with_input(BenchmarkId::from_parameter(viewport), &(), |bencher, _| {
            let mut scroll: u32 = 0;
            bencher.iter(|| {
                scroll = (scroll + 1) % 100;
                let rs = rebuild_rs(&handle, &overlay_cell, scroll, viewport, 0, 1);
                let d = recompute(black_box(&rs), black_box(&overlay_cell));
                debug_assert!(matches!(
                    d,
                    WorkerDecision::Recomputed | WorkerDecision::CacheHit
                ));
            });
        });
    }
    g.finish();
}

fn stale_hold_bench(c: &mut Criterion) {
    // Document `text_version` ratchets ahead of the syntax
    // snapshot's parsed version, so every wake takes the HOLD path.
    // `fold_hash` bumps each iteration to defeat the cache-hit
    // short-circuit; would otherwise see CacheHit on the second call
    // (snapshot pointer + text_version unchanged).
    let mut g = c.benchmark_group("overlay_worker_stale_snapshot_hold");
    for viewport in [24u32, 60, 120] {
        let corpus = rust_corpus(500);
        let (rs_initial, handle, overlay_cell) = build_rs(&corpus, 0, viewport, 0, 1);
        // Prime the cell with computed quads so HOLD has quads to
        // preserve (the realistic state during a held-j edit stream).
        assert_eq!(
            recompute(&rs_initial, &overlay_cell),
            WorkerDecision::Recomputed
        );
        let mut fold_hash: u64 = 0;
        g.bench_with_input(BenchmarkId::from_parameter(viewport), &(), |bencher, _| {
            bencher.iter(|| {
                fold_hash = fold_hash.wrapping_add(1);
                let rs = rebuild_rs(&handle, &overlay_cell, 0, viewport, fold_hash, 2);
                let d = recompute(black_box(&rs), black_box(&overlay_cell));
                debug_assert_eq!(d, WorkerDecision::StaleSnapshotHold);
            });
        });
    }
    g.finish();
}

criterion_group!(worker, cache_hit_bench, recompute_bench, stale_hold_bench);
criterion_main!(worker);
