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

use lattice_cells::{CellMatrix, EditDelta, MatrixVersion, VirtualRowMatrix};
use lattice_core::Document;
use lattice_host::cells_worker::WhitespaceConfig;
use lattice_host::cells_worker::recompute;
use lattice_host::render_state::{CellsRenderState, InlayHintRow, PaneCellsInputs, RenderState};
use lattice_host::ui::theme::Theme;
use lattice_runtime::DocumentSnapshot;

/// Fixed viewport height. Sized to a typical editing window;
/// chunked mode kicks in for `line_count > 4 * 60 = 240`.
const VIEWPORT_HEIGHT: u32 = 60;

/// Generates `line_count` synthetic Rust-ish lines (~80 chars
/// each) so the cell builder walks realistic line lengths
/// rather than empty buffers.
fn synthetic_rust_doc(line_count: usize) -> Document {
    let body: String = (0..line_count)
        .map(|i| format!("fn handler_{i:04}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"))
        .collect();
    Document::from_text(&body)
}

/// Build a [`RenderState`] whose `cells` substate carries a
/// single-Document-pane entry pointing at `snapshot` with the
/// requested `version` and `last_edit`. The pane's matrix cell
/// is `matrix_cell` so the bench can observe / pre-populate the
/// worker's write target.
///
/// D.4.d.1.b (2026-05-29): pre-d.1.b this helper populated only
/// the top-level `cells.snapshot` / `cells.matrix`; the worker
/// now reads each pane's inputs, so the bench publishes through
/// `cells.panes`.
fn rs_for(
    snapshot: Arc<DocumentSnapshot>,
    version: MatrixVersion,
    last_edit: Option<EditDelta>,
    matrix_cell: Arc<ArcSwap<CellMatrix>>,
) -> ArcSwap<RenderState> {
    use lattice_core::BufferId;
    use lattice_core::ui::pane::PaneId;
    let inlay_hints: Arc<[InlayHintRow]> = Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice());
    let folds: Arc<[lattice_core::Fold]> =
        Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice());
    let pane_entry = PaneCellsInputs {
        pane_id: PaneId::default(),
        buffer_id: BufferId::default(),
        matrix: matrix_cell.clone(),
        virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty())),
        version,
        snapshot: Some(snapshot.clone()),
        syntax_handle: None,
        inlay_hints: inlay_hints.clone(),
        folds: folds.clone(),
        viewport_height: VIEWPORT_HEIGHT,
        viewport_width: 0,
        wrap: false,
        foldenable: false,
        last_edit,
    };
    let pane_matrices = {
        let mut m = std::collections::HashMap::new();
        m.insert(pane_entry.pane_id, pane_entry.matrix.clone());
        Arc::new(m)
    };
    let cells = CellsRenderState {
        matrix: matrix_cell,
        version,
        snapshot: Some(snapshot),
        syntax_handle: None,
        inlay_hints,
        folds,
        viewport_height: VIEWPORT_HEIGHT,
        foldenable: false,
        last_edit,
        theme: Theme::default(),
        whitespace: WhitespaceConfig::default(),
        panes: Arc::from(vec![pane_entry].into_boxed_slice()),
        pane_matrices,
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
            ..MatrixVersion::ZERO
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
                    let rs = rs_for(snap.clone(), *ver, None, matrix_cell);
                    let decision = recompute(&rs);
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
        let rs0 = rs_for(snapshot.clone(), v1, None, initial_matrix_cell.clone());
        recompute(&rs0);
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
                    let rs = rs_for(snap.clone(), *ver, Some(*ed), matrix_cell);
                    let decision = recompute(&rs);
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
        let rs_full = rs_for(snapshot.clone(), version, None, matrix_cell.clone());
        recompute(&rs_full);

        let rs = rs_for(snapshot, version, None, matrix_cell.clone());

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &rs,
            |b, rs| {
                b.iter(|| {
                    let decision = recompute(rs);
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
