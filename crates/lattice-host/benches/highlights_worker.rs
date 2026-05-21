//! Phase 5.8.AF.6 / Slice X2.9: highlights worker bench.
//!
//! Simulates a 60Hz scroll burst against the worker's
//! `recompute` decision function and measures three distinct
//! decision paths:
//!
//! - `cache_hit` -- steady-state cursor blink with no input
//!   changes. Should be ~ns: a key compare + Arc swap is the
//!   entire body.
//! - `recompute` -- cache miss, current snapshot. The full
//!   `highlight_lines` walk runs. This is the worker's cost
//!   ceiling per scroll/edit tick.
//! - `stale_snapshot_hold` -- cache miss against a snapshot
//!   that hasn't caught up to the document's `text_version`
//!   yet. Holds prior spans + bumps the key; sub-µs.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench highlights_worker

use std::sync::Arc;

use arc_swap::ArcSwap;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_host::highlights_worker::{WorkerDecision, recompute};
use lattice_host::render_state::{RenderState, SyntaxRenderState, VisibleSpans};

fn rust_corpus(n_fns: usize) -> String {
    let mut s = String::with_capacity(n_fns * 80);
    for i in 0..n_fns {
        s.push_str(&format!(
            "fn handler_{i:04}(input: &str) -> Result<Output, Error> {{\n    let mut acc = 0;\n    if input.is_empty() {{\n        return Ok(acc);\n    }}\n    Ok(acc + 1)\n}}\n\n"
        ));
    }
    s
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
    Arc<ArcSwap<VisibleSpans>>,
) {
    let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .expect("rust grammar available in bench build");
    s.parse_at(text, text_version);
    let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
    let cell = Arc::new(ArcSwap::from_pointee(VisibleSpans::default()));
    let rs = RenderState {
        syntax: Arc::new(SyntaxRenderState {
            syntax_handle: Some(handle.clone()),
            scroll,
            viewport_height,
            end_line_override: None,
            fold_hash,
            text_version,
            visible_spans: cell.clone(),
            pane_highlights: Arc::new(std::collections::HashMap::new()),
        }),
        ..RenderState::default()
    };
    (ArcSwap::from_pointee(rs), handle, cell)
}

fn rebuild_rs(
    handle: &Arc<lattice_syntax::SyntaxHandle>,
    cell: &Arc<ArcSwap<VisibleSpans>>,
    scroll: u32,
    viewport_height: u32,
    fold_hash: u64,
    text_version: u64,
) -> ArcSwap<RenderState> {
    let rs = RenderState {
        syntax: Arc::new(SyntaxRenderState {
            syntax_handle: Some(handle.clone()),
            scroll,
            viewport_height,
            end_line_override: None,
            fold_hash,
            text_version,
            visible_spans: cell.clone(),
            pane_highlights: Arc::new(std::collections::HashMap::new()),
        }),
        ..RenderState::default()
    };
    ArcSwap::from_pointee(rs)
}

fn cache_hit_bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("worker_cache_hit");
    for viewport in [24u32, 60, 120] {
        let corpus = rust_corpus(2000);
        let (rs, _h, cell) = build_rs(&corpus, 0, viewport, 0, 1);
        // Prime once so subsequent calls take the CacheHit path.
        assert_eq!(recompute(&rs, &cell), WorkerDecision::Recomputed);
        g.bench_with_input(
            BenchmarkId::from_parameter(viewport),
            &(),
            |bencher, _| {
                bencher.iter(|| {
                    let d = recompute(black_box(&rs), black_box(&cell));
                    debug_assert_eq!(d, WorkerDecision::CacheHit);
                });
            },
        );
    }
    g.finish();
}

fn recompute_bench(c: &mut Criterion) {
    // Simulates a held-j scroll burst: every iteration the
    // scroll bumps by 1 line, invalidating the cache key so the
    // worker walks `highlight_lines` for the new range. This is
    // the worst-case 60Hz scroll path.
    let mut g = c.benchmark_group("worker_recompute_on_scroll");
    for viewport in [24u32, 60, 120] {
        let corpus = rust_corpus(2000);
        let (_rs0, handle, cell) = build_rs(&corpus, 0, viewport, 0, 1);
        g.bench_with_input(
            BenchmarkId::from_parameter(viewport),
            &(),
            |bencher, _| {
                let mut scroll: u32 = 0;
                bencher.iter(|| {
                    scroll = (scroll + 1) % 100;
                    let rs = rebuild_rs(&handle, &cell, scroll, viewport, 0, 1);
                    let d = recompute(black_box(&rs), black_box(&cell));
                    debug_assert!(matches!(
                        d,
                        WorkerDecision::Recomputed | WorkerDecision::CacheHit
                    ));
                });
            },
        );
    }
    g.finish();
}

fn stale_hold_bench(c: &mut Criterion) {
    // Document `text_version` ratchets ahead of the syntax
    // snapshot's parsed version, so every wake takes the HOLD
    // path. `fold_hash` bumps each iteration to defeat the
    // cache-hit short-circuit; would otherwise see CacheHit on
    // the second call (snapshot pointer + text_version
    // unchanged).
    let mut g = c.benchmark_group("worker_stale_snapshot_hold");
    for viewport in [24u32, 60, 120] {
        let corpus = rust_corpus(500);
        let (rs_initial, handle, cell) = build_rs(&corpus, 0, viewport, 0, 1);
        // Prime the cell with computed spans so HOLD has spans
        // to preserve (the realistic state during a held-j
        // edit stream).
        assert_eq!(recompute(&rs_initial, &cell), WorkerDecision::Recomputed);
        let mut fold_hash: u64 = 0;
        g.bench_with_input(
            BenchmarkId::from_parameter(viewport),
            &(),
            |bencher, _| {
                bencher.iter(|| {
                    fold_hash = fold_hash.wrapping_add(1);
                    let rs = rebuild_rs(&handle, &cell, 0, viewport, fold_hash, 2);
                    let d = recompute(black_box(&rs), black_box(&cell));
                    debug_assert_eq!(d, WorkerDecision::StaleSnapshotHold);
                });
            },
        );
    }
    g.finish();
}

criterion_group!(worker, cache_hit_bench, recompute_bench, stale_hold_bench);
criterion_main!(worker);
