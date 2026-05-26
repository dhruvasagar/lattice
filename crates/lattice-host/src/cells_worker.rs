//! Background cell-builder worker — replaces per-frame `shape_line`
//! with an off-thread cell matrix build.
//!
//! S2.2 (2026-05-26).
//!
//! ## Why this exists
//!
//! Per paramount goal #1 in `CLAUDE.md`:
//!
//! > **Performance.** UI thread does no I/O, no parsing, no shaping.
//!
//! The cell-grid renderer (see
//! `docs/dev/architecture/cell-grid-renderer.md`) replaces the per-
//! frame `shape_line` path for code-class buffers. The matrix
//! producer must run off the UI thread; this module owns it.
//!
//! ## S2.2 scope — minimal
//!
//! S2.2 lands the worker shell with the **simplest possible build**:
//!
//! - one whole-doc [`lattice_cells::CellMatrix`] per published
//!   document,
//! - rows materialised from `snapshot.buffer.line(i)` line-by-line,
//! - cells carry the raw codepoint only (no syntax fg, no bg, no
//!   flags),
//! - no inlay splicing, no fold elision,
//! - no chunking (S2.4 lands that).
//!
//! S2.3 will fold in syntax colour + inlays + folds; S2.4 will
//! switch to chunked mode once the input is above `4 × viewport_height`
//! lines.
//!
//! ## Design (mirrors `highlights_worker`)
//!
//! - Dispatch's `publish_render_state` populates
//!   [`crate::render_state::CellsRenderState`] inputs (`snapshot`,
//!   `version`, …) and fires [`crate::editor::CellsWake`]'s
//!   `Notify`.
//! - The worker `notified().await`s the wake signal. `Notify` is
//!   permit-style: a burst of publishes wakes the worker exactly
//!   once, after which the worker re-reads the *latest* snapshot.
//! - On wake the worker reads `render_state.load_full().cells`,
//!   compares its `version` against the currently-published
//!   [`lattice_cells::CellMatrix::version`], and short-circuits on
//!   cache-hit. On miss it builds a fresh matrix and stores it via
//!   the shared `cells_matrix_cell: Arc<ArcSwap<CellMatrix>>`.
//!
//! ## Renderer contract (S2.2 — not consumed yet)
//!
//! Renderers will read with:
//!
//! ```text
//! let rs = editor.render_state.load_full();
//! let matrix = rs.cells.matrix.load();
//! // matrix.slice(scroll, viewport_height) → CellSlice over &CellRow
//! ```
//!
//! S3 (TUI) and S4 (GPU) are the cutover slices that begin
//! consuming the matrix. S2.2 keeps the producer in place so the
//! consumer slices land against a populated cell, not a stub.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{debug, info};

use crate::editor::CellsWake;
use crate::render_state::RenderState;
use lattice_cells::{Cell, CellChunk, CellMatrix, CellRow, MatrixVersion};

/// Recompute decision the worker takes on a wake. Visible for
/// testing; the production loop calls [`recompute`] directly.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkerDecision {
    /// `snapshot` is `None` — no active document. Worker clears the
    /// published matrix (so a previous document's cells don't
    /// linger after a close).
    Clear,
    /// Current inputs' `version` matches the already-published
    /// matrix's `version`. Worker does nothing.
    CacheHit,
    /// `version` differs from the published matrix; worker built a
    /// fresh `CellMatrix` from the snapshot and stored it.
    Recomputed,
}

/// Worker entry point spawned at boot. Loops forever, awaiting
/// the wake `Notify`. Each wake re-reads the latest
/// `RenderState.cells` inputs and calls [`recompute`].
///
/// Spawn from `editor_boot` once `Editor` is constructed. Pass
/// clones of `Editor::render_state` and `Editor::cells_matrix_cell`
/// plus the wake `Notify` and the `paint_request` notifier.
pub async fn run(
    render_state: Arc<ArcSwap<RenderState>>,
    wake: CellsWake,
    matrix_cell: Arc<ArcSwap<CellMatrix>>,
    paint_request: Arc<tokio::sync::Notify>,
) {
    info!(
        target: "lattice_host::cells_worker",
        "cells worker spawned (S2.2)"
    );
    let mut tick_count: u64 = 0;
    loop {
        wake.0.notified().await;
        let t0 = std::time::Instant::now();
        let decision = recompute(&render_state, &matrix_cell);
        let elapsed_us = t0.elapsed().as_micros();
        tick_count += 1;
        // Wake the renderer on content changes only. CacheHit
        // leaves the matrix bit-identical so waking the peer would
        // be a wasted frame.
        if matches!(decision, WorkerDecision::Recomputed | WorkerDecision::Clear) {
            paint_request.notify_one();
        }
        debug!(
            target: "lattice_host::cells_worker",
            tick = tick_count,
            ?decision,
            elapsed_us,
            "cells worker tick"
        );
    }
}

/// Pure synchronous recompute. Reads the current published
/// `CellsRenderState`, decides whether to recompute, and updates
/// `matrix_cell` accordingly. Returns the decision taken so tests
/// can assert each branch without driving the async loop.
pub fn recompute(
    render_state: &ArcSwap<RenderState>,
    matrix_cell: &ArcSwap<CellMatrix>,
) -> WorkerDecision {
    let rs = render_state.load_full();
    let cells = &rs.cells;

    let Some(snapshot) = cells.snapshot.as_ref() else {
        // No active document. Clear the published matrix if it
        // isn't already empty.
        let existing = matrix_cell.load();
        if existing.is_empty() && existing.version == MatrixVersion::ZERO {
            return WorkerDecision::Clear;
        }
        matrix_cell.store(Arc::new(CellMatrix::empty()));
        return WorkerDecision::Clear;
    };

    // Cache hit: published matrix's version matches the publisher's
    // inputs.
    let existing = matrix_cell.load_full();
    if !cells.version.differs_from(&existing.version) {
        return WorkerDecision::CacheHit;
    }

    // Cache miss: rebuild. S2.2 is whole-doc mode only; S2.4 picks
    // chunked when above 4 × viewport_height lines.
    let matrix = build_whole_doc_matrix(snapshot.as_ref(), cells.version);
    matrix_cell.store(Arc::new(matrix));
    WorkerDecision::Recomputed
}

/// Build a whole-doc [`CellMatrix`] from `snapshot` alone — ASCII
/// codepoints, no fg/bg/flags. The S2.2 minimal producer.
///
/// One [`CellRow`] per source line. Each row's `cells` are the
/// line's chars as `Cell::with_codepoint(c as u32)`. `source_line`
/// is the line index; `inlay_offsets` is empty (S2.3 lands inlay
/// splicing).
fn build_whole_doc_matrix(
    snapshot: &lattice_runtime::DocumentSnapshot,
    version: MatrixVersion,
) -> CellMatrix {
    let line_count = snapshot.buffer.line_count();
    if line_count == 0 {
        return CellMatrix::empty();
    }
    let mut rows: Vec<CellRow> = Vec::with_capacity(line_count as usize);
    for line_idx in 0..line_count {
        let text = snapshot.buffer.line(line_idx).unwrap_or_default();
        let cells: Vec<Cell> = text.chars().map(|c| Cell::with_codepoint(c as u32)).collect();
        rows.push(CellRow::new(
            cells,
            line_idx,
            Vec::<lattice_cells::row::InlayOffset>::new(),
        ));
    }
    let chunk = Arc::new(CellChunk::new(0, rows, version));
    CellMatrix::whole_doc(chunk, line_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_state::{CellsRenderState, RenderState};
    use lattice_core::Document;
    use lattice_runtime::DocumentSnapshot;

    /// Helper: build a `RenderState` whose `cells` substate carries
    /// `snapshot` + `version` and shares `matrix_cell` with the
    /// caller.
    fn rs_with_snapshot(
        snapshot: Option<Arc<DocumentSnapshot>>,
        version: MatrixVersion,
        matrix_cell: Arc<ArcSwap<CellMatrix>>,
    ) -> ArcSwap<RenderState> {
        let cells = CellsRenderState {
            matrix: matrix_cell,
            version,
            snapshot,
            syntax_handle: None,
            inlay_hints: Arc::from(
                Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
            ),
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            viewport_height: 0,
        };
        let rs = RenderState {
            cells: Arc::new(cells),
            ..RenderState::default()
        };
        ArcSwap::from_pointee(rs)
    }

    fn snap_of(text: &str) -> Arc<DocumentSnapshot> {
        let doc = Document::from_text(text);
        Arc::new(DocumentSnapshot::__bench_from_document(&doc))
    }

    fn v(text: u64) -> MatrixVersion {
        MatrixVersion {
            text,
            syntax: 0,
            inlay_hints: 0,
            folds: 0,
            theme: 0,
        }
    }

    /// `recompute` with `snapshot: None` clears the matrix and
    /// short-circuits when the published matrix is already empty.
    #[test]
    fn recompute_with_no_snapshot_clears_matrix() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Seed a non-empty matrix so the first call exercises the
        // store path.
        let pre_chunk = Arc::new(CellChunk::new(
            0,
            vec![CellRow::new(
                vec![Cell::with_codepoint(b'x' as u32)],
                0,
                Vec::<lattice_cells::row::InlayOffset>::new(),
            )],
            v(7),
        ));
        matrix_cell.store(Arc::new(CellMatrix::whole_doc(pre_chunk, 1)));
        let rs = rs_with_snapshot(None, v(7), matrix_cell.clone());

        let decision = recompute(&rs, &matrix_cell);
        assert_eq!(decision, WorkerDecision::Clear);
        assert!(matrix_cell.load().is_empty());

        // Second call sees an already-empty matrix at version ZERO;
        // the idempotent Clear branch short-circuits without a store.
        let before = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Clear);
        let after = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(before, after, "idempotent Clear must not churn the Arc");
    }

    /// Cache miss: with a fresh snapshot + non-matching version,
    /// the worker builds a matrix that reflects every line.
    #[test]
    fn recompute_publishes_matrix_for_snapshot_text() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap = snap_of("ab\ncd\nef");
        let rs = rs_with_snapshot(Some(snap), v(1), matrix_cell.clone());

        let decision = recompute(&rs, &matrix_cell);
        assert_eq!(decision, WorkerDecision::Recomputed);

        let matrix = matrix_cell.load();
        assert!(matrix.is_whole_doc());
        // ropey counts a trailing implicit line; 3 newline-separated
        // lines without trailing `\n` yields exactly 3 lines.
        assert_eq!(matrix.visible_line_count, 3);
        assert_eq!(matrix.source_line_count, 3);
        let slice = matrix.slice(0, 10);
        let rows: Vec<&CellRow> = slice.iter().collect();
        assert_eq!(rows.len(), 3);
        let row_text = |r: &CellRow| -> String {
            r.cells
                .iter()
                .map(|c| char::from_u32(c.codepoint).unwrap_or('?'))
                .collect()
        };
        assert_eq!(row_text(rows[0]), "ab");
        assert_eq!(row_text(rows[1]), "cd");
        assert_eq!(row_text(rows[2]), "ef");
        assert_eq!(matrix.version, v(1));
    }

    /// Cache hit: a second `recompute` with matching versions sees
    /// `published_matrix.version == cells.version` and short-circuits.
    /// The stored Arc identity is preserved.
    #[test]
    fn recompute_with_matching_version_is_cache_hit() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap = snap_of("hello");
        let rs = rs_with_snapshot(Some(snap), v(4), matrix_cell.clone());

        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::CacheHit);
        let second_ptr = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(
            first_ptr, second_ptr,
            "cache-hit must not store a new Arc"
        );
    }

    /// Version bump triggers a fresh build. Earlier matrix is
    /// replaced; new matrix carries the new version stamp.
    #[test]
    fn version_bump_rebuilds_matrix() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of("aaa");
        let rs1 = rs_with_snapshot(Some(snap1), v(1), matrix_cell.clone());
        assert_eq!(recompute(&rs1, &matrix_cell), WorkerDecision::Recomputed);
        assert_eq!(matrix_cell.load().version, v(1));

        // New snapshot + bumped text version.
        let snap2 = snap_of("bbbb");
        let rs2 = rs_with_snapshot(Some(snap2), v(2), matrix_cell.clone());
        assert_eq!(recompute(&rs2, &matrix_cell), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert_eq!(m.version, v(2));
        assert_eq!(m.visible_line_count, 1);
        let first_row = m.slice(0, 1).iter().next().cloned().unwrap();
        assert_eq!(first_row.cells.len(), 4);
    }

    /// Empty text produces a single empty row (ropey reports one
    /// line for an empty buffer). Distinct from the no-snapshot
    /// `Clear` branch.
    #[test]
    fn empty_text_produces_one_empty_row() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap = snap_of("");
        let rs = rs_with_snapshot(Some(snap), v(1), matrix_cell.clone());
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert_eq!(m.visible_line_count, 1);
        let row = m.slice(0, 1).iter().next().cloned().unwrap();
        assert!(row.is_empty());
        assert_eq!(row.source_line, 0);
    }
}
