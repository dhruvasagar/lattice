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

    // Cache miss: rebuild. S2.2 was whole-doc + raw codepoints;
    // S2.3.a adds syntax-resolved fg via the host theme; S2.3.b
    // splices inlay-hint cells; S2.3.c elides closed-fold interior
    // lines. Chunked mode lands in S2.4.
    let matrix = build_whole_doc_matrix(
        snapshot.as_ref(),
        cells.syntax_handle.as_deref(),
        &cells.theme,
        &cells.inlay_hints,
        &cells.folds,
        cells.foldenable,
        cells.version,
    );
    matrix_cell.store(Arc::new(matrix));
    WorkerDecision::Recomputed
}

/// Build a whole-doc [`CellMatrix`] from `snapshot` + optional
/// syntax handle + theme + inlay-hint payload + folds.
///
/// One [`CellRow`] per source line that survives fold elision. Cell
/// codepoints come from the document snapshot's rope. `cell.fg` is
/// the theme-resolved RGB for the syntax span covering each byte;
/// bytes outside any span (or every byte when no syntax handle is
/// attached) take the theme's `Style::Default` fg. Inlay hints
/// whose `(line, byte)` falls inside the visible range splice
/// virtual cells (one per inlay char) at that position with
/// `flags::INLAY` set, and record `(orig_byte, char_width)` on
/// `CellRow::inlay_offsets`. Source lines that fall *strictly
/// inside* a closed fold (`start_line < line <= end_line`) produce
/// no row — the fold's `start_line` is the only visible row for the
/// folded section, matching the existing `line_inside_closed_fold`
/// semantics.
///
/// Stale-syntax behaviour: if the syntax snapshot's
/// `text_version` is behind the document's `text_version`, the
/// snapshot's byte offsets no longer align with the current rope
/// and re-styling against them would mis-colour edits. The worker
/// falls back to the default fg for the whole document in that
/// case. The matrix rebuilds again when the syntax catches up
/// (the cascade bumps `MatrixVersion::syntax`, which equals the
/// document's `text_version` at publish time).
fn build_whole_doc_matrix(
    snapshot: &lattice_runtime::DocumentSnapshot,
    syntax_handle: Option<&lattice_syntax::SyntaxHandle>,
    theme: &crate::ui::theme::Theme,
    inlay_hints: &[crate::render_state::InlayHintRow],
    folds: &[lattice_core::Fold],
    foldenable: bool,
    version: MatrixVersion,
) -> CellMatrix {
    let line_count = snapshot.buffer.line_count();
    if line_count == 0 {
        return CellMatrix::empty();
    }

    let default_fg = resolve_fg(theme, lattice_syntax::Style::Default);
    let inlay_fg = inlay_hint_fg();

    // Resolve per-line styled spans when a current syntax snapshot
    // is available. `highlight_lines` returns one Vec<StyledSpan>
    // per line in [0, line_count); spans are line-relative byte
    // offsets.
    let per_line_spans: Option<Vec<Vec<lattice_syntax::StyledSpan>>> = syntax_handle.and_then(|h| {
        let snap = h.snapshot();
        // Stale snapshot — don't paint with mismatched offsets.
        // Worker will rebuild when the syntax catches up.
        if snap.text_version() < snapshot.text_version {
            return None;
        }
        snap.highlight_lines(0, line_count).ok()
    });

    // Bucket inlay hints by line so each row's splice walk is
    // O(inlays_on_line). Pre-sorted ascending by byte within each
    // bucket — the splice walk assumes that order.
    let inlays_by_line = bucket_inlays_by_line(inlay_hints, line_count);

    // Fold index — predicates collapse to `false` when foldenable
    // is off, so the elision branch becomes a no-op for `zi`.
    let fold_index = crate::folds::FoldIndex::from_folds(folds, foldenable);

    let mut rows: Vec<CellRow> = Vec::with_capacity(line_count as usize);
    for line_idx in 0..line_count {
        // S2.3.c: skip source lines that fall strictly inside a
        // closed fold. The fold's start_line stays visible
        // (renderer paints the fold marker there); only the
        // interior is elided.
        if fold_index.line_inside_closed_fold(line_idx) {
            continue;
        }
        let text = snapshot.buffer.line(line_idx).unwrap_or_default();
        let line_spans: &[lattice_syntax::StyledSpan] = per_line_spans
            .as_ref()
            .and_then(|v| v.get(line_idx as usize))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let line_inlays = inlays_by_line
            .get(line_idx as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (cells, inlay_offsets) = build_row_cells(
            &text,
            line_spans,
            line_inlays,
            theme,
            default_fg,
            inlay_fg,
        );
        rows.push(CellRow::new(cells, line_idx, inlay_offsets));
    }
    let chunk = Arc::new(CellChunk::new(0, rows, version));
    CellMatrix::whole_doc(chunk, line_count)
}

/// Per-row build: walks `text` char-by-char, splices inlay text at
/// each `(orig_byte, text)` position, emits source cells with
/// theme-resolved fg and inlay cells with `flags::INLAY`. Returns
/// `(cells, inlay_offsets)` ready for `CellRow::new`.
///
/// Splice points are inclusive at byte position (inlays whose
/// `orig_byte <= char_byte_start` splice in *before* that char).
/// Trailing inlays at or past EOL splice at end-of-line — matches
/// the existing `highlights_worker::weave_row` contract so S3
/// renderers can switch substrates without semantic drift.
fn build_row_cells(
    text: &str,
    line_spans: &[lattice_syntax::StyledSpan],
    line_inlays: &[(u32, &str)],
    theme: &crate::ui::theme::Theme,
    default_fg: u32,
    inlay_fg: u32,
) -> (Vec<Cell>, Vec<lattice_cells::row::InlayOffset>) {
    // Capacity: source chars + sum of inlay char widths. Slight
    // over-estimate is fine.
    let inlay_total_chars: usize = line_inlays
        .iter()
        .map(|(_, t)| t.chars().count())
        .sum();
    let mut cells: Vec<Cell> = Vec::with_capacity(text.len() + inlay_total_chars);
    let mut inlay_offsets: Vec<lattice_cells::row::InlayOffset> =
        Vec::with_capacity(line_inlays.len());

    let resolve = |style: lattice_syntax::Style| -> u32 {
        if matches!(style, lattice_syntax::Style::Default) {
            default_fg
        } else {
            resolve_fg(theme, style)
        }
    };

    let mut inlay_idx = 0usize;
    for (byte, ch) in text.char_indices() {
        // Splice every inlay whose `orig_byte` is at or before this
        // char position. Order-of-arrival ties at the same byte
        // resolve in input order.
        while inlay_idx < line_inlays.len()
            && (line_inlays[inlay_idx].0 as usize) <= byte
        {
            let (orig_byte, t) = line_inlays[inlay_idx];
            let char_width = t.chars().count() as u32;
            inlay_offsets.push((orig_byte, char_width));
            for ic in t.chars() {
                cells.push(Cell::new(
                    ic as u32,
                    inlay_fg,
                    0,
                    lattice_cells::cell_flags::INLAY,
                ));
            }
            inlay_idx += 1;
        }
        let style = style_at_byte(line_spans, byte);
        cells.push(Cell::new(ch as u32, resolve(style), 0, 0));
    }
    // Trailing inlays at/past EOL.
    while inlay_idx < line_inlays.len() {
        let (orig_byte, t) = line_inlays[inlay_idx];
        let char_width = t.chars().count() as u32;
        inlay_offsets.push((orig_byte, char_width));
        for ic in t.chars() {
            cells.push(Cell::new(
                ic as u32,
                inlay_fg,
                0,
                lattice_cells::cell_flags::INLAY,
            ));
        }
        inlay_idx += 1;
    }

    (cells, inlay_offsets)
}

/// Bucket a flat inlay-hints list by line into per-line slices of
/// `(orig_byte, text)`, each bucket sorted ascending by `orig_byte`.
/// Output length is `line_count` so callers can index by line
/// without bounds-checking. Hints whose `line` is past `line_count`
/// are dropped — out-of-range payloads do not feed the build.
fn bucket_inlays_by_line<'a>(
    inlay_hints: &'a [crate::render_state::InlayHintRow],
    line_count: u32,
) -> Vec<Vec<(u32, &'a str)>> {
    let mut buckets: Vec<Vec<(u32, &'a str)>> =
        vec![Vec::new(); line_count as usize];
    if inlay_hints.is_empty() {
        return buckets;
    }
    for h in inlay_hints {
        if h.line < line_count {
            buckets[h.line as usize].push((h.byte, h.text.as_str()));
        }
    }
    for b in &mut buckets {
        b.sort_by_key(|(off, _)| *off);
    }
    buckets
}

/// Hard-coded `0x7f7f7f` foreground for inlay-hint cells —
/// mirrors the TUI's existing `DarkGray` inlay style. A dedicated
/// `inlay_hint_style` theme slot is a follow-up alongside the
/// match / selection bg slots tracked in the polish backlog
/// (#19).
fn inlay_hint_fg() -> u32 {
    crate::ui::theme::Color::Named(crate::ui::theme::NamedColor::DarkGray)
        .to_rgb_u32(0)
}

/// Resolve a syntax style to its `0xRRGGBB` foreground colour via
/// the host theme. `Style::Default` and styles whose theme entry
/// has no explicit fg return `0` — the renderer maps that to "use
/// the pane's default text colour" at paint time.
fn resolve_fg(theme: &crate::ui::theme::Theme, style: lattice_syntax::Style) -> u32 {
    theme
        .syntax_style(style)
        .fg
        .map(|c| c.to_rgb_u32(0))
        .unwrap_or(0)
}

/// Resolve the highlight style at a given utf-8 byte offset inside
/// `line_spans`. Mirrors `highlights_worker::style_at_byte` —
/// bytes outside every span fall through to `Style::Default`.
fn style_at_byte(
    line_spans: &[lattice_syntax::StyledSpan],
    byte: usize,
) -> lattice_syntax::Style {
    for s in line_spans {
        if byte >= s.start && byte < s.end {
            return s.style;
        }
    }
    lattice_syntax::Style::Default
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
        rs_with_snapshot_themed(
            snapshot,
            version,
            matrix_cell,
            None,
            crate::ui::theme::Theme::default(),
        )
    }

    /// Themed variant used by S2.3.a tests that need a non-default
    /// syntax handle or a tweaked theme palette.
    fn rs_with_snapshot_themed(
        snapshot: Option<Arc<DocumentSnapshot>>,
        version: MatrixVersion,
        matrix_cell: Arc<ArcSwap<CellMatrix>>,
        syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,
        theme: crate::ui::theme::Theme,
    ) -> ArcSwap<RenderState> {
        rs_with_snapshot_full(
            snapshot,
            version,
            matrix_cell,
            syntax_handle,
            theme,
            Vec::<crate::render_state::InlayHintRow>::new(),
        )
    }

    /// Full-input variant used by S2.3.b tests that need to drive
    /// the inlay-hint splice path.
    fn rs_with_snapshot_full(
        snapshot: Option<Arc<DocumentSnapshot>>,
        version: MatrixVersion,
        matrix_cell: Arc<ArcSwap<CellMatrix>>,
        syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,
        theme: crate::ui::theme::Theme,
        inlay_hints: Vec<crate::render_state::InlayHintRow>,
    ) -> ArcSwap<RenderState> {
        rs_with_snapshot_full_folded(
            snapshot,
            version,
            matrix_cell,
            syntax_handle,
            theme,
            inlay_hints,
            Vec::new(),
            true,
        )
    }

    /// Folded variant used by S2.3.c tests that need to drive the
    /// fold-elision path.
    fn rs_with_snapshot_full_folded(
        snapshot: Option<Arc<DocumentSnapshot>>,
        version: MatrixVersion,
        matrix_cell: Arc<ArcSwap<CellMatrix>>,
        syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,
        theme: crate::ui::theme::Theme,
        inlay_hints: Vec<crate::render_state::InlayHintRow>,
        folds: Vec<lattice_core::Fold>,
        foldenable: bool,
    ) -> ArcSwap<RenderState> {
        let cells = CellsRenderState {
            matrix: matrix_cell,
            version,
            snapshot,
            syntax_handle,
            inlay_hints: Arc::from(inlay_hints.into_boxed_slice()),
            folds: Arc::from(folds.into_boxed_slice()),
            viewport_height: 0,
            foldenable,
            theme,
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

    // ---- S2.3.a — syntax fg + theme palette ----

    /// Helper: build a seeded Rust `SyntaxHandle` parsed against
    /// `text` at the given text_version.
    fn rust_handle(text: &str, text_version: u64) -> Arc<lattice_syntax::SyntaxHandle> {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .expect("rust grammar available in test build");
        s.parse_at(text, text_version);
        Arc::new(lattice_syntax::SyntaxHandle::seeded(s))
    }

    /// Helper: produce a snapshot whose `text_version` matches the
    /// caller-supplied value (so syntax / doc text-versions line up
    /// in tests without driving the actor).
    fn snap_of_versioned(text: &str, text_version: u64) -> Arc<DocumentSnapshot> {
        let doc = Document::from_text(text);
        let mut s = DocumentSnapshot::__bench_from_document(&doc);
        s.text_version = text_version;
        Arc::new(s)
    }

    /// With a syntax handle attached and the default theme, the
    /// `fn` keyword on line 0 takes the theme's keyword fg
    /// (`0xcba6f7`); a comment line takes the comment fg
    /// (`0x6c7086`); plain text takes the default fg (`0xcdd6f4`).
    #[test]
    fn syntax_handle_resolves_keyword_string_comment_fg() {
        let theme = crate::ui::theme::Theme::default();
        // Line 0: `fn` keyword + identifier + paren punctuation.
        // Line 1: line comment.
        // Line 2: plain whitespace / EOF.
        let text = "fn main() {}\n// comment\n";
        let handle = rust_handle(text, 1);
        let snap = snap_of_versioned(text, 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs = rs_with_snapshot_themed(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            Some(handle),
            theme,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        let rows: Vec<&CellRow> = m.slice(0, 10).iter().collect();
        assert!(rows.len() >= 2, "expected at least 2 rows for {text:?}");

        // Default fg for the theme is Catppuccin Mocha "Text"
        // (0xcdd6f4) and the keyword fg is Mauve (0xcba6f7).
        let expected_default = resolve_fg(&theme, lattice_syntax::Style::Default);
        let expected_keyword = resolve_fg(&theme, lattice_syntax::Style::Keyword);
        let expected_comment = resolve_fg(&theme, lattice_syntax::Style::LineComment);
        assert_eq!(expected_keyword, 0x00cb_a6f7);
        assert_ne!(expected_default, expected_keyword);

        // First two cells of line 0 are `f` and `n` (the `fn`
        // keyword). Both should carry the keyword fg.
        let line0 = rows[0];
        assert!(line0.cells.len() >= 2, "line 0 has at least `fn`");
        assert_eq!(line0.cells[0].codepoint, b'f' as u32);
        assert_eq!(line0.cells[0].fg, expected_keyword);
        assert_eq!(line0.cells[1].codepoint, b'n' as u32);
        assert_eq!(line0.cells[1].fg, expected_keyword);

        // Line 1 is a line comment — every byte takes the comment fg.
        let line1 = rows[1];
        assert!(line1.cells.iter().all(|c| c.fg == expected_comment),
            "every cell on a line-comment row must carry the comment fg; got {:?}",
            line1.cells.iter().map(|c| c.fg).collect::<Vec<_>>());
    }

    /// Without a syntax handle, every cell on every line takes the
    /// theme's default fg — proves the no-handle fallback path
    /// doesn't accidentally use a different colour.
    #[test]
    fn no_syntax_handle_yields_default_fg_everywhere() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of("ab\ncd");
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs = rs_with_snapshot_themed(Some(snap), v(1), matrix_cell.clone(), None, theme);
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let default_fg = resolve_fg(&theme, lattice_syntax::Style::Default);
        let m = matrix_cell.load();
        for row in m.slice(0, 10).iter() {
            for c in row.cells.iter() {
                assert_eq!(c.fg, default_fg, "no-handle path must use default fg");
            }
        }
    }

    /// A syntax snapshot whose `text_version` lags the document's
    /// `text_version` is treated as stale: the worker falls back
    /// to default fg rather than painting against mismatched byte
    /// offsets. Mirrors `highlights_worker`'s stale-hold contract.
    #[test]
    fn stale_syntax_falls_back_to_default_fg() {
        let theme = crate::ui::theme::Theme::default();
        // Snapshot parsed against version 1; document advanced
        // to version 2 (mid-edit, syntax hasn't reparsed yet).
        let text = "fn x() {}";
        let handle = rust_handle(text, 1);
        let snap = snap_of_versioned(text, 2);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs = rs_with_snapshot_themed(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            Some(handle),
            theme,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);

        let default_fg = resolve_fg(&theme, lattice_syntax::Style::Default);
        let m = matrix_cell.load();
        for row in m.slice(0, 10).iter() {
            for c in row.cells.iter() {
                assert_eq!(
                    c.fg, default_fg,
                    "stale-syntax fallback must use default fg, got {:#08x}",
                    c.fg
                );
            }
        }
    }

    // ---- S2.3.b — inlay-hint splicing ----

    fn inlay(line: u32, byte: u32, text: &str) -> crate::render_state::InlayHintRow {
        crate::render_state::InlayHintRow {
            line,
            byte,
            text: text.to_string(),
        }
    }

    fn row_text(r: &CellRow) -> String {
        r.cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('?'))
            .collect()
    }

    /// Single inlay spliced mid-line: combined text reflects the
    /// inlay, the spliced cells carry `flags::INLAY`, and
    /// `inlay_offsets` records `(orig_byte, char_width)` so
    /// `byte_to_combined_col` returns the post-inlay column for
    /// later bytes.
    #[test]
    fn single_inlay_splices_into_row_and_sets_flags() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("hello", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let hints = vec![inlay(0, 2, ": ")];
        let rs = rs_with_snapshot_full(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            hints,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        let row = m.slice(0, 1).iter().next().cloned().unwrap();
        // Combined cells: `h e : SPACE l l o`.
        assert_eq!(row_text(&row), "he: llo");
        // Inlay-spliced cells at index 2, 3 carry the INLAY flag.
        assert!(row.cells[2].is_inlay(), "cell 2 (`:`) must be INLAY");
        assert!(row.cells[3].is_inlay(), "cell 3 (` `) must be INLAY");
        // Source cells stay clean.
        assert!(!row.cells[0].is_inlay());
        assert!(!row.cells[1].is_inlay());
        assert!(!row.cells[4].is_inlay());
        // Inlay foreground is the hardcoded DarkGray (0x7f7f7f).
        assert_eq!(row.cells[2].fg, 0x7f7f7f);
        // Offsets: one entry, (2, 2) for `(orig_byte, char_width)`.
        assert_eq!(row.inlay_offsets.as_ref(), &[(2u32, 2u32)] as &[_]);
        // byte_to_combined_col round-trip: source byte 2 sits at
        // combined col 4 (after the 2-wide inlay).
        assert_eq!(row.byte_to_combined_col(0), 0);
        assert_eq!(row.byte_to_combined_col(2), 4);
        assert_eq!(row.byte_to_combined_col(3), 5);
    }

    /// Two inlays on the same line, presented out-of-order in the
    /// payload, splice in `(byte, sequence-of-arrival)` order after
    /// the worker's per-line `sort_by_key`.
    #[test]
    fn multiple_inlays_splice_in_byte_order() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("abc", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Insert out of order on purpose.
        let hints = vec![inlay(0, 2, "[2]"), inlay(0, 1, "[1]")];
        let rs = rs_with_snapshot_full(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            hints,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let row = matrix_cell.load().slice(0, 1).iter().next().cloned().unwrap();
        assert_eq!(row_text(&row), "a[1]b[2]c");
        // Offsets ordered by orig_byte.
        assert_eq!(
            row.inlay_offsets.as_ref(),
            &[(1u32, 3u32), (2u32, 3u32)] as &[_]
        );
    }

    /// An inlay at byte 0 splices *before* the first char of the
    /// line — covers the boundary case the byte<=byte splice
    /// inequality is meant to handle.
    #[test]
    fn inlay_at_line_start_splices_before_first_char() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("xyz", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let hints = vec![inlay(0, 0, "?")];
        let rs = rs_with_snapshot_full(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            hints,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let row = matrix_cell.load().slice(0, 1).iter().next().cloned().unwrap();
        assert_eq!(row_text(&row), "?xyz");
        assert!(row.cells[0].is_inlay());
        assert_eq!(row.inlay_offsets.as_ref(), &[(0u32, 1u32)] as &[_]);
    }

    /// A trailing inlay (orig_byte == line_len) splices at EOL.
    /// Matches the highlights_worker contract so future renderer
    /// cutovers don't surprise the user with disappearing
    /// end-of-line hints.
    #[test]
    fn trailing_inlay_splices_at_end_of_line() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("ab", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let hints = vec![inlay(0, 2, ";")];
        let rs = rs_with_snapshot_full(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            hints,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let row = matrix_cell.load().slice(0, 1).iter().next().cloned().unwrap();
        assert_eq!(row_text(&row), "ab;");
        assert!(row.cells[2].is_inlay());
        assert_eq!(row.inlay_offsets.as_ref(), &[(2u32, 1u32)] as &[_]);
    }

    /// An inlay-version bump (same text + theme, new inlay
    /// payload) triggers a recompute. Demonstrates the cells.
    /// inlay_hints field participates in the version axes.
    #[test]
    fn inlay_version_bump_triggers_rebuild() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("a", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let v_a = MatrixVersion {
            text: 1,
            syntax: 1,
            inlay_hints: 0,
            folds: 0,
            theme: 0,
        };
        let v_b = MatrixVersion { inlay_hints: 1, ..v_a };

        let rs1 = rs_with_snapshot_full(
            Some(snap.clone()),
            v_a,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
        );
        assert_eq!(recompute(&rs1, &matrix_cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&matrix_cell.load_full());

        // Add an inlay + bump the version.
        let rs2 = rs_with_snapshot_full(
            Some(snap),
            v_b,
            matrix_cell.clone(),
            None,
            theme,
            vec![inlay(0, 0, "!")],
        );
        assert_eq!(recompute(&rs2, &matrix_cell), WorkerDecision::Recomputed);
        assert_ne!(first_ptr, Arc::as_ptr(&matrix_cell.load_full()));
        let row = matrix_cell.load().slice(0, 1).iter().next().cloned().unwrap();
        assert_eq!(row_text(&row), "!a");
    }

    // ---- S2.3.c — fold elision ----

    fn closed_fold(start: u32, end: u32) -> lattice_core::Fold {
        lattice_core::Fold {
            start_line: start,
            end_line: end,
            closed: true,
            identity: None,
        }
    }

    fn open_fold(start: u32, end: u32) -> lattice_core::Fold {
        lattice_core::Fold {
            start_line: start,
            end_line: end,
            closed: false,
            identity: None,
        }
    }

    /// A closed fold drops its interior source lines from the
    /// matrix. The fold's `start_line` stays visible — vim renders
    /// the marker there — and `source_line` on the next surviving
    /// row preserves its logical line index (so the renderer maps
    /// the click-target back to the source).
    #[test]
    fn closed_fold_elides_interior_lines() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("a\nb\nc\nd\ne", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Fold lines 1..3 — interior lines 2, 3 are elided; line
        // 1 (start) stays.
        let folds = vec![closed_fold(1, 3)];
        let rs = rs_with_snapshot_full_folded(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            folds,
            true,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        // source_line_count is preserved (pre-fold logical count).
        assert_eq!(m.source_line_count, 5);
        // visible_line_count post-fold: 5 - 2 elided = 3 rows.
        assert_eq!(m.visible_line_count, 3);
        let source_lines: Vec<u32> = m
            .slice(0, 10)
            .iter()
            .map(|r| r.source_line)
            .collect();
        assert_eq!(source_lines, vec![0, 1, 4]);
    }

    /// An OPEN fold does not elide its interior. The presence of a
    /// fold range in the list is not enough — only `closed = true`
    /// participates.
    #[test]
    fn open_fold_does_not_elide() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("a\nb\nc", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let folds = vec![open_fold(0, 2)];
        let rs = rs_with_snapshot_full_folded(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            folds,
            true,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert_eq!(m.visible_line_count, 3);
        let source_lines: Vec<u32> =
            m.slice(0, 10).iter().map(|r| r.source_line).collect();
        assert_eq!(source_lines, vec![0, 1, 2]);
    }

    /// `foldenable = false` disables elision even with closed folds
    /// in the list — `zi` (toggle) produces the unfolded matrix
    /// from the same payload without re-touching the fold list.
    #[test]
    fn foldenable_off_disables_elision() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("a\nb\nc", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let folds = vec![closed_fold(0, 2)];
        let rs = rs_with_snapshot_full_folded(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            folds,
            false,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert_eq!(m.visible_line_count, 3, "no elision when foldenable=false");
    }

    /// Two non-overlapping closed folds both elide their interiors.
    /// Establishes that the FoldIndex's `partition_point` walk
    /// handles multiple folds correctly (the worker just calls
    /// `line_inside_closed_fold` per line).
    #[test]
    fn multiple_closed_folds_elide_independently() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("a\nb\nc\nd\ne\nf\ng", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Fold lines 0..2 + 4..6. Visible: 0, 3, 4 (start of 2nd
        // fold).
        let folds = vec![closed_fold(0, 2), closed_fold(4, 6)];
        let rs = rs_with_snapshot_full_folded(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            folds,
            true,
        );
        assert_eq!(recompute(&rs, &matrix_cell), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        let source_lines: Vec<u32> =
            m.slice(0, 10).iter().map(|r| r.source_line).collect();
        assert_eq!(source_lines, vec![0, 3, 4]);
    }

    /// Theme axis bump rebuilds the matrix even with identical
    /// text + syntax. Validates that `MatrixVersion::theme`
    /// participates in `differs_from`.
    #[test]
    fn theme_version_bump_triggers_rebuild() {
        let theme = crate::ui::theme::Theme::default();
        let snap = snap_of_versioned("ab", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let v_a = MatrixVersion {
            text: 1,
            syntax: 1,
            inlay_hints: 0,
            folds: 0,
            theme: 0xaa,
        };
        let v_b = MatrixVersion { theme: 0xbb, ..v_a };

        let rs1 = rs_with_snapshot_themed(
            Some(snap.clone()),
            v_a,
            matrix_cell.clone(),
            None,
            theme,
        );
        assert_eq!(recompute(&rs1, &matrix_cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&matrix_cell.load_full());

        // Repeat with the same version: cache-hit, no store.
        assert_eq!(recompute(&rs1, &matrix_cell), WorkerDecision::CacheHit);
        assert_eq!(first_ptr, Arc::as_ptr(&matrix_cell.load_full()));

        // Bump only the theme axis: must rebuild.
        let rs2 =
            rs_with_snapshot_themed(Some(snap), v_b, matrix_cell.clone(), None, theme);
        assert_eq!(recompute(&rs2, &matrix_cell), WorkerDecision::Recomputed);
        assert_ne!(first_ptr, Arc::as_ptr(&matrix_cell.load_full()));
    }
}
