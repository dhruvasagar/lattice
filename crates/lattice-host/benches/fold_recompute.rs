//! D.3.f.2 (2026-05-29): fold-recompute hot-path bench.
//!
//! Three workloads back the §6.5 / `fold-architecture.md`
//! claims that the registry indirection + hunk-overlay
//! emission stays well inside the 8 ms keystroke budget at
//! the v1 expected scales (N hunks ≤ 100 typical, ≤ 1000
//! pathological).
//!
//! - **`overlay_only_at_n_hunks`** — `Editor::recompute_folds`
//!   with foldmethod=Manual (primary returns `vec![]`),
//!   a published `HunkIndex` of N hunks, and the always-on
//!   `HunkFoldProvider` overlay. Measures the registry
//!   dispatch + overlay emission + carry-over loop cost
//!   end-to-end against the production hot-path entry. The
//!   numbers here are the marginal cost of D.3.f.0 + D.3.f.1
//!   over the pre-refactor `recompute_folds` early-return on
//!   Manual.
//!
//! - **`hunk_provider_compute_pure`** — direct
//!   [`HunkFoldProvider::compute`] call with a constructed
//!   [`FoldContext`] carrying N hunks. Isolates the per-hunk
//!   allocation + identity-hash cost from the Editor seam
//!   and the carry-over merge. Establishes the provider's
//!   inherent floor so changes in the integration cost can
//!   be attributed to either the registry plumbing or the
//!   provider itself.
//!
//! - **`fold_identity_hash`** — raw `hunk_fold_identity`
//!   cost. Sanity check that the `DefaultHasher` salt is
//!   cheap enough to amortise per-hunk in the provider's
//!   per-emit loop.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench fold_recompute
//!
//! Backs paramount goal #1 (sub-frame keystroke→glyph). CI
//! gate enforcement deferred until either the bench falls
//! into a CI runner with stable wall-clock guarantees or a
//! visual regression motivates an absolute ceiling. The
//! recorded baselines live in
//! `docs/dev/operations/benchmarks.md`.

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use smallvec::smallvec;

use lattice_core::{BufferId, Fold, ProviderId};
use lattice_diff::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
use lattice_host::diff::fold::{hunk_fold_identity, HunkFoldProvider};
use lattice_host::editor::Editor;
use lattice_host::fold_provider::{FoldContext, FoldProvider};

/// Build a synthetic `HunkIndex` with `n` Add hunks of 4
/// current-side lines each, spaced 16 lines apart. Matches
/// the "many small hunks scattered through a file" shape
/// that drives the real keystroke budget — the pathological
/// case isn't huge hunks (those are uncommon) but lots of
/// them.
fn make_hunks(n: u32) -> Arc<HunkIndex> {
    let mut hunks = Vec::with_capacity(n as usize);
    for i in 0..n {
        let start = i * 16;
        let end = start + 4;
        hunks.push(Hunk {
            kind: HunkKind::Add,
            ranges: smallvec![LineRange::new(start, start), LineRange::new(start, end)],
        });
    }
    Arc::new(HunkIndex {
        hunks,
        algorithm: DiffAlgorithm::Histogram,
        revision: 1,
    })
}

fn bench_overlay_only_at_n_hunks(c: &mut Criterion) {
    let mut group = c.benchmark_group("overlay_only_at_n_hunks");
    for &n in &[0u32, 10, 100, 1_000] {
        // Build a fresh editor + session per N. Re-use across
        // iterations — `recompute_folds` is idempotent for a
        // fixed `HunkIndex` so steady-state cost is the
        // measurement.
        let mut editor = Editor::default();
        let bid = editor.document_buffer_id;
        let session = editor
            .diff_subsystem
            .register(bid, DiffAlgorithm::Histogram);
        if n > 0 {
            session.publish(make_hunks(n));
        }
        group.bench_with_input(BenchmarkId::new("hunks", n), &(), |b, _| {
            b.iter(|| {
                editor.recompute_folds();
                black_box(&editor.folds);
            });
        });
    }
    group.finish();
}

fn bench_hunk_provider_compute_pure(c: &mut Criterion) {
    use lattice_core::Buffer;
    let buffer = Buffer::empty();
    let mut group = c.benchmark_group("hunk_provider_compute_pure");
    for &n in &[0u32, 10, 100, 1_000] {
        let hunks = make_hunks(n);
        let ctx = FoldContext {
            buffer: &buffer,
            buffer_id: BufferId(1),
            path: None,
            syntax: None,
            lsp_folds: None,
            diff_hunks: Some(&hunks),
        };
        let provider = HunkFoldProvider;
        group.bench_with_input(BenchmarkId::new("hunks", n), &(), |b, _| {
            b.iter(|| {
                let out: Vec<Fold> = provider.compute(black_box(&ctx));
                black_box(out);
            });
        });
    }
    group.finish();
}

fn bench_fold_identity_hash(c: &mut Criterion) {
    c.bench_function("fold_identity_hash", |b| {
        b.iter(|| black_box(hunk_fold_identity(black_box(42), black_box(58))));
    });
}

// Sanity: the `HunkFoldProvider`'s id is the constant we
// expect (would fail to compile if the API drifted).
#[allow(dead_code)]
const _ASSERT_OVERLAY_ID: ProviderId = lattice_host::diff::fold::HUNK_FOLD_PROVIDER_ID;

criterion_group!(
    benches,
    bench_overlay_only_at_n_hunks,
    bench_hunk_provider_compute_pure,
    bench_fold_identity_hash
);
criterion_main!(benches);
