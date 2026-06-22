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
//! Backs paramount goal #1 (imperceptible keystroke→glyph, within
//! the one-frame ceiling -- 8.3 ms at 120Hz): `incremental_build` is
//! the per-keystroke cost that has to fit comfortably inside the
//! ceiling alongside the paint pass.

use std::sync::Arc;

use arc_swap::ArcSwap;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_cells::{CellMatrix, EditDelta, MatrixVersion, VirtualRowMatrix};
use lattice_core::Document;
use lattice_host::cells_worker::WhitespaceConfig;
use lattice_host::cells_worker::{recompute, sync_rebuild_pane_on_edit};
use lattice_host::display_matrix::DisplayMatrix;
use lattice_host::render_state::{CellsRenderState, InlayHintRow, PaneCellsInputs, RenderState};
use lattice_host::ui::theme::{BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry};
use lattice_runtime::DocumentSnapshot;

/// A fresh empty display-matrix cell (boot / cold-start state).
fn empty_display() -> Arc<ArcSwap<DisplayMatrix>> {
    Arc::new(ArcSwap::from_pointee(DisplayMatrix::empty()))
}

/// A display-matrix cell pre-seeded with `dm` — the prior-tick baseline
/// the incremental + sync edit paths reuse from.
fn seeded_display(dm: &DisplayMatrix) -> Arc<ArcSwap<DisplayMatrix>> {
    Arc::new(ArcSwap::from_pointee(dm.clone()))
}

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
    syntax: Option<Arc<lattice_syntax::SyntaxHandle>>,
    display_cell: Arc<ArcSwap<DisplayMatrix>>,
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
        // B2.3: the canonical `DisplayMatrix` cell — `recompute` reads it
        // for the prior-tick baseline (incremental reuse) and writes the
        // rebuilt matrix; `sync_rebuild_pane_on_edit` reads + writes it on
        // the actor thread. Sharing the caller's cell lets a bench seed a
        // baseline so the incremental / sync paths are actually exercised.
        display_matrix: display_cell.clone(),
        virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty())),
        version,
        snapshot: Some(snapshot.clone()),
        syntax_handle: syntax.clone(),
        // K.4.7 added per-excerpt syntax for multibuffer panes; a
        // single-file bench pane has no excerpts, so an empty slice.
        excerpt_syntax: Arc::from(Vec::new()),
        inlay_hints: inlay_hints.clone(),
        folds: folds.clone(),
        viewport_height: VIEWPORT_HEIGHT,
        // H.3: window anchor. The existing benches build at the top
        // (scroll 0); the windowed bench relies on this too — at
        // scroll 0 a large doc windows to its first ~chunk regardless
        // of total size, which is exactly the O(viewport) measurement.
        scroll: 0,
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
    let display_pane_matrices = {
        let mut m = std::collections::HashMap::new();
        m.insert(pane_entry.pane_id, pane_entry.display_matrix.clone());
        Arc::new(m)
    };
    let cells = CellsRenderState {
        matrix: matrix_cell,
        version,
        snapshot: Some(snapshot),
        syntax_handle: syntax,
        inlay_hints,
        folds,
        viewport_height: VIEWPORT_HEIGHT,
        foldenable: false,
        last_edit,
        // T.6.t: the host `Theme` struct is gone; the cell builder reads
        // styles through the resolved table + builtin ids.
        resolved_theme: {
            let reg = InMemoryThemeRegistry::with_defaults();
            reg.resolved()
        },
        theme_ids: {
            let reg = InMemoryThemeRegistry::with_defaults();
            BuiltinElementIds::capture(&reg)
        },
        whitespace: WhitespaceConfig::default(),
        panes: Arc::from(vec![pane_entry].into_boxed_slice()),
        pane_matrices,
        display_matrix: display_cell,
        display_pane_matrices,
    };
    let rs = RenderState {
        cells: Arc::new(ArcSwap::from_pointee(cells)),
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
                    let rs = rs_for(snap.clone(), *ver, None, matrix_cell, None, empty_display());
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

        // Build the prior matrices once. The bench loop will clone their
        // Arcs as the starting point for each iter so the prefix-reuse path
        // is exercised. B2.3: the canonical baseline is the DISPLAY matrix
        // (`recompute` reuses it incrementally); the cell baseline is the
        // projection. Seed BOTH per iter, else `recompute` finds an empty
        // display cell and silently does a full build, not incremental.
        let initial_matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let initial_display_cell = empty_display();
        let rs0 = rs_for(
            snapshot.clone(),
            v1,
            None,
            initial_matrix_cell.clone(),
            None,
            initial_display_cell.clone(),
        );
        recompute(&rs0);
        let baseline_matrix = initial_matrix_cell.load_full();
        let baseline_display = initial_display_cell.load_full();

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
            &(snapshot, baseline_matrix, baseline_display, v2, edit),
            |b, (snap, baseline, baseline_dm, ver, ed)| {
                b.iter(|| {
                    // Each iter starts from the same baseline
                    // matrices and the same edit delta.
                    let matrix_cell: Arc<ArcSwap<CellMatrix>> =
                        Arc::new(ArcSwap::from_pointee((**baseline).clone()));
                    let rs = rs_for(
                        snap.clone(),
                        *ver,
                        Some(*ed),
                        matrix_cell,
                        None,
                        seeded_display(baseline_dm),
                    );
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

        // Pre-populate matrix_cell + display_cell at the same version
        // rs.cells.version carries — recompute should see the version match
        // (display current) and the projection current, returning CacheHit
        // without building. The measured `rs` must SHARE both cells so the
        // populated baseline is visible.
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let display_cell = empty_display();
        let rs_full = rs_for(
            snapshot.clone(),
            version,
            None,
            matrix_cell.clone(),
            None,
            display_cell.clone(),
        );
        recompute(&rs_full);

        let rs = rs_for(snapshot, version, None, matrix_cell.clone(), None, display_cell);

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

/// H.1 (2026-06-04): incremental rebuild WITH a live syntax handle, so the
/// per-keystroke **highlight** cost is in the measurement (the other benches
/// pass `syntax: None` and measure cell-build only). After H.1 the rebuild
/// highlights only the edited line range, so this should stay roughly flat as
/// `line_count` grows; a regression to whole-file highlight shows up here as
/// cost scaling with file size. Recorded in `docs/dev/operations/benchmarks.md`.
fn bench_incremental_build_highlighted(c: &mut Criterion) {
    let mut group = c.benchmark_group("cells_worker_incremental_highlighted");
    for &line_count in &[100usize, 1_000, 5_000] {
        let doc = synthetic_rust_doc(line_count);
        let text = doc.text();
        let snapshot = Arc::new(DocumentSnapshot::__bench_from_document(&doc));
        let v1 = MatrixVersion {
            text: 1,
            ..MatrixVersion::ZERO
        };
        let v2 = MatrixVersion {
            text: 2,
            ..MatrixVersion::ZERO
        };

        // Rust syntax parsed at the doc version so the scoped highlight has
        // fresh spans. `seeded` retains the snapshot without a runtime (the
        // reparse worker is dropped) — fine for a one-shot bench rebuild.
        let mut syn = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .unwrap();
        syn.parse_at(&text, 2);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(syn));

        let initial_matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let initial_display_cell = empty_display();
        let rs0 = rs_for(
            snapshot.clone(),
            v1,
            None,
            initial_matrix_cell.clone(),
            Some(handle.clone()),
            initial_display_cell.clone(),
        );
        recompute(&rs0);
        let baseline_matrix = initial_matrix_cell.load_full();
        let baseline_display = initial_display_cell.load_full();

        // In-place edit of the middle line (removed == added == 1).
        let edit = EditDelta {
            start_line: (line_count / 2) as u32,
            lines_removed: 1,
            lines_added: 1,
        };

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &(snapshot, baseline_matrix, baseline_display, v2, edit, handle),
            |b, (snap, baseline, baseline_dm, ver, ed, handle)| {
                b.iter(|| {
                    let matrix_cell: Arc<ArcSwap<CellMatrix>> =
                        Arc::new(ArcSwap::from_pointee((**baseline).clone()));
                    let rs = rs_for(
                        snap.clone(),
                        *ver,
                        Some(*ed),
                        matrix_cell,
                        Some(handle.clone()),
                        seeded_display(baseline_dm),
                    );
                    let decision = recompute(&rs);
                    black_box(decision);
                });
            },
        );
    }
    group.finish();
}

/// H.3 (2026-06-04): the headline large-file win. A full (cold) build
/// at a FIXED viewport over docs from 5k to 100k lines, WITH a live
/// syntax handle so highlight + cell materialisation are both measured.
///
/// Above `WINDOW_CAP_LINES` the chunked matrix is windowed to the
/// viewport (`build_matrix` builds + highlights only `[scroll−overscan,
/// scroll+viewport+overscan)`), so build latency must stay ~flat as
/// `line_count` grows — O(viewport), not O(file). A regression to
/// whole-file builds shows here as cost scaling with `line_count`.
///
/// Clone-free harness: a fresh `matrix_cell` per iter forces
/// `build_matrix` (no incremental reuse), and `rs_for` only Arc-clones
/// the shared snapshot + syntax handle (O(1)) — there is NO per-iter
/// O(file) baseline-matrix clone (the pitfall the H.1 incremental
/// benches carry). The synthetic doc + tree-sitter parse happen once
/// per size, outside the timed loop. Recorded in
/// `docs/dev/operations/benchmarks.md`.
fn bench_windowed_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("cells_worker_windowed_build");
    for &line_count in &[5_000usize, 20_000, 50_000, 100_000] {
        let doc = synthetic_rust_doc(line_count);
        let text = doc.text();
        let snap_version = doc.text_version();
        let snapshot = Arc::new(DocumentSnapshot::__bench_from_document(&doc));
        let version = MatrixVersion {
            text: 1,
            ..MatrixVersion::ZERO
        };

        // Parse the whole doc once at the snapshot's version so the
        // build's scoped `highlight_lines` has fresh (non-stale) spans.
        let mut syn = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .unwrap();
        syn.parse_at(&text, snap_version);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(syn));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &(snapshot, version, handle),
            |b, (snap, ver, handle)| {
                b.iter(|| {
                    // Fresh cell so the version check misses → full
                    // (windowed) build_matrix runs.
                    let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
                    let rs = rs_for(
                        snap.clone(),
                        *ver,
                        None,
                        matrix_cell,
                        Some(handle.clone()),
                        empty_display(),
                    );
                    black_box(recompute(&rs));
                });
            },
        );
    }
    group.finish();
}

/// B2.3 (2026-06-04): the synchronous edit-path cost the ACTOR pays in the
/// publish tail (`sync_rebuild_pane_on_edit`) on every keystroke, BEFORE it
/// replies to the UI thread — so this latency is directly on the
/// keystroke→glyph ceiling (paramount goal #1: ≤ 8.3 ms at 120Hz).
///
/// The sync path does ONLY the windowed incremental `DisplayMatrix` rebuild
/// with highlight forced off: prefix/suffix `DisplayLine` (whole-doc) or
/// whole-`DisplayChunk` (chunked) `Arc`-reuse + the edited line's text
/// rebuild. No `highlight_lines`, no reparse, no cell projection (that stays
/// on the async worker). It must stay well under ~200µs even on a 100k-line
/// file — and roughly FLAT across `line_count` in chunked mode, since the
/// rebuild touches O(window), not O(file). A regression to O(file) shows up
/// here as cost scaling with size. Recorded in
/// `docs/dev/operations/benchmarks.md`.
fn bench_display_edit_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("display_edit_path");
    for &line_count in &[100usize, 5_000, 100_000] {
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

        // Baseline display matrix once (the prior published tick). No syntax
        // handle: the sync path forces highlight off regardless, and Arc
        // reuse cost is colour-independent.
        let initial_display_cell = empty_display();
        let initial_matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs0 = rs_for(
            snapshot.clone(),
            v1,
            None,
            initial_matrix_cell,
            None,
            initial_display_cell.clone(),
        );
        recompute(&rs0);
        let baseline_display = initial_display_cell.load_full();

        // In-place edit of the middle line (removed == added == 1) so prefix
        // reuse + suffix shift both have work to do.
        let edit = EditDelta {
            start_line: (line_count / 2) as u32,
            lines_removed: 1,
            lines_added: 1,
        };

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{line_count}_lines")),
            &(snapshot, baseline_display, v2, edit),
            |b, (snap, baseline_dm, ver, ed)| {
                b.iter(|| {
                    // Fresh seeded display cell per iter (same baseline every
                    // time); a throwaway matrix cell to satisfy `rs_for`.
                    let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
                    let rs = rs_for(
                        snap.clone(),
                        *ver,
                        Some(*ed),
                        matrix_cell,
                        None,
                        seeded_display(baseline_dm),
                    );
                    let loaded = rs.load_full();
                    let cells_snap = loaded.cells.load();
                    let pane = &cells_snap.panes[0];
                    // T.6.t: the host `Theme` field is gone; build a
                    // `CellTheme` from the resolved table + builtin ids
                    // (mirrors `dispatch.rs`'s production `next_ct`).
                    let ct = lattice_host::cells_worker::CellTheme {
                        resolved: &cells_snap.resolved_theme,
                        ids: &cells_snap.theme_ids,
                    };
                    let did = sync_rebuild_pane_on_edit(pane, ct, &cells_snap.whitespace);
                    black_box(did);
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
    bench_incremental_build_highlighted,
    bench_windowed_build,
    bench_cache_hit,
    bench_display_edit_path
);
criterion_main!(benches);
