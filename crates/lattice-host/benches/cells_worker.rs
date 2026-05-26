//! S5 (2026-05-27): cells_worker hot-path bench.
//!
//! Times `lattice_host::cells_worker::recompute` — the
//! entrypoint the worker thread invokes on every cells-wake —
//! across three workloads:
//!
//! - `full_build` — fresh `matrix_cell` (empty matrix at
//!   `MatrixVersion::ZERO`); the version differs from the
//!   `CellsRenderState`'s version so `recompute` falls through
//!   to `build_matrix`. Measures the cost of building the
//!   entire viewport's worth of cells from scratch. This is
//!   the cold-start path (boot frame, buffer-switch).
//!
//! - `incremental_build` — published matrix at version `v=1`;
//!   `CellsRenderState` carries version `v=2` plus a
//!   single-line `EditDelta`. `recompute` invokes
//!   `try_incremental_build` which reuses prefix chunks
//!   (Arc-clone), rebuilds the edit zone, and shifts the
//!   suffix. Measures the per-keystroke cost the typing path
//!   pays.
//!
//! - `cache_hit` — published matrix and `CellsRenderState`
//!   carry the same `MatrixVersion`. `recompute` returns
//!   `WorkerDecision::CacheHit` after a single comparison.
//!   Floor cost of the entry — no work should happen here.
//!
//! Each workload runs at three line counts (`100`, `1_000`,
//! `5_000`) so both whole-doc mode (small files) and
//! chunked-mode (big files) are exercised. The threshold is
//! `4 × viewport_height = 240` lines for the default 60-line
//! viewport — so `100` is whole-doc, `1_000` and `5_000` are
//! chunked.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench cells_worker
//!
//! Backs paramount goal #1 (sub-8ms keystroke→glyph at 120Hz):
//! `incremental_build` is the per-keystroke cost that has to
//! fit comfortably inside the budget alongside the paint pass.

use std::sync::Arc;

use arc_swap::ArcSwap;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_cells::{CellMatrix, EditDelta, MatrixVersion};
use lattice_host::cells_worker::recompute;
use lattice_host::render_state::{CellsRenderState, InlayHintRow, RenderState};
use lattice_host::ui::theme::Theme;
use lattice_core::Document;
use lattice_runtime::DocumentSnapshot;

/// Fixed viewport height. Sized to a typical editing window;
/// chunked mode kicks in for `line_count > 4 * 60 = 240`.
const VIEWPORT_HEIGHT: u32 = 60;

/// Generates `line_count` synthetic Rust-ish lines (~80 chars
/// each) so the cell builder walks realistic line lengths
/// rather than empty buffers.
fn synthetic_rust_doc(line_count: usize) -> Document {
    let body: String = (0..line_count)
        .map(|i| {
            format!(
                "fn handler_{i:04}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"
            )
        })
        .collect();
    Document::from_text(&body)
}

/// Build a [`RenderState`] whose `cells` substate points at
/// `snapshot` with the requested `version`, plus an optional
/// `last_edit` for the incremental path. The returned
/// `ArcSwap` is what `recompute` reads from.
fn rs_for(
    snapshot: Arc<DocumentSnapshot>,
    version: MatrixVersion,
    last_edit: Option<EditDelta>,
) -> ArcSwap<RenderState> {
    let cells = CellsRenderState {
        matrix: Arc::default(),
        version,
        snapshot: Some(snapshot),
        syntax_handle: None,
        inlay_hints: Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice()),
        folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
        viewport_height: VIEWPORT_HEIGHT,
        foldenable: false,
        last_edit,
        theme: Theme::default(),
    };
    let rs = RenderState {
        cells: Arc::new(cells),
        ..RenderState::default()
    };
    ArcSwap::from_pointee(rs)
}

fn bench_full_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("cells_worker_full_build");
    for &line_count in &[100usize, 1_000, 5_000] {
        let doc = synthetic_rust_doc(line_count);
        let snapshot = Arc::new(DocumentSnapshot::__bench_from_document(&doc));
        // version=1 so the empty published matrix at version=0
        // is stale → forces a full rebuild.
        let version = MatrixVersion {
            text: 1,
            syntax: 0,
            inlay_hints: 0,
            folds: 0,
            theme: 0,
        };
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &(snapshot, version),
            |b, (snap, ver)| {
                b.iter(|| {
                    // Fresh matrix_cell per iter so the version
                    // check always fails the cache and forces
                    // build_matrix.
                    let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
                    let rs = rs_for(snap.clone(), *ver, None);
                    let decision = recompute(&rs, &matrix_cell);
                    black_box(decision);
                });
            },
        );
    }
    group.finish();
}

fn bench_incremental_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("cells_worker_incremental_build");
    for &line_count in &[100usize, 1_000, 5_000] {
        let doc = synthetic_rust_doc(line_count);
        let snapshot = Arc::new(DocumentSnapshot::__bench_from_document(&doc));
        let v1 = MatrixVersion {
            text: 1,
            ..MatrixVersion::ZERO
        };
        let v2 = MatrixVersion {
            text: 2,
            ..MatrixVersion::ZERO
        };

        // Build the prior matrix once. The bench loop will
        // clone its Arc as the starting point for each iter so
        // the prefix-reuse path is exercised.
        let initial_matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs0 = rs_for(snapshot.clone(), v1, None);
        recompute(&rs0, &initial_matrix_cell);
        let baseline_matrix = initial_matrix_cell.load_full();

        // Single-line edit on line line_count/2 — touches the
        // middle of the document so prefix reuse + suffix
        // shift both have work to do.
        let edit = EditDelta {
            start_line: (line_count / 2) as u32,
            lines_removed: 0,
            lines_added: 0,
        };

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &(snapshot, baseline_matrix, v2, edit),
            |b, (snap, baseline, ver, ed)| {
                b.iter(|| {
                    // Each iter starts from the same baseline
                    // matrix and the same edit delta.
                    let matrix_cell: Arc<ArcSwap<CellMatrix>> =
                        Arc::new(ArcSwap::from_pointee((**baseline).clone()));
                    let rs = rs_for(snap.clone(), *ver, Some(*ed));
                    let decision = recompute(&rs, &matrix_cell);
                    black_box(decision);
                });
            },
        );
    }
    group.finish();
}

fn bench_cache_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("cells_worker_cache_hit");
    for &line_count in &[100usize, 1_000, 5_000] {
        let doc = synthetic_rust_doc(line_count);
        let snapshot = Arc::new(DocumentSnapshot::__bench_from_document(&doc));
        let version = MatrixVersion {
            text: 1,
            ..MatrixVersion::ZERO
        };

        // Pre-populate matrix_cell at the same version
        // rs.cells.version carries — recompute should see the
        // version match and return CacheHit without touching
        // build_matrix or try_incremental_build.
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs_full = rs_for(snapshot.clone(), version, None);
        recompute(&rs_full, &matrix_cell);

        let rs = rs_for(snapshot, version, None);

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &(rs, matrix_cell),
            |b, (rs, cell)| {
                b.iter(|| {
                    let decision = recompute(rs, cell);
                    black_box(decision);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_full_build,
    bench_incremental_build,
    bench_cache_hit
);
criterion_main!(benches);
