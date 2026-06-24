//! D.3.f.2 (2026-05-29) / DX.3-C7 (2026-06-24): fold-recompute hot-path bench.
//!
//! Three workloads back the §6.5 / `fold-architecture.md`
//! claims that the registry indirection + hunk-overlay
//! emission stays well inside the one-frame keystroke→glyph ceiling (8.3 ms at 120 Hz) at
//! the v1 expected scales (N hunks ≤ 100 typical, ≤ 1000
//! pathological).
//!
//! - **`overlay_only_at_n_hunks`** — `Editor::recompute_folds`
//!   with foldmethod=Manual (primary returns `vec![]`),
//!   a published `HunkIndex` of N hunks, and the mode-owned
//!   `HunkFoldSource` overlay (registered by
//!   `diff-mode::on_activate` via the `FoldOverlayService`).
//!   Measures the registry dispatch + overlay emission +
//!   carry-over loop cost end-to-end against the production
//!   hot-path entry.
//!
//! - **`hunk_source_compute_pure`** — direct
//!   [`HunkFoldSource::compute_folds`] call over a session
//!   carrying N published hunks. Isolates the per-hunk
//!   allocation + identity-hash cost from the Editor seam
//!   and the carry-over merge. Establishes the source's
//!   inherent floor so changes in the integration cost can
//!   be attributed to either the registry plumbing or the
//!   source itself.
//!
//! - **`fold_identity_hash`** — raw `hunk_fold_identity`
//!   cost. Sanity check that the `DefaultHasher` salt is
//!   cheap enough to amortise per-hunk in the source's
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

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use smallvec::smallvec;

use lattice_core::{BufferId, Fold, FoldSource};
use lattice_diff::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
use lattice_host::diff::fold::{HUNK_FOLD_NAMESPACE, HunkFoldSource, hunk_fold_identity};
use lattice_host::diff::subsystem::DiffSession;
use lattice_host::editor::Editor;

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
        // DX.3-C7: hunk folds are mode-owned. Boot a real editor (wires
        // the fold-overlay + diff-subsystem services), register a session
        // + activate diff-mode so `on_activate` registers the
        // `HunkFoldSource`, then measure steady-state `recompute_folds`
        // (idempotent for a fixed `HunkIndex`). Setup is per-N, outside
        // the measured loop.
        let mut editor = Editor::boot(lattice_core::Document::from_text("x\n"));
        let bid = editor.document_buffer_id;
        let session = editor
            .diff_subsystem
            .register(bid, DiffAlgorithm::Histogram);
        if n > 0 {
            session.publish(make_hunks(n));
        }
        editor
            .diff_subsystem
            .mode_bridge()
            .note_session_opened(bid, &[bid]);
        editor.apply_pending_diff_mode_changes();
        group.bench_with_input(BenchmarkId::new("hunks", n), &(), |b, _| {
            b.iter(|| {
                editor.recompute_folds();
                black_box(&editor.folds);
            });
        });
    }
    group.finish();
}

fn bench_hunk_source_compute_pure(c: &mut Criterion) {
    let mut group = c.benchmark_group("hunk_source_compute_pure");
    for &n in &[0u32, 10, 100, 1_000] {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        if n > 0 {
            session.publish(make_hunks(n));
        }
        let source = HunkFoldSource::new(session, BufferId(1));
        group.bench_with_input(BenchmarkId::new("hunks", n), &(), |b, _| {
            b.iter(|| {
                let out: Vec<Fold> = black_box(&source).compute_folds();
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

// Sanity: the per-buffer hunk-fold id namespace is the constant we
// expect (would fail to compile if the API drifted).
#[allow(dead_code)]
const _ASSERT_OVERLAY_NS: u64 = HUNK_FOLD_NAMESPACE;

criterion_group!(
    benches,
    bench_overlay_only_at_n_hunks,
    bench_hunk_source_compute_pure,
    bench_fold_identity_hash
);
criterion_main!(benches);
