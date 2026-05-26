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

    // Cache miss: rebuild. S2.2 was whole-doc mode + raw codepoints;
    // S2.3.a adds syntax-resolved fg via the host theme. Chunked
    // mode lands in S2.4.
    let matrix = build_whole_doc_matrix(
        snapshot.as_ref(),
        cells.syntax_handle.as_deref(),
        &cells.theme,
        cells.version,
    );
    matrix_cell.store(Arc::new(matrix));
    WorkerDecision::Recomputed
}

/// Build a whole-doc [`CellMatrix`] from `snapshot` + optional
/// syntax handle + theme.
///
/// One [`CellRow`] per source line. Cell codepoints come from the
/// document snapshot's rope. `cell.fg` is the theme-resolved RGB
/// for the syntax span covering each byte; bytes outside any span
/// (or every byte when no syntax handle is attached) take the
/// theme's `Style::Default` fg. S2.3.b will fold inlay splicing
/// into this loop; S2.3.c adds fold elision.
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
    version: MatrixVersion,
) -> CellMatrix {
    let line_count = snapshot.buffer.line_count();
    if line_count == 0 {
        return CellMatrix::empty();
    }

    let default_fg = resolve_fg(theme, lattice_syntax::Style::Default);

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

    let mut rows: Vec<CellRow> = Vec::with_capacity(line_count as usize);
    for line_idx in 0..line_count {
        let text = snapshot.buffer.line(line_idx).unwrap_or_default();
        let line_spans: &[lattice_syntax::StyledSpan] = per_line_spans
            .as_ref()
            .and_then(|v| v.get(line_idx as usize))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let cells: Vec<Cell> = text
            .char_indices()
            .map(|(byte, ch)| {
                let style = style_at_byte(line_spans, byte);
                let fg = if matches!(style, lattice_syntax::Style::Default) {
                    default_fg
                } else {
                    resolve_fg(theme, style)
                };
                Cell::new(ch as u32, fg, 0, 0)
            })
            .collect();
        rows.push(CellRow::new(
            cells,
            line_idx,
            Vec::<lattice_cells::row::InlayOffset>::new(),
        ));
    }
    let chunk = Arc::new(CellChunk::new(0, rows, version));
    CellMatrix::whole_doc(chunk, line_count)
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
        let cells = CellsRenderState {
            matrix: matrix_cell,
            version,
            snapshot,
            syntax_handle,
            inlay_hints: Arc::from(
                Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
            ),
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            viewport_height: 0,
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
