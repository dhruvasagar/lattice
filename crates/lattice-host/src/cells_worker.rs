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
use lattice_cells::{CHUNK_SIZE_WHOLE_DOC, Cell, CellChunk, CellMatrix, CellRow, MatrixVersion};

/// 2026-05-27: `display.whitespace.*` snapshot consumed by the
/// cell-builder when emitting cells. Mirrors the
/// `option_cache.whitespace_*` shape but lives here so the worker
/// can be written without a config-crate import cycle.
///
/// `show` is the master gate (`display.show_whitespace`). When
/// false, the builder emits whitespace bytes verbatim. When true:
/// - `tab`: glyph substituted for `\t` (None → leave as-is).
/// - `trailing`: glyph for spaces between the last non-space
///   byte of a line and EOL.
/// - `leading`: glyph for spaces between BOL and the first
///   non-space byte.
/// - `space`: glyph for middle (non-leading, non-trailing)
///   spaces. None → keep middle spaces as ' '.
/// - `eol`: glyph appended at the visual end of each line.
///   Reserved for a follow-up (extending past the last char
///   complicates byte↔col remap).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WhitespaceConfig {
    pub show: bool,
    pub tab: Option<char>,
    pub trailing: Option<char>,
    pub leading: Option<char>,
    pub space: Option<char>,
    pub eol: Option<char>,
    /// W.4.t: columns a hard tab expands to. The builder advances
    /// each `\t` to the next multiple of this width, emitting that
    /// many cells (a marker glyph + space fill when `show`, plain
    /// spaces otherwise) so the cell grid models a tab at its true
    /// display width — one width model the host scroll model and
    /// both renderers share. `0` (the `Default`) is treated as `1`
    /// (legacy one-cell tabs) so test fixtures need no change.
    pub tabstop: u32,
}

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
    /// fresh `CellMatrix` from the snapshot and stored it. The
    /// full-rebuild path — every chunk's rows were materialised
    /// from rope text + syntax + inlays + theme.
    Recomputed,
    /// S2.4.b: incremental rebuild — exactly one text edit happened
    /// since the last publish AND all other version axes are
    /// unchanged. Chunks before the edit's affected range reuse
    /// their `Arc<CellChunk>` verbatim; chunks at the affected
    /// range rebuild from scratch; chunks past the affected range
    /// shift by `lines_added - lines_removed` (cell payloads
    /// shared, only `source_line` advances).
    RecomputedIncremental,
}

/// Worker entry point spawned at boot. Loops forever, awaiting
/// the wake `Notify`. Each wake re-reads the latest
/// `RenderState.cells` inputs and calls [`recompute`].
///
/// Spawn from `editor_boot` once `Editor` is constructed. Pass
/// clones of `Editor::render_state` and `Editor::cells_matrix_cell`
/// plus the wake `Notify` and the `paint_request` notifier.
///
/// ## Coalescing contract (S2.5)
///
/// `tokio::sync::Notify` is *permit-style*: any `notify_one()`
/// calls that arrive while no `notified().await` is pending
/// store a single permit. The next `notified().await` consumes
/// the permit and resolves immediately. This gives us optimal
/// burst coalescing for free:
///
/// - **Quiescent state.** Worker is blocked on `notified().await`.
///   No CPU cost; no permit held.
/// - **Single wake.** Publisher calls `notify_one()` while the
///   worker is parked. Worker resolves, builds, publishes, loops
///   back, parks again.
/// - **Burst during build.** Multiple `notify_one()` calls arrive
///   while the worker is mid-build. They collapse to exactly one
///   stored permit (Notify drops subsequent calls when a permit
///   is already present). After the current build publishes and
///   the worker loops back, the next `notified().await` consumes
///   that single permit; one additional build runs against the
///   LATEST `render_state.load_full()` (which captures all the
///   bursts' inputs because `RenderState` is published atomically
///   via `ArcSwap`).
/// - **Net behaviour for a burst of N publishes during one
///   build.** Exactly 2 builds run: the original and one tail
///   build that catches up to the latest state. No queue, no
///   intermediate states processed; the cumulative `MatrixVersion`
///   diff drives the rebuild decision.
///
/// No explicit debounce is needed. Adding a sleep before
/// processing would *add* latency without reducing useful work —
/// `Notify`'s natural permit semantics already drop intermediate
/// states.
///
/// ## `paint_request` semantics
///
/// `paint_request` is a shared `Notify` consumed by the renderer
/// peer. Both this worker and `highlights_worker` fire
/// `notify_one()` on content-changing decisions. The renderer
/// observes one wake per coalesced burst across both workers and
/// schedules a single paint — matrix + spans are read together
/// from the next `render_state.load_full()`.
///
/// `WorkerDecision::CacheHit` leaves the matrix bit-identical so
/// no paint wake fires. `Clear` / `Recomputed` /
/// `RecomputedIncremental` all signal content change.
pub async fn run(
    render_state: Arc<ArcSwap<RenderState>>,
    wake: CellsWake,
    paint_request: Arc<tokio::sync::Notify>,
) {
    info!(
        target: "lattice_host::cells_worker",
        "cells worker spawned"
    );
    let mut tick_count: u64 = 0;
    loop {
        wake.0.notified().await;
        let t0 = std::time::Instant::now();
        let decision = recompute(&render_state);
        let elapsed_us = t0.elapsed().as_micros();
        tick_count += 1;
        // Wake the renderer on content changes only. CacheHit
        // leaves the matrix bit-identical so waking the peer would
        // be a wasted frame.
        if matches!(
            decision,
            WorkerDecision::Recomputed
                | WorkerDecision::RecomputedIncremental
                | WorkerDecision::Clear
        ) {
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
/// `CellsRenderState`, iterates over every visible Document
/// pane (`cells.panes`), and updates each pane's `matrix`
/// independently. Returns the aggregate decision used to
/// gate the renderer `paint_request` wake:
///
/// - `Clear`: at least one pane saw a `Clear` (and no pane
///   saw a content-producing rebuild).
/// - `Recomputed` / `RecomputedIncremental`: at least one
///   pane rebuilt content. `Recomputed` wins when both kinds
///   of rebuild fire in one tick, since the renderer just
///   needs to know "something changed" — the exact mix
///   matters per-pane only.
/// - `CacheHit`: every pane was idle (or `panes` was empty).
///   No paint wake.
///
/// D.4.d.1.b (2026-05-29): pre-d.1.b, the worker wrote into
/// a single top-level `matrix_cell` corresponding to the
/// active document. Now each pane's own
/// `Arc<ArcSwap<CellMatrix>>` is the write target — the
/// renderer reads them per pane in D.4.d.1.c.
pub fn recompute(render_state: &ArcSwap<RenderState>) -> WorkerDecision {
    let rs = render_state.load_full();
    // I.5.2: `cells` is an inner `ArcSwap` so the keystroke fast path
    // can republish it without reswapping the monolith. Load the
    // current cells snapshot once for this recompute pass.
    let cells = rs.cells.load();
    if cells.panes.is_empty() {
        return WorkerDecision::CacheHit;
    }
    let mut any_recomputed = false;
    let mut any_incremental = false;
    let mut any_cleared = false;
    let ct = CellTheme {
        resolved: &cells.resolved_theme,
        ids: &cells.theme_ids,
    };
    for pane in cells.panes.iter() {
        match recompute_pane(pane, ct, &cells.whitespace) {
            WorkerDecision::CacheHit => {}
            WorkerDecision::Clear => any_cleared = true,
            WorkerDecision::Recomputed => any_recomputed = true,
            WorkerDecision::RecomputedIncremental => any_incremental = true,
        }
    }
    if any_recomputed {
        WorkerDecision::Recomputed
    } else if any_incremental {
        WorkerDecision::RecomputedIncremental
    } else if any_cleared {
        WorkerDecision::Clear
    } else {
        WorkerDecision::CacheHit
    }
}

/// K.4.7 (2026-06-07): assemble per-excerpt tree-sitter highlights for
/// a multibuffer composed row range `[lo, hi)` (exclusive `hi`).
///
/// Returns a `Vec` indexed relative to `lo` (row 0 = composed row `lo`).
/// Each inner `Vec` is the `StyledSpan` list for that row — empty when
/// no excerpt covers it or the excerpt's parse hasn't landed yet.
/// Returns `None` only when the `excerpt_syntax` slice is empty (caller
/// falls back to the standard single-document path).
fn highlight_range_multibuffer(
    excerpt_syntax: &[crate::render_state::ExcerptSyntax],
    lo: u32,
    hi: u32,
) -> Option<Vec<Vec<lattice_syntax::StyledSpan>>> {
    if excerpt_syntax.is_empty() {
        return None;
    }
    if hi <= lo {
        return Some(Vec::new());
    }
    let row_count = (hi - lo) as usize;
    let mut result: Vec<Vec<lattice_syntax::StyledSpan>> = vec![Vec::new(); row_count];
    for ex in excerpt_syntax {
        if ex.composed_start >= hi || ex.composed_end < lo {
            continue;
        }
        let composed_lo = ex.composed_start.max(lo);
        let composed_hi = (ex.composed_end + 1).min(hi);
        let src_lo = ex.source_start + (composed_lo - ex.composed_start);
        let src_hi = src_lo + (composed_hi - composed_lo);
        let Some(spans) = ex.handle.highlight_lines(src_lo, src_hi) else {
            continue;
        };
        for (i, span_row) in spans.into_iter().enumerate() {
            let dest = (composed_lo - lo) as usize + i;
            if dest < row_count {
                result[dest] = span_row;
            }
        }
    }
    Some(result)
}

/// D.4.d.1.b (2026-05-29): per-pane recompute. Same algorithm
/// the pre-d.1.b `recompute` ran against the top-level
/// active-doc fields, now keyed off a single
/// [`crate::render_state::PaneCellsInputs`]. Visible for tests
/// that want to assert per-pane decisions without driving the
/// aggregate.
///
/// Writes via `pane.matrix` (the per-buffer registry cell), so
/// two panes showing the same buffer share a single output cell
/// and the second pane sees a `CacheHit` against the rebuild
/// the first one already published.
pub fn recompute_pane(
    pane: &crate::render_state::PaneCellsInputs,
    ct: CellTheme<'_>,
    whitespace: &WhitespaceConfig,
) -> WorkerDecision {
    let Some(snapshot) = pane.snapshot.as_ref() else {
        // No snapshot — buffer closed mid-publish, or no
        // active document for this pane. Clear this pane's
        // matrices if not already empty; idempotent on repeat
        // clears so the second call doesn't churn the Arcs.
        // B2.2: the `DisplayMatrix` is canonical; clear it and the
        // projected cell grid together. Idempotent only when BOTH are
        // already empty (a fresh display cell with a stale projected
        // cell, or vice versa, must still clear the non-empty one).
        let display_empty = {
            let e = pane.display_matrix.load();
            e.is_empty() && e.version == MatrixVersion::ZERO
        };
        let cell_empty = {
            let e = pane.matrix.load();
            e.is_empty() && e.version == MatrixVersion::ZERO
        };
        if display_empty && cell_empty {
            return WorkerDecision::Clear;
        }
        pane.display_matrix
            .store(Arc::new(crate::display_matrix::DisplayMatrix::empty()));
        pane.matrix.store(Arc::new(CellMatrix::empty()));
        return WorkerDecision::Clear;
    };

    // W.2 (A2): effective wrap width. `0` ⇒ wrapping off (one
    // display row per source line). Stamped onto every matrix this
    // function publishes so consumers (host scroll model + both
    // renderers) derive display geometry from it. Wrap geometry is
    // versioned directly here rather than through `MatrixVersion`:
    // a wrap toggle / pane-width change leaves the content version
    // unchanged (A2 keeps identical rows) but must still re-stamp
    // the matrix, so the cache-hit check below compares
    // `wrap_width` alongside the version.
    let effective_wrap = if pane.wrap { pane.viewport_width } else { 0 };

    // H.3 (2026-06-04): the source-line range the renderer needs
    // covered this tick. For a windowed large-file matrix the
    // cache-hit gate must also confirm the published matrix still
    // covers this range — otherwise a pure scroll past the window
    // edge (no version change) would wrongly short-circuit and leave
    // the new viewport painting plain-text fallback forever.
    //
    // Clamp the upper bound to the doc's line count: a viewport taller
    // than the file (short buffers, big terminals) needs coverage only
    // through EOF, not past it — otherwise `covers` would spuriously
    // fail against a whole-doc matrix whose `covered_end_line` is the
    // (smaller) line count and reject valid cache hits / incremental
    // results.
    let coverage_line_count = snapshot.buffer.line_count();
    let visible_lo = pane.scroll.min(coverage_line_count);
    let visible_hi = pane
        .scroll
        .saturating_add(pane.viewport_height)
        .min(coverage_line_count);

    // B2.2 (2026-06-04): the `DisplayMatrix` is now the canonical
    // build; the `CellMatrix` is a projection of it
    // (`display_matrix_to_cell_matrix`) feeding the not-yet-cut-over
    // renderers (GPU until B3) until B4 deletes the cell path. Cache
    // hit / incremental / full all gate on the canonical matrix; the
    // projection runs only when the canonical matrix is rebuilt.
    //
    // Cache hit: the published canonical matrix already reflects the
    // inputs AND still covers the viewport (H.3). The cell projection
    // from the prior tick is still valid (nothing changed), so we
    // touch neither cell.
    let existing = pane.display_matrix.load_full();
    if !pane.version.differs_from(&existing.version)
        && existing.wrap_width == effective_wrap
        && existing.covers(visible_lo, visible_hi)
    {
        // B2.3 (2026-06-04): the canonical `DisplayMatrix` is already
        // current for these inputs — typically because the actor rebuilt
        // it synchronously in the publish tail
        // ([`sync_rebuild_pane_on_edit`]) so `version.text` never lags the
        // snapshot. That sync path deliberately does NOT project to the
        // cell grid (the O(window) projection stays off the edit-critical
        // actor thread per the B2 threading guarantee), so the projected
        // cells may still be a frame behind. Reconcile them here, off the
        // actor thread, for the not-yet-cut-over cell renderers (GPU until
        // B3). When the projection already matches, it is a true cache hit
        // and no paint wake fires.
        let cells_current = {
            let cm = pane.matrix.load();
            cm.version == existing.version
                && cm.wrap_width == existing.wrap_width
                && cm.covers(visible_lo, visible_hi)
        };
        if cells_current {
            return WorkerDecision::CacheHit;
        }
        let cells = display_matrix_to_cell_matrix(&existing, ct);
        pane.matrix.store(Arc::new(cells));
        return WorkerDecision::Recomputed;
    }

    // Incremental rebuild (S2.4.b semantics, DisplayLine payload).
    // Eligibility is checked inside `try_incremental_display_build`;
    // `None` falls through to a full windowed rebuild. The worker path
    // keeps full syntax colour (`allow_highlight: true`); the sync edit
    // path forces it off.
    let rebuilt = try_incremental_display_build(&existing, snapshot.as_ref(), pane, ct, whitespace, true)
        .and_then(|mut dm| {
            dm.wrap_width = effective_wrap;
            // H.3: accept the incremental result only if it still
            // covers the viewport; a same-tick scroll past the window
            // edge falls through to a recentred full rebuild.
            dm.covers(visible_lo, visible_hi)
                .then_some((dm, WorkerDecision::RecomputedIncremental))
        });

    let (matrix, decision) = match rebuilt {
        Some((dm, decision)) => (dm, decision),
        None => {
            // Full rebuild fallback — windowed around `pane.scroll`
            // in chunked mode (H.3).
            let mut dm = build_display_matrix(
                snapshot.as_ref(),
                pane.syntax_handle.as_deref(),
                &pane.excerpt_syntax,
                ct,
                &pane.inlay_hints,
                &pane.folds,
                pane.foldenable,
                pane.viewport_height,
                pane.scroll,
                pane.version,
                whitespace,
            );
            dm.wrap_width = effective_wrap;
            (dm, WorkerDecision::Recomputed)
        }
    };

    // Project the canonical matrix to the cell grid (the transient
    // B2→B4 bridge), then publish both. Store the cell projection
    // first so a renderer reading `pane.matrix` after seeing the new
    // `pane.display_matrix` never observes a stale cell grid.
    let cells = display_matrix_to_cell_matrix(&matrix, ct);
    pane.matrix.store(Arc::new(cells));
    pane.display_matrix.store(Arc::new(matrix));
    decision
}

/// B2.3 (2026-06-04): the synchronous, edit-path-only `DisplayMatrix`
/// rebuild the actor runs in the publish tail
/// ([`crate::dispatch`]'s `publish_render_state`) **before** the render
/// state is stored, so the published `display_matrix` is text-current the
/// instant the renderer paints — `version.text` never lags the snapshot,
/// which is what retires the per-keystroke whole-viewport stale-guard
/// flicker.
///
/// Honours the B2 threading guarantee (CLAUDE.md / the slice plan): the
/// edit-critical thread does ONLY O(window) text + structure work —
/// prefix/suffix `DisplayLine` `Arc`-reuse plus the edited line(s)' text
/// rebuild — and never a `highlight_lines` call, reparse, or full O(file)
/// build. Concretely it attempts ONLY [`try_incremental_display_build`]
/// with `allow_highlight: false`:
///
/// - **Eligible** (single text edit, unchanged inlay/fold/theme axes,
///   line-count consistent, stable chunk shape, result still covers the
///   viewport) → store the rebuilt matrix into `pane.display_matrix` and
///   return `true`. The edited line shows default fg until the async
///   worker recolours it; unchanged lines keep their colour via `Arc`
///   reuse.
/// - **Ineligible** (non-edit publish, doc switch, chunk-shape change,
///   same-tick window miss) → leave `pane.display_matrix` untouched and
///   return `false`. The async worker performs the full / highlighted
///   build off-thread.
///
/// Deliberately does NOT project to the cell grid: that O(window)
/// projection stays on the async worker ([`recompute_pane`] reconciles the
/// lagging cells on its next wake). Cells therefore trail the display
/// matrix by one worker tick until the renderers cut over to
/// `DisplayMatrix` (TUI B2.4, GPU B3) and the cell path is deleted (B4).
pub fn sync_rebuild_pane_on_edit(
    pane: &crate::render_state::PaneCellsInputs,
    ct: CellTheme<'_>,
    whitespace: &WhitespaceConfig,
) -> bool {
    let Some(snapshot) = pane.snapshot.as_ref() else {
        return false;
    };
    // Match `recompute_pane`'s wrap + coverage model so an accepted sync
    // result is one the worker treats as a cache hit (no redundant
    // rebuild) on its following wake.
    let effective_wrap = if pane.wrap { pane.viewport_width } else { 0 };
    let coverage_line_count = snapshot.buffer.line_count();
    let visible_lo = pane.scroll.min(coverage_line_count);
    let visible_hi = pane
        .scroll
        .saturating_add(pane.viewport_height)
        .min(coverage_line_count);

    let existing = pane.display_matrix.load_full();
    let result =
        try_incremental_display_build(&existing, snapshot.as_ref(), pane, ct, whitespace, false);
    let Some(mut matrix) = result else {
        return false;
    };
    matrix.wrap_width = effective_wrap;
    if !matrix.covers(visible_lo, visible_hi) {
        // Same-tick scroll past the window edge — let the async worker do
        // the recentred (windowed) full rebuild off-thread.
        return false;
    }
    pane.display_matrix.store(Arc::new(matrix));
    true
}

/// S2.4.b: attempt an incremental rebuild from the previously-
/// published `matrix` and the current `cells` substate. Returns
/// `Some(new_matrix)` when the fast path is eligible; `None`
/// otherwise. The caller falls back to a full rebuild on `None`.
///
/// Eligibility requires *all* of:
/// - `cells.last_edit` is `Some(delta)` (single text edit since
///   the last publish, set by `publish_document_changed` and
///   `take()`n at `build_render_state` time);
/// - the published matrix has at least one chunk (no prior
///   matrix → nothing to reuse);
/// - exactly the `text` and `syntax` axes of `MatrixVersion`
///   differ (other axes — `inlay_hints`, `folds`, `theme` —
///   are unchanged; any other axis would invalidate parts of
///   the cached cell content);
/// - the post-edit line count consistency check holds
///   (`published.source_line_count + (added - removed) ==
///   snapshot.line_count()`) — guards against silent corruption
///   when the matrix is for a different document;
/// - the chunked-mode shape doesn't change between pre and post
///   edit (same whole-doc vs chunked decision, same `chunk_size`).
///
/// On success the new matrix is composed by walking the published
/// matrix's chunks and:
/// - **Prefix chunks** (entirely below `edit.start_line`) are
///   cloned by `Arc` (zero work, refcount bump only).
/// - **Affected chunks** (those whose covered range intersects
///   the edit's affected range) rebuild via [`build_chunk_rows`].
/// - **Suffix chunks** (entirely past `edit.pre_edit_end_line()`)
///   are shifted via [`CellChunk::shifted_by`] — cell payloads
///   shared, `source_line` advances by `edit.net_delta()`.
///
/// Chunks at the chunked-mode boundary that contain the affected
/// range may need merging with adjacent rebuilt-zone content;
/// this implementation rebuilds any chunk whose covered range
/// touches the affected range to keep the logic simple and
/// correct.
// B2.2 (2026-06-04): superseded by `try_incremental_display_build`;
// kept as the cell-path parity oracle, deleted in B4.
#[allow(dead_code)]
fn try_incremental_build(
    published: &CellMatrix,
    snapshot: &lattice_runtime::DocumentSnapshot,
    pane: &crate::render_state::PaneCellsInputs,
    ct: CellTheme<'_>,
    whitespace: &WhitespaceConfig,
) -> Option<CellMatrix> {
    let edit = pane.last_edit?;
    if published.chunks.is_empty() {
        return None;
    }

    let new_version = pane.version;
    let pub_v = published.version;

    // Only text / syntax axes may differ here. The text axis is the
    // document version; the syntax axis is the syntax-snapshot version
    // (2026-06-03) — they bump independently, because the async reparse
    // can land after the edit and must invalidate the cache on its own.
    if new_version.inlay_hints != pub_v.inlay_hints
        || new_version.folds != pub_v.folds
        || new_version.theme != pub_v.theme
    {
        return None;
    }
    if new_version.text == pub_v.text && new_version.syntax == pub_v.syntax {
        // No actual content delta — would be cache-hit territory.
        return None;
    }

    // Line-count consistency check guards against doc switches
    // where versions coincidentally line up.
    let new_line_count = snapshot.buffer.line_count();
    let pre_count = published.source_line_count as i64;
    let expected_new = pre_count + edit.net_delta() as i64;
    if expected_new < 0 || expected_new as u32 != new_line_count {
        return None;
    }

    // Chunked-mode shape must be unchanged. Whole-doc → whole-doc
    // and chunked(n) → chunked(n) both qualify; any cross-shape
    // transition forces a full rebuild.
    let new_mode = pick_chunk_size(pane.viewport_height, new_line_count);
    let new_chunk_size = match new_mode {
        ChunkMode::WholeDoc => CHUNK_SIZE_WHOLE_DOC,
        ChunkMode::Chunked(n) => n,
    };
    if new_chunk_size != published.chunk_size {
        return None;
    }

    // Edit-affected ranges in pre- and post-edit line space.
    // `post_hi = edit.post_edit_end_line()` is unused in the
    // current partitioning (rebuild_hi is derived from the first
    // suffix chunk's post-edit start); kept conceptually here for
    // readers tracing the design doc but not bound.
    let edit_lo = edit.start_line;
    let pre_hi = edit.pre_edit_end_line();
    let net = edit.net_delta();

    let (default_fg, default_flags) = resolve_style(ct, lattice_syntax::Style::Default);
    let inlay_fg = inlay_hint_fg();
    let inlays_by_line = bucket_inlays_by_line(&pane.inlay_hints, new_line_count);
    let fold_index = crate::folds::FoldIndex::from_folds(&pane.folds, pane.foldenable);
    // H.1 (2026-06-04): highlight only the line range a rebuild actually
    // touches, not the whole file. Returns spans indexed RELATIVE to `lo`
    // (so the `ChunkInputs.spans_base` for the build is `lo`). `None` ⇒
    // syntax stale/absent → rows fall back to default fg. This is what keeps
    // per-keystroke highlight O(edit) instead of O(file) — and collapses the
    // compose `cells_stale` plain-text window that read as a flicker.
    let highlight_range = |lo: u32, hi: u32| -> Option<Vec<Vec<lattice_syntax::StyledSpan>>> {
        // K.4.7: multibuffer panes carry per-excerpt handles; use them
        // when present. Falls back to the single-doc path when empty.
        if let Some(spans) = highlight_range_multibuffer(&pane.excerpt_syntax, lo, hi) {
            return Some(spans);
        }
        if hi <= lo {
            return Some(Vec::new());
        }
        pane.syntax_handle.as_deref().and_then(|h| {
            let snap = h.snapshot();
            if snap.text_version() < snapshot.text_version {
                return None;
            }
            snap.highlight_lines(lo, hi).ok()
        })
    };

    if new_chunk_size == CHUNK_SIZE_WHOLE_DOC {
        // Whole-doc mode is one chunk. 2026-06-04: reuse the prior
        // chunk's ROWS for unchanged lines (prefix + suffix), rebuilding
        // only the affected range — the row-level analogue of the
        // chunked prefix/suffix reuse below. A wholesale
        // `build_chunk_rows(0, count)` here recoloured EVERY line from
        // `per_line_spans`, which lags right after an edit (the async
        // reparse hasn't landed → stale gate returns `None` → colourless
        // cells). So a small file (whole-doc mode) blanked its whole
        // viewport on each keystroke until syntax caught up — invisible
        // for fast single-layer grammars (Rust) but a visible stutter
        // for markdown's slower injection reparse. Reusing prior rows
        // keeps their colours through the window; only the edited
        // line(s) recolour. See `feedback_decorations_update_in_place`.
        let Some(prior) = published.chunks.first() else {
            let spans = highlight_range(0, new_line_count);
            let inputs = ChunkInputs {
                snapshot,
                per_line_spans: spans.as_ref(),
                spans_base: 0,
                inlays_by_line: &inlays_by_line,
                fold_index: &fold_index,
                ct,
                default_fg,
                default_flags,
                inlay_fg,
                whitespace,
            };
            let rows = build_chunk_rows(&inputs, 0, new_line_count);
            let chunk = Arc::new(CellChunk::new(0, rows, new_version));
            return Some(CellMatrix::whole_doc(chunk, new_line_count));
        };
        let prior_rows = &prior.rows;
        // Affected upper bound = the first suffix row's POST-edit start
        // (mirrors the chunked `rebuild_hi`). Computed from the actual
        // first prior row at/after `pre_hi` so folds (non-contiguous
        // source lines) don't throw off the boundary; `None` ⇒ the edit
        // reached EOF and the rebuild zone runs to `new_line_count`.
        let affected_hi = prior_rows
            .iter()
            .map(|r| r.source_line)
            .find(|&l| l >= pre_hi)
            .map(|l| (l as i64 + net as i64).max(edit_lo as i64) as u32)
            .unwrap_or(new_line_count)
            .min(new_line_count);
        let mut rows: Vec<CellRow> = Vec::with_capacity(prior_rows.len() + 2);
        // Prefix: unchanged lines before the edit — reuse verbatim.
        rows.extend(
            prior_rows
                .iter()
                .filter(|r| r.source_line < edit_lo)
                .cloned(),
        );
        // Affected zone: rebuilt (the only lines that recolour) — highlight
        // scoped to exactly this range (H.1).
        let spans = highlight_range(edit_lo, affected_hi);
        let inputs = ChunkInputs {
            snapshot,
            per_line_spans: spans.as_ref(),
            spans_base: edit_lo,
            inlays_by_line: &inlays_by_line,
            fold_index: &fold_index,
            ct,
            default_fg,
            default_flags,
            inlay_fg,
            whitespace,
        };
        rows.extend(build_chunk_rows(&inputs, edit_lo, affected_hi));
        // Suffix: lines past the edit — reuse with shifted source line.
        rows.extend(
            prior_rows
                .iter()
                .filter(|r| r.source_line >= pre_hi)
                .map(|r| r.with_source_line((r.source_line as i64 + net as i64).max(0) as u32)),
        );
        let chunk = Arc::new(CellChunk::new(0, rows, new_version));
        return Some(CellMatrix::whole_doc(chunk, new_line_count));
    }

    // Chunked-mode incremental rebuild. Partition the published
    // chunks into three regions whose post-edit projections are
    // mutually exclusive and contiguous:
    //
    // 1. Prefix-reuse: chunks fully before the edit
    //    (chunk_end <= edit_lo). Their cells *and* logical-line
    //    positions are unchanged; clone the `Arc<CellChunk>`
    //    verbatim (one refcount bump).
    // 2. Rebuild zone: post-edit lines from the prefix's high-
    //    water-mark up to (but not including) the first suffix
    //    chunk's post-edit start. Materialised via
    //    `build_chunk_rows` in `chunk_size`-aligned buckets.
    // 3. Suffix-shift: chunks fully past the edit
    //    (chunk_start >= pre_hi). Their `start_source_line` and
    //    every row's `source_line` shift by `net`; cell payloads
    //    are shared via row-level `Arc` refcount bumps.
    //
    // Chunks straddling the affected range fall into the rebuild
    // zone implicitly — they are not picked up by either prefix
    // or suffix and the rebuild loop covers their post-edit
    // footprint.
    let chunk_size = new_chunk_size;
    let mut new_chunks: Vec<Arc<CellChunk>> = Vec::with_capacity(published.chunks.len() + 2);

    // --- Step 1: prefix-reuse ---
    let mut rebuild_lo: u32 = 0;
    for chunk in published.chunks.iter() {
        let chunk_end = chunk.start_source_line.saturating_add(published.chunk_size);
        if chunk_end <= edit_lo {
            new_chunks.push(Arc::clone(chunk));
            rebuild_lo = chunk_end;
        } else {
            // chunks are sorted by start_source_line; the rest
            // overlap or are past the edit.
            break;
        }
    }

    // --- Step 2: suffix-shift ---
    let mut suffix_chunks: Vec<Arc<CellChunk>> = Vec::new();
    for chunk in published.chunks.iter() {
        if chunk.start_source_line >= pre_hi {
            suffix_chunks.push(Arc::new(chunk.shifted_by(net, new_version)));
        }
    }
    // First suffix chunk's post-edit start anchors the upper bound of the
    // rebuild zone. When no suffix chunks remain (the edit fell in the last
    // covered chunk) the zone stops at the published window's covered end
    // shifted by `net`, NOT `new_line_count` — otherwise a windowed
    // large-file matrix rebuilds O(file) rows on an edit near the top. See
    // the matching fix + rationale in `try_incremental_display_build`. (Dead
    // parity oracle; mirrored to stay honest until B4 deletes the cell path.)
    let rebuild_hi = suffix_chunks.first().map(|c| c.start_source_line).unwrap_or_else(|| {
        ((published.covered_end_line() as i64 + net as i64).max(rebuild_lo as i64) as u32)
            .min(new_line_count)
    });

    // --- Step 3: rebuild zone ---
    // Carve [rebuild_lo, rebuild_hi) into `chunk_size`-aligned
    // chunks. The final chunk may be ragged if `rebuild_hi` falls
    // mid-chunk — that's expected when suffix-shift produces a
    // chunk at a non-aligned start (e.g. an insert shifts an old
    // chunk-aligned start by `net != 0`). The matrix invariant is
    // contiguous ordered chunks, not uniform sizing — the renderer
    // walks via `chunk.rows.iter()` so any ragged tail is fine.
    // H.1: highlight scoped to the rebuild zone [rebuild_lo, rebuild_hi),
    // indexed relative to `rebuild_lo` — not the whole file.
    let spans = highlight_range(rebuild_lo, rebuild_hi);
    let inputs = ChunkInputs {
        snapshot,
        per_line_spans: spans.as_ref(),
        spans_base: rebuild_lo,
        inlays_by_line: &inlays_by_line,
        fold_index: &fold_index,
        ct,
        default_fg,
        default_flags,
        inlay_fg,
        whitespace,
    };
    let mut cur = rebuild_lo;
    while cur < rebuild_hi {
        let end = cur.saturating_add(chunk_size).min(rebuild_hi);
        let rows = build_chunk_rows(&inputs, cur, end);
        new_chunks.push(Arc::new(CellChunk::new(cur, rows, new_version)));
        cur = end;
    }

    // --- Append suffix chunks ---
    new_chunks.extend(suffix_chunks);

    Some(CellMatrix::chunked(
        new_chunks,
        chunk_size,
        new_line_count,
        new_version,
    ))
}

/// Build a [`CellMatrix`] from `snapshot` + optional syntax
/// handle + theme + inlay-hint payload + folds. Picks whole-doc
/// vs chunked mode based on `viewport_height` and the snapshot's
/// line count.
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
/// **Mode selection (S2.4.a)**: when `viewport_height == 0` or
/// `line_count <= 4 × viewport_height`, the matrix is a single
/// whole-doc chunk. Otherwise it splits into
/// `next_pow2(2 × viewport_height)`-line chunks (clamped to a
/// 16-line floor). Every chunk carries the same publisher
/// `MatrixVersion` for now — S2.4.b will reuse unaffected chunks
/// by per-chunk version comparison once edit-range plumbing
/// arrives.
///
/// Stale-syntax behaviour: if the syntax snapshot's
/// `text_version` is behind the document's `text_version`, the
/// snapshot's byte offsets no longer align with the current rope
/// and re-styling against them would mis-colour edits. The worker
/// falls back to the default fg for the whole document in that
/// case. The matrix rebuilds again when the syntax catches up
/// (the cascade bumps `MatrixVersion::syntax`, which equals the
/// document's `text_version` at publish time).
// B2.2 (2026-06-04): superseded in production by `build_display_matrix`
// + `display_matrix_to_cell_matrix`. Retained as the parity oracle for
// the projection tests; deleted with the cell path in B4.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
fn build_matrix(
    snapshot: &lattice_runtime::DocumentSnapshot,
    syntax_handle: Option<&lattice_syntax::SyntaxHandle>,
    ct: CellTheme<'_>,
    inlay_hints: &[crate::render_state::InlayHintRow],
    folds: &[lattice_core::Fold],
    foldenable: bool,
    viewport_height: u32,
    scroll: u32,
    version: MatrixVersion,
    whitespace: &WhitespaceConfig,
) -> CellMatrix {
    let line_count = snapshot.buffer.line_count();
    if line_count == 0 {
        return CellMatrix::empty();
    }

    let (default_fg, default_flags) = resolve_style(ct, lattice_syntax::Style::Default);
    let inlay_fg = inlay_hint_fg();

    let inlays_by_line = bucket_inlays_by_line(inlay_hints, line_count);
    let fold_index = crate::folds::FoldIndex::from_folds(folds, foldenable);

    // H.3 (2026-06-04): highlight only the line range a build actually
    // covers — the whole file in whole-doc mode, the viewport window in
    // chunked mode — not the whole file unconditionally. Mirrors the
    // `highlight_range` closure in `try_incremental_build`. Returns spans
    // indexed RELATIVE to `lo` (so the build's `spans_base` is `lo`).
    // `None` ⇒ syntax stale/absent → rows fall back to default fg, and the
    // matrix rebuilds when the syntax catches up (the cascade bumps
    // `MatrixVersion::syntax`).
    let highlight_range = |lo: u32, hi: u32| -> Option<Vec<Vec<lattice_syntax::StyledSpan>>> {
        if hi <= lo {
            return Some(Vec::new());
        }
        syntax_handle.and_then(|h| {
            let snap = h.snapshot();
            // Stale snapshot — don't paint with mismatched offsets.
            if snap.text_version() < snapshot.text_version {
                return None;
            }
            snap.highlight_lines(lo, hi).ok()
        })
    };

    match pick_chunk_size(viewport_height, line_count) {
        ChunkMode::WholeDoc => {
            let spans = highlight_range(0, line_count);
            let inputs = ChunkInputs {
                snapshot,
                per_line_spans: spans.as_ref(),
                spans_base: 0,
                inlays_by_line: &inlays_by_line,
                fold_index: &fold_index,
                ct,
                default_fg,
                default_flags,
                inlay_fg,
                whitespace,
            };
            let rows = build_chunk_rows(&inputs, 0, line_count);
            let chunk = Arc::new(CellChunk::new(0, rows, version));
            CellMatrix::whole_doc(chunk, line_count)
        }
        ChunkMode::Chunked(chunk_size) => {
            // H.3: window the chunked build around the viewport (above
            // `WINDOW_CAP_LINES`; full coverage at/below it). Highlight +
            // row materialisation are scoped to `[win_lo, win_hi)`, so the
            // build is O(window) on large files. `source_line_count` stays
            // the true doc count so `row_at_source_line` bounds + scroll
            // geometry are unchanged; off-window lines simply have no chunk
            // → `None` → the renderers' existing plain-text/legacy-span
            // fallback.
            let (win_lo, win_hi) = window_bounds(scroll, viewport_height, line_count, chunk_size);
            let spans = highlight_range(win_lo, win_hi);
            let inputs = ChunkInputs {
                snapshot,
                per_line_spans: spans.as_ref(),
                spans_base: win_lo,
                inlays_by_line: &inlays_by_line,
                fold_index: &fold_index,
                ct,
                default_fg,
                default_flags,
                inlay_fg,
                whitespace,
            };
            let mut chunks: Vec<Arc<CellChunk>> =
                Vec::with_capacity(((win_hi - win_lo) / chunk_size + 1) as usize);
            let mut start = win_lo;
            while start < win_hi {
                let end = start.saturating_add(chunk_size).min(win_hi);
                let rows = build_chunk_rows(&inputs, start, end);
                chunks.push(Arc::new(CellChunk::new(start, rows, version)));
                start = end;
            }
            CellMatrix::chunked(chunks, chunk_size, line_count, version)
        }
    }
}

/// Mode selection for [`build_matrix`]. `WholeDoc` collapses to a
/// single chunk covering the entire document; `Chunked(n)` builds
/// `n`-line chunks. Switching point and chunk size match the
/// design doc (`docs/dev/architecture/cell-grid-renderer.md` §
/// Chunking policy).
#[derive(Debug, PartialEq, Eq)]
enum ChunkMode {
    WholeDoc,
    Chunked(u32),
}

/// Pick the chunk mode. Whole-doc when:
/// - `viewport_height == 0` (boot / no layout yet), or
/// - `line_count <= 4 × viewport_height` (small-doc threshold).
///
/// Chunked otherwise. `chunk_size = next_pow2(2 × viewport_height)`,
/// clamped to a 16-line floor so tiny viewports don't produce
/// per-line chunks. The power-of-two snap is intentional — it
/// keeps the chunk-table cache-friendly and makes the eventual
/// LRU eviction policy in the design doc trivial to reason about.
fn pick_chunk_size(viewport_height: u32, line_count: u32) -> ChunkMode {
    if viewport_height == 0 || line_count <= viewport_height.saturating_mul(4) {
        return ChunkMode::WholeDoc;
    }
    let target = viewport_height.saturating_mul(2).max(16);
    ChunkMode::Chunked(next_power_of_two(target))
}

/// Smallest `u32` power of two `≥ n`. `n == 0` returns 1 (the
/// minimum non-zero power of two); inputs near `u32::MAX` saturate
/// at `1 << 31` to avoid overflow.
fn next_power_of_two(n: u32) -> u32 {
    if n <= 1 {
        return 1;
    }
    let leading = (n - 1).leading_zeros();
    if leading == 0 {
        1u32 << 31
    } else {
        1u32 << (32 - leading)
    }
}

/// H.3 (2026-06-04): line-count cap above which a chunked-mode
/// matrix is *windowed* around the viewport rather than covering
/// the whole document.
///
/// Below the cap a full-residency chunked build is cheap (~2K lines
/// of cells is a couple of MB) and — crucially — avoids paying a
/// window rebuild every time the user scrolls past the covered
/// range, which on a small doc is pure waste. Above the cap, the
/// matrix tracks `[scroll − overscan, scroll + viewport + overscan)`
/// so build, rebuild, and memory all stay O(viewport) on large
/// files (paramount goal #1) — the headline large-file win.
///
/// The cap sits well above any realistic "small file" yet below the
/// 10k+-line scale where an O(file) cell build visibly stutters. All
/// pre-H.3 chunked-mode tests use docs far below it, so they keep
/// the full-coverage behaviour they assert.
const WINDOW_CAP_LINES: u32 = 2048;

/// H.3: the source-line range a chunked build covers for a given
/// `scroll` / `viewport_height`. Returns the whole document
/// `(0, line_count)` at or below [`WINDOW_CAP_LINES`] (full
/// residency); above it, a `chunk_size`-aligned window
/// `[scroll − overscan, scroll + viewport_height + overscan)` clamped
/// to `[0, line_count)`.
///
/// Overscan is one `viewport_height` on each side: combined with the
/// chunk-aligned bounds, line-by-line scrolling stays inside the
/// covered range (worker returns `CacheHit`); only a jump that
/// crosses the window edge triggers a rebuild, and that rebuild is
/// O(window). `chunk_size` is `> 0` here (chunked mode only;
/// whole-doc mode never calls this).
fn window_bounds(scroll: u32, viewport_height: u32, line_count: u32, chunk_size: u32) -> (u32, u32) {
    if line_count <= WINDOW_CAP_LINES {
        return (0, line_count);
    }
    let overscan = viewport_height;
    let raw_lo = scroll.saturating_sub(overscan);
    let raw_hi = scroll
        .saturating_add(viewport_height)
        .saturating_add(overscan)
        .min(line_count);
    // Align lo down and hi up to chunk boundaries so chunks stay
    // chunk-aligned (matching the full-coverage build) and a small
    // scroll lands inside the already-built window.
    let lo = (raw_lo / chunk_size) * chunk_size;
    let hi = raw_hi
        .saturating_add(chunk_size - 1)
        .saturating_div(chunk_size)
        .saturating_mul(chunk_size)
        .min(line_count);
    (lo, hi)
}

/// Inputs shared across all chunks of a single matrix build. Held
/// by reference so the orchestrator can call `build_chunk_rows`
/// repeatedly without cloning.
struct ChunkInputs<'a> {
    snapshot: &'a lattice_runtime::DocumentSnapshot,
    /// Per-line highlight spans for the range `[spans_base, spans_base + len)`,
    /// indexed RELATIVE to `spans_base` (H.1, 2026-06-04). The highlight is
    /// scoped to exactly the range a rebuild touches — whole-file no longer —
    /// so `build_chunk_rows` looks up `per_line_spans[line_idx - spans_base]`.
    /// `None` ⇒ syntax unavailable/stale; every row falls back to default fg.
    per_line_spans: Option<&'a Vec<Vec<lattice_syntax::StyledSpan>>>,
    /// Absolute source line that `per_line_spans[0]` corresponds to.
    spans_base: u32,
    inlays_by_line: &'a std::collections::HashMap<u32, Vec<(u32, &'a str)>>,
    fold_index: &'a crate::folds::FoldIndex,
    ct: CellTheme<'a>,
    default_fg: u32,
    /// S3.a: modifier flags from `theme.syntax_style(Default)` so
    /// cells outside any styled span pick up the theme's default
    /// modifiers (typically none, but cheaply propagated).
    default_flags: u16,
    inlay_fg: u32,
    /// 2026-05-27: passed through to `build_row_cells` for the
    /// whitespace-marker substitution. Empty (`show: false`) when
    /// the user hasn't enabled `display.show_whitespace`.
    whitespace: &'a WhitespaceConfig,
}

/// Build the row vector for one chunk covering source lines
/// `[start_line, end_line)`. Folded interior lines are skipped
/// (see `line_inside_closed_fold`); surviving rows keep their
/// logical `source_line`.
// B2.2: cell-path row builder; superseded by `build_display_rows`
// (+ projection). Kept as a parity oracle; deleted in B4.
#[allow(dead_code)]
fn build_chunk_rows(inputs: &ChunkInputs, start_line: u32, end_line: u32) -> Vec<CellRow> {
    let mut rows: Vec<CellRow> = Vec::with_capacity((end_line - start_line) as usize);
    for line_idx in start_line..end_line {
        if inputs.fold_index.line_inside_closed_fold(line_idx) {
            continue;
        }
        let text = inputs.snapshot.buffer.line(line_idx).unwrap_or_default();
        let line_spans: &[lattice_syntax::StyledSpan] = inputs
            .per_line_spans
            .and_then(|v| {
                let rel = line_idx.checked_sub(inputs.spans_base)?;
                v.get(rel as usize)
            })
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let line_inlays = inputs
            .inlays_by_line
            .get(&line_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (cells, inlay_offsets) = build_row_cells(
            &text,
            line_spans,
            line_inlays,
            inputs.ct,
            inputs.default_fg,
            inputs.default_flags,
            inputs.inlay_fg,
            inputs.whitespace,
        );
        rows.push(CellRow::new(cells, line_idx, inlay_offsets));
    }
    rows
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
// B2.2: cell-path per-row builder; superseded by `build_display_row`
// (+ `display_line_to_cell_row`). Kept as the parity oracle the
// `display_build_parity_*` tests compare against; deleted in B4.
#[allow(dead_code)]
fn build_row_cells(
    text: &str,
    line_spans: &[lattice_syntax::StyledSpan],
    line_inlays: &[(u32, &str)],
    ct: CellTheme<'_>,
    default_fg: u32,
    default_flags: u16,
    inlay_fg: u32,
    ws: &WhitespaceConfig,
) -> (Vec<Cell>, Vec<lattice_cells::row::InlayOffset>) {
    // Capacity: source chars + sum of inlay char widths. Slight
    // over-estimate is fine.
    let inlay_total_chars: usize = line_inlays.iter().map(|(_, t)| t.chars().count()).sum();
    let mut cells: Vec<Cell> = Vec::with_capacity(text.len() + inlay_total_chars);
    let mut inlay_offsets: Vec<lattice_cells::row::InlayOffset> =
        Vec::with_capacity(line_inlays.len());

    // Resolve `Style → (fg, flags)`. `Style::Default` returns the
    // pre-resolved defaults so callers can avoid the per-cell theme
    // lookup on the hot path.
    let resolve = |style: lattice_syntax::Style| -> (u32, u16) {
        if matches!(style, lattice_syntax::Style::Default) {
            (default_fg, default_flags)
        } else {
            resolve_style(ct, style)
        }
    };

    // 2026-05-27: pre-scan the line to find leading-end and
    // trailing-start byte positions. `leading_end_byte` is the
    // byte of the first NON-space char (== text.len() for blank
    // lines = all chars trailing). `trailing_start_byte` is the
    // byte AFTER the last NON-space char.
    let mut leading_end_byte = text.len();
    let mut trailing_start_byte = 0;
    if ws.show {
        for (b, c) in text.char_indices() {
            if c != ' ' && c != '\t' {
                if leading_end_byte == text.len() {
                    leading_end_byte = b;
                }
                trailing_start_byte = b + c.len_utf8();
            }
        }
        if leading_end_byte == text.len() {
            // No non-space char on the line — treat every cell as
            // trailing.
            trailing_start_byte = 0;
        }
    }
    // Trailing-style fg (red) when emitting trailing-space markers.
    // Matches TUI's theme.whitespace_trailing_style.
    let trailing_fg = ct
        .resolved
        .get(ct.ids.whitespace_trailing)
        .fg
        .map(|c| c.to_rgb_u32(default_fg))
        .unwrap_or(default_fg);

    let mut inlay_idx = 0usize;
    for (byte, ch) in text.char_indices() {
        // Splice every inlay whose `orig_byte` is at or before this
        // char position. Order-of-arrival ties at the same byte
        // resolve in input order.
        while inlay_idx < line_inlays.len() && (line_inlays[inlay_idx].0 as usize) <= byte {
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
        let (fg, mods) = resolve(style);
        // 2026-05-27 whitespace decoration. When `ws.show` is true
        // and the source char is whitespace, substitute the
        // configured marker glyph and set `WS_MARKER`. Falls
        // through to the verbatim emit when ws is off or no glyph
        // is configured for the position.
        let mut emitted = false;
        // W.4.t: a hard tab ALWAYS expands to its display width (the
        // next multiple of `tabstop`), so no literal `\t` ever
        // reaches the renderers and the cell grid models the tab at
        // full width — one width model the host scroll model and
        // both renderers share. Respects `display.whitespace.tab`:
        // when whitespace is shown and a tab glyph is configured the
        // first column is that marker (WS_MARKER) and the remaining
        // columns are space fill; otherwise the whole run is plain
        // spaces. `tabstop == 0` (the `WhitespaceConfig::default`)
        // falls back to `1`, i.e. the legacy one-cell tab.
        if ch == '\t' {
            let tabstop = ws.tabstop.max(1);
            let col = cells.len() as u32;
            let fill = tabstop - (col % tabstop); // 1..=tabstop
            let is_trailing = byte >= trailing_start_byte;
            let marker = ws.show && ws.tab.is_some();
            let cell_fg = if marker && is_trailing { trailing_fg } else { fg };
            let flags = if marker {
                mods | lattice_cells::cell_flags::WS_MARKER
            } else {
                mods
            };
            let first = if marker { ws.tab.unwrap_or(' ') } else { ' ' };
            cells.push(Cell::new(first as u32, cell_fg, 0, flags));
            for _ in 1..fill {
                cells.push(Cell::new(' ' as u32, cell_fg, 0, flags));
            }
            // Record the 1-source-byte → `fill`-cell expansion so
            // byte↔column mapping (`byte_to_combined_col`, used by
            // overlays + the GPUI cursor) shifts bytes AFTER the tab
            // by the `fill - 1` extra columns.
            if fill > 1 {
                inlay_offsets.push((byte as u32 + 1, fill - 1));
            }
            emitted = true;
        } else if ws.show {
            let is_trailing = byte >= trailing_start_byte;
            let is_leading = byte < leading_end_byte;
            if ch == ' ' {
                let glyph = if is_trailing {
                    ws.trailing
                } else if is_leading {
                    ws.leading.or(ws.space)
                } else {
                    ws.space
                };
                if let Some(g) = glyph {
                    let cell_fg = if is_trailing { trailing_fg } else { fg };
                    cells.push(Cell::new(
                        g as u32,
                        cell_fg,
                        0,
                        mods | lattice_cells::cell_flags::WS_MARKER,
                    ));
                    emitted = true;
                }
            }
        }
        if !emitted {
            cells.push(Cell::new(ch as u32, fg, 0, mods));
        }
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

/// B1 (2026-06-04): the canonical per-line builder for the
/// [`crate::display_matrix::DisplayMatrix`] — the substrate that
/// retires the per-character cell grid. Same display resolution as
/// [`build_row_cells`] (inlay splice, tab expansion to display width,
/// whitespace markers; fold elision is the caller's), but emits
/// style-*tagged* runs over a display string instead of theme-resolved
/// per-character cells. The renderer resolves each run's `style` +
/// `flags` → colour at paint (GPU shapes the string once; no un-bake).
///
/// Returns `(display_text, runs, col_map, col_count)`. `build_row_cells`
/// (the cell path) is deleted in B4; until then both exist and the
/// `display_build_parity_*` tests guard their equivalence so they can't
/// drift. `flags` carries only the non-style bits (`INLAY` / `WS_MARKER`)
/// — modifiers (bold/italic/…) are derived from `style` by the renderer,
/// exactly as the cell projection does.
// B2.2: live — the canonical per-row builder behind
// `build_display_rows` → `build_display_matrix` → `recompute_pane`.
fn build_display_row(
    text: &str,
    line_spans: &[lattice_syntax::StyledSpan],
    line_inlays: &[(u32, &str)],
    ws: &WhitespaceConfig,
) -> (
    Box<str>,
    Vec<crate::display_matrix::DisplayRun>,
    Vec<(u32, u32)>,
    u32,
) {
    use crate::display_matrix::DisplayRun;
    use lattice_cells::cell_flags;

    /// Append `s` under one `(style, flags)`; merge with the last run.
    fn push(
        out: &mut String,
        runs: &mut Vec<DisplayRun>,
        s: &str,
        style: lattice_syntax::Style,
        flags: u16,
    ) {
        if s.is_empty() {
            return;
        }
        let len = s.len() as u32;
        out.push_str(s);
        match runs.last_mut() {
            Some(last) if last.style == style && last.flags == flags => last.len += len,
            _ => runs.push(DisplayRun { len, style, flags }),
        }
    }

    let inlay_total: usize = line_inlays.iter().map(|(_, t)| t.len()).sum();
    let mut out = String::with_capacity(text.len() + inlay_total);
    let mut runs: Vec<DisplayRun> = Vec::new();
    let mut col_map: Vec<(u32, u32)> = Vec::with_capacity(line_inlays.len());
    // Display columns emitted so far (== char count == cell count in the
    // old grid). Drives tab fill the same way `cells.len()` did.
    let mut col: u32 = 0;
    let mut tmp = [0u8; 4];

    // Leading/trailing prescan for whitespace markers (mirrors
    // `build_row_cells`).
    let mut leading_end_byte = text.len();
    let mut trailing_start_byte = 0;
    if ws.show {
        for (b, c) in text.char_indices() {
            if c != ' ' && c != '\t' {
                if leading_end_byte == text.len() {
                    leading_end_byte = b;
                }
                trailing_start_byte = b + c.len_utf8();
            }
        }
        if leading_end_byte == text.len() {
            trailing_start_byte = 0;
        }
    }

    let mut inlay_idx = 0usize;
    for (byte, ch) in text.char_indices() {
        while inlay_idx < line_inlays.len() && (line_inlays[inlay_idx].0 as usize) <= byte {
            let (orig_byte, t) = line_inlays[inlay_idx];
            col_map.push((orig_byte, t.chars().count() as u32));
            push(&mut out, &mut runs, t, lattice_syntax::Style::Default, cell_flags::INLAY);
            col += t.chars().count() as u32;
            inlay_idx += 1;
        }
        let style = style_at_byte(line_spans, byte);
        let mut emitted = false;
        if ch == '\t' {
            let tabstop = ws.tabstop.max(1);
            let fill = tabstop - (col % tabstop);
            let marker = ws.show && ws.tab.is_some();
            // WS_TRAILING mirrors the cell path's `marker && is_trailing`
            // condition for trailing-fg resolution (build_row_cells).
            let is_trailing = byte >= trailing_start_byte;
            let flags = if marker {
                cell_flags::WS_MARKER
                    | if is_trailing { cell_flags::WS_TRAILING } else { 0 }
            } else {
                0
            };
            let first = if marker { ws.tab.unwrap_or(' ') } else { ' ' };
            push(&mut out, &mut runs, first.encode_utf8(&mut tmp), style, flags);
            for _ in 1..fill {
                push(&mut out, &mut runs, " ", style, flags);
            }
            if fill > 1 {
                col_map.push((byte as u32 + 1, fill - 1));
            }
            col += fill;
            emitted = true;
        } else if ws.show && ch == ' ' {
            let is_trailing = byte >= trailing_start_byte;
            let is_leading = byte < leading_end_byte;
            let glyph = if is_trailing {
                ws.trailing
            } else if is_leading {
                ws.leading.or(ws.space)
            } else {
                ws.space
            };
            if let Some(g) = glyph {
                // Space markers take trailing-fg iff trailing (the cell
                // path keys trailing-fg on `is_trailing` for spaces).
                let flags = cell_flags::WS_MARKER
                    | if is_trailing { cell_flags::WS_TRAILING } else { 0 };
                push(
                    &mut out,
                    &mut runs,
                    g.encode_utf8(&mut tmp),
                    style,
                    flags,
                );
                col += 1;
                emitted = true;
            }
        }
        if !emitted {
            push(&mut out, &mut runs, ch.encode_utf8(&mut tmp), style, 0);
            col += 1;
        }
    }
    while inlay_idx < line_inlays.len() {
        let (orig_byte, t) = line_inlays[inlay_idx];
        col_map.push((orig_byte, t.chars().count() as u32));
        push(&mut out, &mut runs, t, lattice_syntax::Style::Default, cell_flags::INLAY);
        col += t.chars().count() as u32;
        inlay_idx += 1;
    }

    (out.into_boxed_str(), runs, col_map, col)
}

/// B1: per-chunk display-row build — the `DisplayLine` analogue of
/// [`build_chunk_rows`]. Folded interior lines are skipped (same
/// `line_inside_closed_fold` semantics); surviving rows keep their
/// logical `source_line`. Highlight spans are looked up relative to
/// `inputs.spans_base` exactly as the cell path does (H.1).
// B2.2: live — called by `build_display_matrix` /
// `try_incremental_display_build`.
fn build_display_rows(
    inputs: &ChunkInputs,
    start_line: u32,
    end_line: u32,
) -> Vec<crate::display_matrix::DisplayLine> {
    use crate::display_matrix::DisplayLine;
    let mut rows: Vec<DisplayLine> = Vec::with_capacity((end_line - start_line) as usize);
    for line_idx in start_line..end_line {
        if inputs.fold_index.line_inside_closed_fold(line_idx) {
            continue;
        }
        let text = inputs.snapshot.buffer.line(line_idx).unwrap_or_default();
        let line_spans: &[lattice_syntax::StyledSpan] = inputs
            .per_line_spans
            .and_then(|v| {
                let rel = line_idx.checked_sub(inputs.spans_base)?;
                v.get(rel as usize)
            })
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let line_inlays = inputs
            .inlays_by_line
            .get(&line_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (text, runs, col_map, col_count) =
            build_display_row(&text, line_spans, line_inlays, inputs.whitespace);
        rows.push(DisplayLine {
            source_line: line_idx,
            text: Arc::from(text),
            runs: Arc::from(runs.into_boxed_slice()),
            col_map: Arc::from(col_map.into_boxed_slice()),
            col_count,
            // Fold-head metadata is wired when the renderers consume
            // `DisplayMatrix` (B2); the cell path carried no fold info
            // on the row either (the renderer computes the fold suffix).
            fold: None,
        });
    }
    rows
}

/// B2.2 (2026-06-04): build a [`crate::display_matrix::DisplayMatrix`]
/// — the `DisplayLine` analogue of [`build_matrix`]. Identical mode
/// selection ([`pick_chunk_size`]), windowing ([`window_bounds`]), and
/// highlight scoping (H.1/H.3); only the per-row payload differs
/// ([`build_display_rows`] instead of [`build_chunk_rows`]). This is the
/// *canonical* build once B2.2b flips `recompute_pane` over; the
/// `CellMatrix` is then a projection ([`display_matrix_to_cell_matrix`]).
#[allow(clippy::too_many_arguments)]
fn build_display_matrix(
    snapshot: &lattice_runtime::DocumentSnapshot,
    syntax_handle: Option<&lattice_syntax::SyntaxHandle>,
    excerpt_syntax: &[crate::render_state::ExcerptSyntax],
    ct: CellTheme<'_>,
    inlay_hints: &[crate::render_state::InlayHintRow],
    folds: &[lattice_core::Fold],
    foldenable: bool,
    viewport_height: u32,
    scroll: u32,
    version: MatrixVersion,
    whitespace: &WhitespaceConfig,
) -> crate::display_matrix::DisplayMatrix {
    use crate::display_matrix::{DisplayChunk, DisplayMatrix};
    let line_count = snapshot.buffer.line_count();
    if line_count == 0 {
        return DisplayMatrix::empty();
    }

    // `default_*` / `inlay_fg` are required to construct `ChunkInputs`
    // (shared with the cell path) even though `build_display_rows` reads
    // only the snapshot / spans / inlays / folds / whitespace fields.
    let (default_fg, default_flags) = resolve_style(ct, lattice_syntax::Style::Default);
    let inlay_fg = inlay_hint_fg();
    let inlays_by_line = bucket_inlays_by_line(inlay_hints, line_count);
    let fold_index = crate::folds::FoldIndex::from_folds(folds, foldenable);

    let highlight_range = |lo: u32, hi: u32| -> Option<Vec<Vec<lattice_syntax::StyledSpan>>> {
        // K.4.7: multibuffer panes use per-excerpt handles.
        if let Some(spans) = highlight_range_multibuffer(excerpt_syntax, lo, hi) {
            return Some(spans);
        }
        if hi <= lo {
            return Some(Vec::new());
        }
        syntax_handle.and_then(|h| {
            let snap = h.snapshot();
            if snap.text_version() < snapshot.text_version {
                return None;
            }
            snap.highlight_lines(lo, hi).ok()
        })
    };

    match pick_chunk_size(viewport_height, line_count) {
        ChunkMode::WholeDoc => {
            let spans = highlight_range(0, line_count);
            let inputs = ChunkInputs {
                snapshot,
                per_line_spans: spans.as_ref(),
                spans_base: 0,
                inlays_by_line: &inlays_by_line,
                fold_index: &fold_index,
                ct,
                default_fg,
                default_flags,
                inlay_fg,
                whitespace,
            };
            let rows = build_display_rows(&inputs, 0, line_count);
            let chunk = Arc::new(DisplayChunk::new(0, rows, version));
            DisplayMatrix::whole_doc(chunk, line_count)
        }
        ChunkMode::Chunked(chunk_size) => {
            let (win_lo, win_hi) = window_bounds(scroll, viewport_height, line_count, chunk_size);
            let spans = highlight_range(win_lo, win_hi);
            let inputs = ChunkInputs {
                snapshot,
                per_line_spans: spans.as_ref(),
                spans_base: win_lo,
                inlays_by_line: &inlays_by_line,
                fold_index: &fold_index,
                ct,
                default_fg,
                default_flags,
                inlay_fg,
                whitespace,
            };
            let mut chunks: Vec<Arc<DisplayChunk>> =
                Vec::with_capacity(((win_hi - win_lo) / chunk_size + 1) as usize);
            let mut start = win_lo;
            while start < win_hi {
                let end = start.saturating_add(chunk_size).min(win_hi);
                let rows = build_display_rows(&inputs, start, end);
                chunks.push(Arc::new(DisplayChunk::new(start, rows, version)));
                start = end;
            }
            DisplayMatrix::chunked(chunks, chunk_size, line_count, version)
        }
    }
}

/// B2.2 (2026-06-04): incremental `DisplayMatrix` rebuild — the
/// `DisplayLine` analogue of [`try_incremental_build`]. Same eligibility
/// gates (single edit, only text/syntax axes differ, line-count
/// consistency, unchanged chunked-mode shape) and the same
/// prefix-reuse / rebuild-zone / suffix-shift partition; only the
/// payload + matrix/chunk types differ. Unchanged `DisplayLine`s are
/// `Arc`-reused byte-identical (pixel-stable; only the edited line
/// recolours), exactly as the cell path does.
///
/// B2.3 (2026-06-04): `allow_highlight` gates whether the rebuild zone
/// is syntax-highlighted. The async worker passes `true` (full colour).
/// The synchronous actor path ([`sync_rebuild_pane_on_edit`]) passes
/// `false` so the rebuild does ZERO `highlight_lines` work on the
/// edit-critical thread — the B2 threading guarantee, enforced rather
/// than relying on the syntax snapshot happening to be stale. The
/// edited line keeps default fg until the async worker recolours it a
/// frame or two later (eventual consistency, within the keystroke UX
/// contract); unchanged lines `Arc`-reuse their prior colour.
fn try_incremental_display_build(
    published: &crate::display_matrix::DisplayMatrix,
    snapshot: &lattice_runtime::DocumentSnapshot,
    pane: &crate::render_state::PaneCellsInputs,
    ct: CellTheme<'_>,
    whitespace: &WhitespaceConfig,
    allow_highlight: bool,
) -> Option<crate::display_matrix::DisplayMatrix> {
    use crate::display_matrix::{DisplayChunk, DisplayLine, DisplayMatrix};
    let edit = pane.last_edit?;
    if published.chunks.is_empty() {
        return None;
    }

    let new_version = pane.version;
    let pub_v = published.version;
    if new_version.inlay_hints != pub_v.inlay_hints
        || new_version.folds != pub_v.folds
        || new_version.theme != pub_v.theme
    {
        return None;
    }
    if new_version.text == pub_v.text && new_version.syntax == pub_v.syntax {
        return None;
    }

    let new_line_count = snapshot.buffer.line_count();
    let pre_count = published.source_line_count as i64;
    let expected_new = pre_count + edit.net_delta() as i64;
    if expected_new < 0 || expected_new as u32 != new_line_count {
        return None;
    }

    let new_mode = pick_chunk_size(pane.viewport_height, new_line_count);
    let new_chunk_size = match new_mode {
        ChunkMode::WholeDoc => CHUNK_SIZE_WHOLE_DOC,
        ChunkMode::Chunked(n) => n,
    };
    if new_chunk_size != published.chunk_size {
        return None;
    }

    let edit_lo = edit.start_line;
    // B2.3 intra-line staleness fix (2026-06-05): the row-reuse partition
    // treats lines `>= pre_edit_end_line()` as the unchanged SUFFIX (reused,
    // shifted by `net`). But `EditDelta` counts only FULL lines added/removed,
    // so a **pure intra-line edit** — the COMMON typing case, inserting or
    // deleting a char without crossing a newline — reports
    // `lines_added == lines_removed == 0`, which makes
    // `pre_edit_end_line() == start_line == the EDITED line`. Reusing it as
    // suffix paints the PRE-edit row while the matrix version is stamped
    // current, so the renderer's per-line staleness fallback never fires and
    // the typed glyph visibly lags the cursor for a frame
    // (`|word` → `w|ord` → ` |word`; felt as "one key behind" because the
    // TUI's 100ms poll only redraws on the NEXT keystroke). For an intra-line
    // edit the edited line MUST be rebuilt, so the reusable suffix starts one
    // past it. Gated to `removed == added == 0`: for a structural edit
    // (`lines_*` > 0) `pre_edit_end_line()` is a genuinely-unchanged line that
    // only SHIFTS — reusing its row (and thus its syntax colour) is correct,
    // and extending the boundary there would needlessly recolour it (a flicker
    // the async worker would have to repaint — feedback_decorations_update_in_place).
    // Boundary-line CONTENT changes from mid-line splits / joins (same
    // `EditDelta` shape as a clean line insert/delete, distinguishable only
    // with intra-line column info) remain a rarer, self-healing follow-up.
    let suffix_lo = if edit.lines_removed == 0 && edit.lines_added == 0 {
        edit.pre_edit_end_line().saturating_add(1)
    } else {
        edit.pre_edit_end_line()
    };
    let net = edit.net_delta();

    let (default_fg, default_flags) = resolve_style(ct, lattice_syntax::Style::Default);
    let inlay_fg = inlay_hint_fg();
    let inlays_by_line = bucket_inlays_by_line(&pane.inlay_hints, new_line_count);
    let fold_index = crate::folds::FoldIndex::from_folds(&pane.folds, pane.foldenable);
    let highlight_range = |lo: u32, hi: u32| -> Option<Vec<Vec<lattice_syntax::StyledSpan>>> {
        // B2.3: sync edit path forces highlight off — no `highlight_lines`
        // call ever lands on the edit-critical actor thread.
        if !allow_highlight {
            return None;
        }
        // K.4.7: multibuffer panes use per-excerpt handles.
        if let Some(spans) = highlight_range_multibuffer(&pane.excerpt_syntax, lo, hi) {
            return Some(spans);
        }
        if hi <= lo {
            return Some(Vec::new());
        }
        pane.syntax_handle.as_deref().and_then(|h| {
            let snap = h.snapshot();
            if snap.text_version() < snapshot.text_version {
                return None;
            }
            snap.highlight_lines(lo, hi).ok()
        })
    };

    if new_chunk_size == CHUNK_SIZE_WHOLE_DOC {
        let Some(prior) = published.chunks.first() else {
            let spans = highlight_range(0, new_line_count);
            let inputs = ChunkInputs {
                snapshot,
                per_line_spans: spans.as_ref(),
                spans_base: 0,
                inlays_by_line: &inlays_by_line,
                fold_index: &fold_index,
                ct,
                default_fg,
                default_flags,
                inlay_fg,
                whitespace,
            };
            let rows = build_display_rows(&inputs, 0, new_line_count);
            let chunk = Arc::new(DisplayChunk::new(0, rows, new_version));
            return Some(DisplayMatrix::whole_doc(chunk, new_line_count));
        };
        let prior_rows = &prior.rows;
        let affected_hi = prior_rows
            .iter()
            .map(|r| r.source_line)
            .find(|&l| l >= suffix_lo)
            .map(|l| (l as i64 + net as i64).max(edit_lo as i64) as u32)
            .unwrap_or(new_line_count)
            .min(new_line_count);
        let mut rows: Vec<DisplayLine> = Vec::with_capacity(prior_rows.len() + 2);
        rows.extend(
            prior_rows
                .iter()
                .filter(|r| r.source_line < edit_lo)
                .cloned(),
        );
        let spans = highlight_range(edit_lo, affected_hi);
        let inputs = ChunkInputs {
            snapshot,
            per_line_spans: spans.as_ref(),
            spans_base: edit_lo,
            inlays_by_line: &inlays_by_line,
            fold_index: &fold_index,
            ct,
            default_fg,
            default_flags,
            inlay_fg,
            whitespace,
        };
        rows.extend(build_display_rows(&inputs, edit_lo, affected_hi));
        rows.extend(
            prior_rows
                .iter()
                .filter(|r| r.source_line >= suffix_lo)
                .map(|r| r.with_source_line((r.source_line as i64 + net as i64).max(0) as u32)),
        );
        let chunk = Arc::new(DisplayChunk::new(0, rows, new_version));
        return Some(DisplayMatrix::whole_doc(chunk, new_line_count));
    }

    let chunk_size = new_chunk_size;
    let mut new_chunks: Vec<Arc<DisplayChunk>> = Vec::with_capacity(published.chunks.len() + 2);

    // Step 1: prefix-reuse.
    let mut rebuild_lo: u32 = 0;
    for chunk in published.chunks.iter() {
        let chunk_end = chunk.start_source_line.saturating_add(published.chunk_size);
        if chunk_end <= edit_lo {
            new_chunks.push(Arc::clone(chunk));
            rebuild_lo = chunk_end;
        } else {
            break;
        }
    }

    // Step 2: suffix-shift.
    let mut suffix_chunks: Vec<Arc<DisplayChunk>> = Vec::new();
    for chunk in published.chunks.iter() {
        if chunk.start_source_line >= suffix_lo {
            suffix_chunks.push(Arc::new(chunk.shifted_by(net, new_version)));
        }
    }
    // H.3 windowing (2026-06-04, found by the B2.3 `display_edit_path`
    // bench): when no suffix chunk remains — the edit fell in the LAST
    // covered chunk of a *windowed* large-file matrix — the rebuild zone
    // must stop at the published window's covered end (shifted by `net`),
    // NOT `new_line_count`. The full build is windowed to O(viewport)
    // (H.3); without this bound the incremental rebuild instead
    // materialised every row from the edit to EOF — O(file) — so typing
    // near the top of a 100k-line file cost ~57ms per keystroke, blowing
    // the sync edit path's O(window) guarantee. The matrix preserves its
    // windowed coverage here; a scroll past the window then fails the
    // `covers` gate in `recompute_pane` and triggers a recentred (windowed)
    // full rebuild.
    let rebuild_hi = suffix_chunks.first().map(|c| c.start_source_line).unwrap_or_else(|| {
        ((published.covered_end_line() as i64 + net as i64).max(rebuild_lo as i64) as u32)
            .min(new_line_count)
    });

    // Step 3: rebuild zone — ROW-LEVEL reuse (mirrors the whole-doc
    // branch above). Only the actually-edited line range
    // `[edit_lo, affected_hi)` is rebuilt; every unchanged line WITHIN the
    // zone `Arc`-reuses its prior `DisplayLine` (and thus its syntax
    // colour). Load-bearing for the keystroke UX contract: the sync edit
    // path ([`sync_rebuild_pane_on_edit`]) runs with `allow_highlight:
    // false`, so the prior *wholesale* rebuild of the zone blanked
    // ~`chunk_size` lines of colour on EVERY keystroke — a whole-viewport
    // syntax-highlight flicker on any chunked-mode file (e.g. README at 500
    // lines: the viewport sits inside one 64-line chunk, so the entire
    // screen lost colour per char, recoloured a frame later by the async
    // worker). Reusing prior rows confines the transient colour loss to the
    // single edited line. Whole-doc mode already did this; chunked mode
    // rebuilt the whole zone — that asymmetry WAS the flicker bug.
    // (feedback_decorations_update_in_place)
    //
    // Prior rows for the zone come from the STRADDLING chunks: those
    // neither fully prefix-reused (`chunk_end <= edit_lo`) nor fully
    // suffix-shifted (`chunk_start >= suffix_lo`).
    let zone_prior_rows: Vec<DisplayLine> = published
        .chunks
        .iter()
        .filter(|c| {
            let start = c.start_source_line;
            let end = start.saturating_add(published.chunk_size);
            end > edit_lo && start < suffix_lo
        })
        .flat_map(|c| c.rows.iter().cloned())
        .collect();

    // First unchanged line at/after the edit, mapped into post-edit space
    // (mirrors the whole-doc `affected_hi`); bounded by the zone end so a
    // suffix row never collides with the suffix-shifted chunks.
    let affected_hi = zone_prior_rows
        .iter()
        .map(|r| r.source_line)
        .find(|&l| l >= suffix_lo)
        .map(|l| (l as i64 + net as i64).max(edit_lo as i64) as u32)
        .unwrap_or(rebuild_hi)
        .min(rebuild_hi);

    let spans = highlight_range(edit_lo, affected_hi);
    let inputs = ChunkInputs {
        snapshot,
        per_line_spans: spans.as_ref(),
        spans_base: edit_lo,
        inlays_by_line: &inlays_by_line,
        fold_index: &fold_index,
        ct,
        default_fg,
        default_flags,
        inlay_fg,
        whitespace,
    };

    // Assemble the zone rows in source-line order:
    //   prefix-reuse (source_line < edit_lo, unshifted)
    //   ++ rebuilt edit range [edit_lo, affected_hi)
    //   ++ suffix-reuse (source_line >= suffix_lo, shifted by net).
    let mut zone_rows: Vec<DisplayLine> = Vec::with_capacity(zone_prior_rows.len() + 2);
    zone_rows.extend(
        zone_prior_rows
            .iter()
            .filter(|r| r.source_line < edit_lo)
            .cloned(),
    );
    zone_rows.extend(build_display_rows(&inputs, edit_lo, affected_hi));
    zone_rows.extend(
        zone_prior_rows
            .iter()
            .filter(|r| r.source_line >= suffix_lo)
            .map(|r| r.with_source_line((r.source_line as i64 + net as i64).max(0) as u32)),
    );

    // Re-bucket into `chunk_size`-aligned chunks — identical chunk starts
    // to the prior wholesale loop (`cur` stepping by `chunk_size`), so the
    // chunked-matrix shape and the `chunk_end = start + chunk_size`
    // prefix/suffix detection on the NEXT edit are preserved; only the ROWS
    // differ (reused vs rebuilt).
    let mut cur = rebuild_lo;
    let mut idx = 0usize;
    while cur < rebuild_hi {
        let end = cur.saturating_add(chunk_size).min(rebuild_hi);
        let mut bucket: Vec<DisplayLine> = Vec::new();
        while idx < zone_rows.len() && zone_rows[idx].source_line < end {
            bucket.push(zone_rows[idx].clone());
            idx += 1;
        }
        new_chunks.push(Arc::new(DisplayChunk::new(cur, bucket, new_version)));
        cur = end;
    }

    new_chunks.extend(suffix_chunks);

    Some(DisplayMatrix::chunked(
        new_chunks,
        chunk_size,
        new_line_count,
        new_version,
    ))
}

/// B2.2 (2026-06-04): project one [`crate::display_matrix::DisplayLine`]
/// to a [`CellRow`] — the temporary bridge that keeps the not-yet-cut-
/// over renderers (GPU until B3) painting off the cell grid while the
/// `DisplayMatrix` is canonical. Reproduces [`build_row_cells`]
/// byte-for-byte: per run, resolve `style → (fg, mods)` (inlay runs
/// take `inlay_fg` + `INLAY` only; trailing markers take the theme's
/// trailing-whitespace fg via the `WS_TRAILING` provenance bit); emit
/// one cell per char. `col_map` IS the cell path's `inlay_offsets`
/// (the B1 parity test pins this), so it transfers verbatim. The
/// `WS_TRAILING` bit is DisplayRun-only provenance — it is stripped
/// from the projected cell's flags so the result matches the cell
/// path, which bakes trailing-fg into `fg` instead. Deleted with the
/// cell path in B4.
fn display_line_to_cell_row(
    line: &crate::display_matrix::DisplayLine,
    ct: CellTheme<'_>,
    default_fg: u32,
    default_flags: u16,
    inlay_fg: u32,
) -> CellRow {
    use lattice_cells::cell_flags;
    let trailing_fg = ct
        .resolved
        .get(ct.ids.whitespace_trailing)
        .fg
        .map(|c| c.to_rgb_u32(default_fg))
        .unwrap_or(default_fg);
    let mut cells: Vec<Cell> = Vec::with_capacity(line.col_count as usize);
    let mut byte_off = 0usize;
    for run in line.runs.iter() {
        let run_len = run.len as usize;
        let slice = &line.text[byte_off..byte_off + run_len];
        byte_off += run_len;
        let is_inlay = run.flags & cell_flags::INLAY != 0;
        let is_ws_marker = run.flags & cell_flags::WS_MARKER != 0;
        let is_trailing = run.flags & cell_flags::WS_TRAILING != 0;
        let (style_fg, mods) = if matches!(run.style, lattice_syntax::Style::Default) {
            (default_fg, default_flags)
        } else {
            resolve_style(ct, run.style)
        };
        let (fg, flags) = if is_inlay {
            (inlay_fg, cell_flags::INLAY)
        } else if is_ws_marker {
            let f = if is_trailing { trailing_fg } else { style_fg };
            (f, mods | cell_flags::WS_MARKER)
        } else {
            (style_fg, mods)
        };
        for ch in slice.chars() {
            cells.push(Cell::new(ch as u32, fg, 0, flags));
        }
    }
    CellRow::new(cells, line.source_line, line.col_map.to_vec())
}

/// B2.2 (2026-06-04): project a whole [`crate::display_matrix::DisplayMatrix`]
/// to a [`CellMatrix`], preserving chunk structure, per-chunk versions,
/// mode (whole-doc vs chunked), and `wrap_width`. The bridge feeding the
/// cell renderers until their cutover (B2.4 TUI, B3 GPU); deleted in B4.
fn display_matrix_to_cell_matrix(
    dm: &crate::display_matrix::DisplayMatrix,
    ct: CellTheme<'_>,
) -> CellMatrix {
    let (default_fg, default_flags) = resolve_style(ct, lattice_syntax::Style::Default);
    let inlay_fg = inlay_hint_fg();
    if dm.chunks.is_empty() {
        return CellMatrix::empty();
    }
    let chunks: Vec<Arc<CellChunk>> = dm
        .chunks
        .iter()
        .map(|dc| {
            let rows: Vec<CellRow> = dc
                .rows
                .iter()
                .map(|dl| display_line_to_cell_row(dl, ct, default_fg, default_flags, inlay_fg))
                .collect();
            Arc::new(CellChunk::new(dc.start_source_line, rows, dc.version))
        })
        .collect();
    let mut cm = if dm.is_whole_doc() {
        // Whole-doc mode is exactly one chunk (guaranteed non-empty here).
        CellMatrix::whole_doc(chunks.into_iter().next().unwrap(), dm.source_line_count)
    } else {
        CellMatrix::chunked(chunks, dm.chunk_size, dm.source_line_count, dm.version)
    };
    cm.wrap_width = dm.wrap_width;
    cm
}

/// Bucket a flat inlay-hints list by line into per-line slices of
/// `(orig_byte, text)`, each bucket sorted ascending by `orig_byte`.
/// Output length is `line_count` so callers can index by line
/// without bounds-checking. Hints whose `line` is past `line_count`
/// are dropped — out-of-range payloads do not feed the build.
fn bucket_inlays_by_line<'a>(
    inlay_hints: &'a [crate::render_state::InlayHintRow],
    line_count: u32,
) -> std::collections::HashMap<u32, Vec<(u32, &'a str)>> {
    // H.3 (2026-06-04): keyed by absolute source line rather than a
    // dense `Vec<Vec<_>>` of length `line_count`. The dense form was
    // O(file) — it allocated and zeroed one slot per *document* line
    // even when the build only touches a viewport window (and even
    // when there are no inlays at all), which the
    // `cells_worker_windowed_build` bench exposed as build cost
    // scaling with `line_count`. A `HashMap` is O(inlays): empty when
    // inlay mode is off, and base-agnostic so the whole-doc, windowed,
    // and incremental builds all share one lookup without threading a
    // window offset.
    let mut buckets: std::collections::HashMap<u32, Vec<(u32, &'a str)>> =
        std::collections::HashMap::new();
    for h in inlay_hints {
        if h.line < line_count {
            buckets
                .entry(h.line)
                .or_default()
                .push((h.byte, h.text.as_str()));
        }
    }
    for b in buckets.values_mut() {
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
    crate::ui::theme::Color::Named(crate::ui::theme::NamedColor::DarkGray).to_rgb_u32(0)
}

/// Resolve a syntax style to its `0xRRGGBB` foreground colour via
/// the host theme. `Style::Default` and styles whose theme entry
/// has no explicit fg return `0` — the renderer maps that to "use
/// the pane's default text colour" at paint time.
///
/// S3.a (2026-05-26): kept under `#[cfg(test)]` after the worker
/// switched to [`resolve_style`] (returns `(fg, flags)` together).
/// Tests that only assert fg keep the simpler one-value helper.
#[cfg(test)]
fn resolve_fg(ct: CellTheme<'_>, style: lattice_syntax::Style) -> u32 {
    resolve_style(ct, style).0
}

/// T.5 (theme-system): the resolved theme read table + builtin ids the
/// cell builder threads in place of the old `&Theme`. `Copy` (two
/// refs), so it passes through the build chain for free. The builder
/// uses it only for syntax-category + whitespace-marker styling.
#[derive(Clone, Copy)]
pub struct CellTheme<'a> {
    pub resolved: &'a crate::ui::theme::ResolvedTheme,
    pub ids: &'a crate::ui::theme::BuiltinElementIds,
}

/// S3.a / T.5: resolve a syntax style to `(fg, flags)` via the shared
/// host `resolve_syntax_style` (resolved table). The returned `flags`
/// is the OR of `Cell::flags` modifier bits ([`flags::BOLD`] etc.)
/// matching the resolved style's [`crate::ui::theme::Modifiers`].
/// Splice-flags like `INLAY` / `WS_MARKER` are NOT set here — callers
/// OR those in separately when emitting an inlay or whitespace cell.
fn resolve_style(ct: CellTheme<'_>, style: lattice_syntax::Style) -> (u32, u16) {
    let s = crate::ui::theme::resolve_syntax_style(ct.resolved, ct.ids, style);
    let fg = s.fg.map(|c| c.to_rgb_u32(0)).unwrap_or(0);
    let flags = modifiers_to_flags(&s.modifiers);
    (fg, flags)
}

/// S3.a: pack the host theme's [`crate::ui::theme::Modifiers`]
/// (bold / italic / underline / dim / reverse) into the
/// `Cell::flags` bit layout declared in
/// [`lattice_cells::cell_flags`]. Keeps the
/// `host::Theme → Cell` mapping centralised so adding a new
/// modifier is a one-line change here + a one-line flag-bit
/// declaration in `lattice-cells`.
fn modifiers_to_flags(m: &crate::ui::theme::Modifiers) -> u16 {
    use lattice_cells::cell_flags;
    let mut f: u16 = 0;
    if m.bold {
        f |= cell_flags::BOLD;
    }
    if m.italic {
        f |= cell_flags::ITALIC;
    }
    if m.underline {
        f |= cell_flags::UNDERLINE;
    }
    if m.dim {
        f |= cell_flags::DIM;
    }
    if m.reverse {
        f |= cell_flags::REVERSE;
    }
    f
}

/// Resolve the highlight style at a given utf-8 byte offset inside
/// `line_spans`. Mirrors `highlights_worker::style_at_byte` —
/// bytes outside every span fall through to `Style::Default`.
fn style_at_byte(line_spans: &[lattice_syntax::StyledSpan], byte: usize) -> lattice_syntax::Style {
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

    /// T.5: a real resolved theme read-table + builtin ids for tests
    /// that thread a [`CellTheme`] into the builder. The default
    /// registry reproduces the legacy `Theme::default()` syntax colours
    /// exactly, so colour-asserting tests keep their expected values.
    fn test_cell_theme() -> (
        std::sync::Arc<crate::ui::theme::ResolvedTheme>,
        crate::ui::theme::BuiltinElementIds,
    ) {
        use crate::ui::theme::ThemeRegistry as _;
        let reg = crate::ui::theme::InMemoryThemeRegistry::with_defaults();
        (
            reg.resolved(),
            crate::ui::theme::BuiltinElementIds::capture(&reg),
        )
    }

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
        rs_with_everything(
            snapshot,
            version,
            matrix_cell,
            syntax_handle,
            theme,
            inlay_hints,
            folds,
            foldenable,
            None,
            0,
        )
    }

    /// S2.4.b: superset helper exposing `last_edit` and
    /// `viewport_height` for incremental-rebuild tests.
    ///
    /// D.4.d.1.b (2026-05-29): also publishes a single-pane
    /// `cells.panes[0]` entry mirroring the top-level inputs
    /// and using the same `matrix_cell`, so the worker (which
    /// now iterates `cells.panes`) writes into the caller's
    /// `matrix_cell`. Without this entry the worker would see
    /// an empty `panes` slice and return `CacheHit` without
    /// touching the matrix — every pre-d.1.b test expects the
    /// matrix to receive a fresh build.
    #[allow(clippy::too_many_arguments)]
    /// B2.2 test shim: the worker now persists its canonical state in
    /// `display_matrix`, so multi-publish tests that thread a shared
    /// `matrix_cell` need the matching display cell to persist too.
    /// Pair a per-thread display cell with each `matrix_cell` keyed by
    /// its Arc pointer, so the same `matrix_cell` always yields the
    /// same display cell (mirroring how `matrix_cell` itself persists),
    /// while a fresh `Arc::default()` gets a fresh display cell. The
    /// `Weak` guard pins the keyed address (a live `Weak` keeps the
    /// allocation reserved) so a freed+reused address can never leak a
    /// prior test's display cell into a later one.
    fn display_cell_for(
        matrix_cell: &Arc<ArcSwap<CellMatrix>>,
    ) -> Arc<ArcSwap<crate::display_matrix::DisplayMatrix>> {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::sync::Weak;
        type DisplayCell = Arc<ArcSwap<crate::display_matrix::DisplayMatrix>>;
        thread_local! {
            static MAP: RefCell<HashMap<usize, (Weak<ArcSwap<CellMatrix>>, DisplayCell)>> =
                RefCell::new(HashMap::new());
        }
        let key = Arc::as_ptr(matrix_cell) as usize;
        MAP.with(|m| {
            let mut map = m.borrow_mut();
            if let Some((weak, disp)) = map.get(&key) {
                if weak
                    .upgrade()
                    .is_some_and(|a| Arc::ptr_eq(&a, matrix_cell))
                {
                    return disp.clone();
                }
            }
            let disp: DisplayCell = Arc::new(ArcSwap::from_pointee(
                crate::display_matrix::DisplayMatrix::empty(),
            ));
            map.insert(key, (Arc::downgrade(matrix_cell), disp.clone()));
            disp
        })
    }

    fn rs_with_everything(
        snapshot: Option<Arc<DocumentSnapshot>>,
        version: MatrixVersion,
        matrix_cell: Arc<ArcSwap<CellMatrix>>,
        syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,
        theme: crate::ui::theme::Theme,
        inlay_hints: Vec<crate::render_state::InlayHintRow>,
        folds: Vec<lattice_core::Fold>,
        foldenable: bool,
        last_edit: Option<lattice_cells::EditDelta>,
        viewport_height: u32,
    ) -> ArcSwap<RenderState> {
        let inlay_hints_arc: Arc<[crate::render_state::InlayHintRow]> =
            Arc::from(inlay_hints.into_boxed_slice());
        let folds_arc: Arc<[lattice_core::Fold]> = Arc::from(folds.into_boxed_slice());
        // Canonical display cell persisted alongside the shared
        // `matrix_cell` (see `display_cell_for`).
        let display_cell = display_cell_for(&matrix_cell);
        let pane_entry = crate::render_state::PaneCellsInputs {
            pane_id: lattice_core::ui::pane::PaneId::default(),
            buffer_id: lattice_core::BufferId::default(),
            matrix: matrix_cell.clone(),
            display_matrix: display_cell.clone(),
            virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(
                lattice_cells::VirtualRowMatrix::empty(),
            )),
            version,
            snapshot: snapshot.clone(),
            syntax_handle: syntax_handle.clone(),
            inlay_hints: inlay_hints_arc.clone(),
            folds: folds_arc.clone(),
            viewport_height,
            scroll: 0,
            viewport_width: 0,
            wrap: false,
            foldenable,
            last_edit,
            excerpt_syntax: Arc::from([]),
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
        let (resolved_theme, theme_ids) = test_cell_theme();
        let cells = CellsRenderState {
            matrix: matrix_cell,
            version,
            snapshot,
            syntax_handle,
            inlay_hints: inlay_hints_arc,
            folds: folds_arc,
            viewport_height,
            foldenable,
            last_edit,
            theme,
            resolved_theme,
            theme_ids,
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
            whitespace: 0,
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

        let decision = recompute(&rs);
        assert_eq!(decision, WorkerDecision::Clear);
        assert!(matrix_cell.load().is_empty());

        // Second call sees an already-empty matrix at version ZERO;
        // the idempotent Clear branch short-circuits without a store.
        let before = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(recompute(&rs), WorkerDecision::Clear);
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

        let decision = recompute(&rs);
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

        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(recompute(&rs), WorkerDecision::CacheHit);
        let second_ptr = Arc::as_ptr(&matrix_cell.load_full());
        assert_eq!(first_ptr, second_ptr, "cache-hit must not store a new Arc");
    }

    /// Version bump triggers a fresh build. Earlier matrix is
    /// replaced; new matrix carries the new version stamp.
    #[test]
    fn version_bump_rebuilds_matrix() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of("aaa");
        let rs1 = rs_with_snapshot(Some(snap1), v(1), matrix_cell.clone());
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        assert_eq!(matrix_cell.load().version, v(1));

        // New snapshot + bumped text version.
        let snap2 = snap_of("bbbb");
        let rs2 = rs_with_snapshot(Some(snap2), v(2), matrix_cell.clone());
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
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
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
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
        let rs =
            rs_with_snapshot_themed(Some(snap), v(1), matrix_cell.clone(), Some(handle), theme);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        let rows: Vec<&CellRow> = m.slice(0, 10).iter().collect();
        assert!(rows.len() >= 2, "expected at least 2 rows for {text:?}");

        // Default fg for the theme is Catppuccin Mocha "Text"
        // (0xcdd6f4) and the keyword fg is Mauve (0xcba6f7).
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let expected_default = resolve_fg(ct, lattice_syntax::Style::Default);
        let expected_keyword = resolve_fg(ct, lattice_syntax::Style::Keyword);
        let expected_comment = resolve_fg(ct, lattice_syntax::Style::LineComment);
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
        assert!(
            line1.cells.iter().all(|c| c.fg == expected_comment),
            "every cell on a line-comment row must carry the comment fg; got {:?}",
            line1.cells.iter().map(|c| c.fg).collect::<Vec<_>>()
        );
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
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let default_fg = resolve_fg(ct, lattice_syntax::Style::Default);
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
        let rs =
            rs_with_snapshot_themed(Some(snap), v(1), matrix_cell.clone(), Some(handle), theme);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let default_fg = resolve_fg(ct, lattice_syntax::Style::Default);
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
        let rs = rs_with_snapshot_full(Some(snap), v(1), matrix_cell.clone(), None, theme, hints);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
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
        let rs = rs_with_snapshot_full(Some(snap), v(1), matrix_cell.clone(), None, theme, hints);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let row = matrix_cell
            .load()
            .slice(0, 1)
            .iter()
            .next()
            .cloned()
            .unwrap();
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
        let rs = rs_with_snapshot_full(Some(snap), v(1), matrix_cell.clone(), None, theme, hints);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let row = matrix_cell
            .load()
            .slice(0, 1)
            .iter()
            .next()
            .cloned()
            .unwrap();
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
        let rs = rs_with_snapshot_full(Some(snap), v(1), matrix_cell.clone(), None, theme, hints);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let row = matrix_cell
            .load()
            .slice(0, 1)
            .iter()
            .next()
            .cloned()
            .unwrap();
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
            whitespace: 0,
        };
        let v_b = MatrixVersion {
            inlay_hints: 1,
            ..v_a
        };

        let rs1 = rs_with_snapshot_full(
            Some(snap.clone()),
            v_a,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
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
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
        assert_ne!(first_ptr, Arc::as_ptr(&matrix_cell.load_full()));
        let row = matrix_cell
            .load()
            .slice(0, 1)
            .iter()
            .next()
            .cloned()
            .unwrap();
        assert_eq!(row_text(&row), "!a");
    }

    // ---- S2.4.a — chunked-mode switch ----

    /// `pick_chunk_size` policy: small docs and zero-viewport
    /// inputs collapse to whole-doc; everything past the
    /// `4 × viewport_height` threshold goes chunked.
    #[test]
    fn pick_chunk_size_policy() {
        // viewport == 0 → whole-doc regardless of line count.
        assert_eq!(pick_chunk_size(0, 0), ChunkMode::WholeDoc);
        assert_eq!(pick_chunk_size(0, 1_000_000), ChunkMode::WholeDoc);

        // Below threshold (line_count <= 4 × viewport): whole-doc.
        assert_eq!(pick_chunk_size(50, 200), ChunkMode::WholeDoc);
        assert_eq!(pick_chunk_size(50, 199), ChunkMode::WholeDoc);

        // Above threshold: chunked with next_pow2(2 × viewport).
        // viewport=50 → 2×50=100 → next_pow2 = 128.
        assert_eq!(pick_chunk_size(50, 201), ChunkMode::Chunked(128));
        // viewport=70 → 2×70=140 → next_pow2 = 256.
        assert_eq!(pick_chunk_size(70, 281), ChunkMode::Chunked(256));

        // 16-line floor: tiny viewport doesn't produce sub-16
        // chunks.
        // viewport=3 → 2×3=6, clamped to 16 → next_pow2 = 16.
        assert_eq!(pick_chunk_size(3, 13), ChunkMode::Chunked(16));
    }

    #[test]
    fn next_power_of_two_table() {
        assert_eq!(next_power_of_two(0), 1);
        assert_eq!(next_power_of_two(1), 1);
        assert_eq!(next_power_of_two(2), 2);
        assert_eq!(next_power_of_two(3), 4);
        assert_eq!(next_power_of_two(15), 16);
        assert_eq!(next_power_of_two(16), 16);
        assert_eq!(next_power_of_two(17), 32);
        assert_eq!(next_power_of_two(100), 128);
        assert_eq!(next_power_of_two(1_000_000), 1_048_576);
        // u32::MAX-class inputs saturate at 1 << 31 (avoid overflow).
        assert_eq!(next_power_of_two(u32::MAX), 1u32 << 31);
    }

    /// Small doc (line_count <= 4 × viewport_height) builds a
    /// whole-doc matrix — one chunk covering every line.
    #[test]
    fn small_doc_stays_whole_doc() {
        // 5 lines + viewport 5 → threshold 20; line_count <= 20.
        let snap = snap_of_versioned("l0\nl1\nl2\nl3\nl4", 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs = rs_with_everything(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            crate::ui::theme::Theme::default(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert!(m.is_whole_doc(), "small doc must stay whole-doc");
        assert_eq!(m.chunks.len(), 1);
        assert_eq!(m.visible_line_count, 5);
    }

    /// Large doc (line_count > 4 × viewport_height) switches to
    /// chunked mode. Chunk size = next_pow2(2 × viewport_height),
    /// chunks cover the full document, and `matrix.slice(0, all)`
    /// walks them in order with row content matching each line.
    #[test]
    fn large_doc_splits_into_chunks() {
        // viewport=5, threshold=20. Build 25 lines so we cross it.
        // chunk_size = next_pow2(10) = 16. Expect ceil(25/16) = 2.
        let text: String = (0..25)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap = snap_of_versioned(&text, 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs = rs_with_everything(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            crate::ui::theme::Theme::default(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        assert!(!m.is_whole_doc());
        assert_eq!(m.chunk_size, 16);
        assert_eq!(m.source_line_count, 25);
        assert_eq!(m.visible_line_count, 25);
        // Two chunks: 16 + 9 rows.
        assert_eq!(m.chunks.len(), 2);
        assert_eq!(m.chunks[0].start_source_line, 0);
        assert_eq!(m.chunks[0].rows.len(), 16);
        assert_eq!(m.chunks[1].start_source_line, 16);
        assert_eq!(m.chunks[1].rows.len(), 9);

        // Slice iteration walks across chunks transparently and
        // preserves logical source_line on each row.
        let source_lines: Vec<u32> = m.slice(0, 100).iter().map(|r| r.source_line).collect();
        assert_eq!(source_lines, (0u32..25).collect::<Vec<_>>());
    }

    /// Fold elision works in chunked mode: a closed fold whose
    /// interior crosses a chunk boundary still elides the right
    /// source lines. The fold's start_line lands in chunk 0; the
    /// fold's end_line lands in chunk 1; only the start stays
    /// visible.
    #[test]
    fn chunked_mode_honours_fold_elision_across_chunks() {
        let text: String = (0..25)
            .map(|i| format!("l{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap = snap_of_versioned(&text, 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Close a fold from line 10 through line 20 — interior
        // lines 11..=20 are elided, 10 stays.
        let folds = vec![closed_fold(10, 20)];
        let rs = rs_with_everything(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            None,
            crate::ui::theme::Theme::default(),
            Vec::new(),
            folds,
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        assert!(!m.is_whole_doc(), "still chunked");
        assert_eq!(m.source_line_count, 25);
        // 25 source - 10 elided (11..=20) = 15 visible rows.
        assert_eq!(m.visible_line_count, 15);
        let source_lines: Vec<u32> = m.slice(0, 100).iter().map(|r| r.source_line).collect();
        let expected: Vec<u32> = (0u32..=10).chain(21u32..25).collect();
        assert_eq!(source_lines, expected);
    }

    /// Viewport-height change that crosses the threshold flips the
    /// matrix between whole-doc and chunked shapes — exercised
    /// because `viewport_height` is not in `MatrixVersion` (it
    /// only changes via dispatch); the publisher's version axes
    /// must still drive the rebuild. We bump `text` to simulate
    /// the version cascade that would accompany a viewport
    /// resize-induced republish.
    #[test]
    fn viewport_shrink_can_promote_to_chunked() {
        let theme = crate::ui::theme::Theme::default();
        let text: String = (0..25)
            .map(|i| format!("l{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap = snap_of_versioned(&text, 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        let mk_rs = |vp: u32, version: MatrixVersion| -> ArcSwap<RenderState> {
            rs_with_everything(
                Some(snap.clone()),
                version,
                matrix_cell.clone(),
                None,
                theme,
                Vec::new(),
                Vec::new(),
                true,
                None,
                vp,
            )
        };

        // Wide viewport (8) → 4×8=32 ≥ 25 → whole-doc.
        let rs1 = mk_rs(8, v(1));
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        assert!(matrix_cell.load().is_whole_doc());

        // Shrink to 5 → 4×5=20 < 25 → chunked. Bump text version
        // so the cache key differs and the worker rebuilds.
        let rs2 = mk_rs(5, v(2));
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert!(!m.is_whole_doc(), "post-shrink must be chunked");
        assert_eq!(m.chunk_size, 16);
    }

    // ---- H.3 — viewport-scoped (windowed) chunked matrix ----

    /// Build a snapshot of `line_count` short lines at version 1.
    fn big_snap(line_count: u32) -> Arc<DocumentSnapshot> {
        let text: String = (0..line_count)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        snap_of_versioned(&text, 1)
    }

    /// `window_bounds` is full-coverage at/below the cap and a
    /// chunk-aligned window above it.
    #[test]
    fn window_bounds_full_below_cap_windowed_above() {
        let cs = 128;
        // At/below cap → whole doc regardless of scroll.
        assert_eq!(window_bounds(0, 50, WINDOW_CAP_LINES, cs), (0, WINDOW_CAP_LINES));
        assert_eq!(
            window_bounds(900, 50, WINDOW_CAP_LINES, cs),
            (0, WINDOW_CAP_LINES)
        );
        // Above cap → window around the viewport, aligned to chunks.
        // scroll=2500, vh=50, overscan=50 ⇒ raw [2450, 2600);
        // align lo down to 2432 (19·128), hi up to 2688 (21·128).
        let (lo, hi) = window_bounds(2500, 50, 5000, cs);
        assert_eq!(lo % cs, 0, "lo chunk-aligned");
        assert_eq!(hi % cs, 0, "hi chunk-aligned");
        assert!(lo <= 2450 && hi >= 2600, "window brackets the viewport+overscan");
        assert!(hi - lo < 5000, "window is a strict subset of the doc");
        // Window never exceeds the document.
        let (_, hi_eof) = window_bounds(4990, 50, 5000, cs);
        assert!(hi_eof <= 5000);
    }

    /// A large doc (> `WINDOW_CAP_LINES`) builds a matrix that covers
    /// only the viewport window, not the whole document. Off-window
    /// source lines have no row (the renderers fall back to plain
    /// text / legacy spans for those).
    #[test]
    fn windowed_matrix_covers_viewport_not_whole_doc() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let mut pane = pane_inputs(matrix_cell.clone(), Some(big_snap(5000)), v(1), 50);
        pane.scroll = 2500;
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let ws = WhitespaceConfig::default();

        assert_eq!(
            recompute_pane(&pane, ct, &ws),
            WorkerDecision::Recomputed
        );
        let m = matrix_cell.load();
        assert!(!m.is_whole_doc(), "large doc is chunked");
        assert_eq!(m.source_line_count, 5000, "true doc line count preserved");
        // Covered range is a small window around the viewport, NOT the
        // whole 5000-line document — this is the O(viewport) win.
        assert!(m.covered_start_line() <= 2500);
        assert!(m.covered_end_line() >= 2550);
        assert!(
            m.covered_end_line() - m.covered_start_line() < 1000,
            "covered span bounded (got {}..{})",
            m.covered_start_line(),
            m.covered_end_line()
        );
        assert!(m.covers(2500, 2550), "viewport is covered");
        // In-window line has a row; far off-window lines do not.
        assert!(m.row_at_source_line(2500).is_some());
        assert!(
            m.row_at_source_line(10).is_none(),
            "line far above the window has no row"
        );
        assert!(
            m.row_at_source_line(4990).is_none(),
            "line far below the window has no row"
        );
    }

    /// Scrolling the viewport past the covered window (no version
    /// change) forces a rebuild that recentres the window on the new
    /// scroll — the old region is no longer covered.
    #[test]
    fn scroll_past_window_triggers_rebuild() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let ws = WhitespaceConfig::default();

        // First build at the top.
        let mut pane = pane_inputs(matrix_cell.clone(), Some(big_snap(5000)), v(1), 50);
        pane.scroll = 0;
        assert_eq!(
            recompute_pane(&pane, ct, &ws),
            WorkerDecision::Recomputed
        );
        assert!(matrix_cell.load().row_at_source_line(0).is_some());

        // Same version, jump far down. Pure-scroll past the window must
        // rebuild (not CacheHit) and recentre.
        pane.scroll = 3000;
        assert_eq!(
            recompute_pane(&pane, ct, &ws),
            WorkerDecision::Recomputed,
            "scroll past window rebuilds despite unchanged version"
        );
        let m = matrix_cell.load();
        assert!(m.covers(3000, 3050), "new viewport covered");
        assert!(m.row_at_source_line(3000).is_some());
        assert!(
            m.row_at_source_line(0).is_none(),
            "window moved off the original top"
        );
    }

    /// Scrolling within the covered window (overscan slack) is a
    /// cache hit — no rebuild on line-by-line scrolling.
    #[test]
    fn in_window_scroll_is_cache_hit() {
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let ws = WhitespaceConfig::default();

        let mut pane = pane_inputs(matrix_cell.clone(), Some(big_snap(5000)), v(1), 50);
        pane.scroll = 2500;
        assert_eq!(
            recompute_pane(&pane, ct, &ws),
            WorkerDecision::Recomputed
        );
        // A few lines of scroll stays inside the overscan window.
        pane.scroll = 2510;
        assert_eq!(
            recompute_pane(&pane, ct, &ws),
            WorkerDecision::CacheHit,
            "small in-window scroll does not rebuild"
        );
    }

    // ---- B1: display-builder parity with the cell builder ----

    /// The canonical `build_display_row` must reproduce the exact
    /// content `build_row_cells` produces (whitespace off): projecting
    /// the `DisplayLine` runs back to cells yields the same codepoints +
    /// resolved fg + flags, and `col_map` equals `inlay_offsets`. Guards
    /// against drift while both builders coexist (B1→B4).
    #[test]
    fn display_build_parity_with_cells_ws_off() {
        use lattice_cells::cell_flags;
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let (default_fg, default_flags) = resolve_style(ct, lattice_syntax::Style::Default);
        let inlay_fg = inlay_hint_fg();
        let ws = WhitespaceConfig::default(); // show: false

        // "let\tx" — `let` styled as Keyword, a tab (expands), and an
        // inlay ": T" spliced after byte 5 (EOL).
        let text = "let\tx";
        let spans = vec![lattice_syntax::StyledSpan {
            start: 0,
            end: 3,
            style: lattice_syntax::Style::Keyword,
        }];
        let inlays: Vec<(u32, &str)> = vec![(5, ": T")];

        let (cells, inlay_offsets) = build_row_cells(
            text,
            &spans,
            &inlays,
            ct,
            default_fg,
            default_flags,
            inlay_fg,
            &ws,
        );
        let (dtext, runs, col_map, col_count) = build_display_row(text, &spans, &inlays, &ws);

        // Project display runs → cells (the ws-off resolution path).
        let mut projected: Vec<Cell> = Vec::new();
        let mut byte = 0usize;
        for run in &runs {
            let s = &dtext[byte..byte + run.len as usize];
            for ch in s.chars() {
                let (fg, flags) = if run.flags & cell_flags::INLAY != 0 {
                    (inlay_fg, cell_flags::INLAY)
                } else if matches!(run.style, lattice_syntax::Style::Default) {
                    (default_fg, default_flags)
                } else {
                    resolve_style(ct, run.style)
                };
                projected.push(Cell::new(ch as u32, fg, 0, flags));
            }
            byte += run.len as usize;
        }

        assert_eq!(
            projected, cells,
            "display→cells projection must equal build_row_cells output"
        );
        assert_eq!(col_map, inlay_offsets, "col_map must equal inlay_offsets");
        assert_eq!(
            col_count,
            cells.len() as u32,
            "col_count must equal the display cell count"
        );
        assert!(
            runs.iter()
                .any(|r| matches!(r.style, lattice_syntax::Style::Keyword)),
            "the `let` keyword run must carry the Keyword style tag"
        );
        assert!(
            runs.iter().any(|r| r.flags & cell_flags::INLAY != 0),
            "the inlay must produce an INLAY-flagged run"
        );
    }

    /// B2.2: the full `DisplayMatrix → CellMatrix` projection must equal
    /// the cell builder byte-for-byte across the hard cases the
    /// projection has to reconstruct from runs alone — whitespace ON
    /// (leading / interior / trailing markers), tab expansion, AND an
    /// inlay splice. The trailing case is the load-bearing one: the
    /// default theme's trailing-whitespace fg is red, so a trailing
    /// marker's cell fg differs from a non-trailing one — the projection
    /// recovers that only via the `WS_TRAILING` run flag. A regression in
    /// either builder (or a missing `WS_TRAILING`) makes the projected
    /// cells diverge from `build_matrix` and fails here.
    #[test]
    fn projection_parity_ws_on_trailing_tab_inlay() {
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        // The default theme paints trailing whitespace red — assert that
        // so the parity check below genuinely exercises `WS_TRAILING`.
        // T.5: the trailing-whitespace style resolves through the
        // `whitespace.trailing` element (the old `Theme` style field is
        // gone), mirroring the production read at the `WS_TRAILING` site.
        let (default_fg, _) = resolve_style(ct, lattice_syntax::Style::Default);
        let trailing_fg = resolved
            .get(ids.whitespace_trailing)
            .fg
            .map(|c| c.to_rgb_u32(default_fg))
            .unwrap_or(default_fg);
        assert_ne!(
            trailing_fg, default_fg,
            "test premise: default theme trailing fg must differ from default fg"
        );

        let ws = WhitespaceConfig {
            show: true,
            tab: Some('→'),
            trailing: Some('·'),
            leading: Some('▏'),
            space: Some('•'),
            eol: None,
            tabstop: 4,
        };
        // line 0: leading tab, "ab", two trailing spaces.
        // line 1: all-blank (every cell trailing).
        // line 2: "xy" + an inlay spliced after byte 1.
        let snap = snap_of_versioned("\tab  \n   \nxy", 1);
        let inlays = vec![inlay(2, 1, ": T")];

        let cm = build_matrix(
            snap.as_ref(),
            None,
            ct,
            &inlays,
            &[],
            true,
            5, // whole-doc
            0,
            v(1),
            &ws,
        );
        let dm = build_display_matrix(
            snap.as_ref(),
            None,
            &[],
            ct,
            &inlays,
            &[],
            true,
            5,
            0,
            v(1),
            &ws,
        );
        let projected = display_matrix_to_cell_matrix(&dm, ct);

        assert_eq!(projected.source_line_count, cm.source_line_count);
        assert_eq!(projected.is_whole_doc(), cm.is_whole_doc());
        let mut saw_trailing = false;
        for line in 0..cm.source_line_count {
            let a = cm.row_at_source_line(line).expect("cell row");
            let b = projected.row_at_source_line(line).expect("projected row");
            assert_eq!(
                a.cells, b.cells,
                "projected cells for line {line} must equal build_matrix"
            );
            assert_eq!(
                a.inlay_offsets, b.inlay_offsets,
                "projected inlay_offsets for line {line} must equal build_matrix"
            );
            saw_trailing |= b.cells.iter().any(|c| c.fg == trailing_fg);
        }
        assert!(
            saw_trailing,
            "scenario must produce trailing-fg cells (WS_TRAILING projection path)"
        );
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
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        // source_line_count is preserved (pre-fold logical count).
        assert_eq!(m.source_line_count, 5);
        // visible_line_count post-fold: 5 - 2 elided = 3 rows.
        assert_eq!(m.visible_line_count, 3);
        let source_lines: Vec<u32> = m.slice(0, 10).iter().map(|r| r.source_line).collect();
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
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        assert_eq!(m.visible_line_count, 3);
        let source_lines: Vec<u32> = m.slice(0, 10).iter().map(|r| r.source_line).collect();
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
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
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
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        let m = matrix_cell.load();
        let source_lines: Vec<u32> = m.slice(0, 10).iter().map(|r| r.source_line).collect();
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
            whitespace: 0,
        };
        let v_b = MatrixVersion { theme: 0xbb, ..v_a };

        let rs1 =
            rs_with_snapshot_themed(Some(snap.clone()), v_a, matrix_cell.clone(), None, theme);
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&matrix_cell.load_full());

        // Repeat with the same version: cache-hit, no store.
        assert_eq!(recompute(&rs1), WorkerDecision::CacheHit);
        assert_eq!(first_ptr, Arc::as_ptr(&matrix_cell.load_full()));

        // Bump only the theme axis: must rebuild.
        let rs2 = rs_with_snapshot_themed(Some(snap), v_b, matrix_cell.clone(), None, theme);
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
        assert_ne!(first_ptr, Arc::as_ptr(&matrix_cell.load_full()));
    }

    // ---- S2.4.b — incremental rebuild ----

    fn edit_delta(start: u32, removed: u32, added: u32) -> lattice_cells::EditDelta {
        lattice_cells::EditDelta {
            start_line: start,
            lines_removed: removed,
            lines_added: added,
        }
    }

    /// Whole-doc mode + single-text-edit takes the incremental
    /// branch (`RecomputedIncremental`), even though whole-doc has
    /// nothing to reuse — the eligibility check passes and the
    /// branch produces a correct matrix.
    #[test]
    fn whole_doc_with_edit_takes_incremental_branch() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        // First publish: 3-line doc at text_version 1, no edit.
        let snap1 = snap_of_versioned("aa\nbb\ncc", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5, // small viewport ⇒ whole-doc
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        assert!(matrix_cell.load().is_whole_doc());

        // Second publish: insert a line at line 1; text_version → 2.
        let snap2 = snap_of_versioned("aa\nNEW\nbb\ncc", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(1, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);
        let m = matrix_cell.load();
        assert!(m.is_whole_doc());
        assert_eq!(m.source_line_count, 4);
        let row_texts: Vec<String> = m
            .slice(0, 10)
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| char::from_u32(c.codepoint).unwrap_or('?'))
                    .collect()
            })
            .collect();
        assert_eq!(row_texts, vec!["aa", "NEW", "bb", "cc"]);
    }

    /// 2026-06-05 REGRESSION (intra-line text lag). A PURE intra-line edit
    /// — inserting a char without crossing a newline, `EditDelta {removed:0,
    /// added:0}` — has `pre_edit_end_line() == start_line == the edited
    /// line`. The row-reuse partition used to classify that line as the
    /// unchanged SUFFIX and reuse its prior row VERBATIM while stamping the
    /// matrix version current, so the renderer's per-line staleness fallback
    /// never fired and it painted PRE-edit text for a frame (`|word` →
    /// `w|ord` → ` |word`; the felt "one key behind" typing lag on every
    /// keystroke). The edited line MUST be rebuilt from the current snapshot;
    /// unchanged lines still reuse their `text` Arc (colour preserved). The
    /// sibling `whole_doc_incremental_reuses_unchanged_rows` covers the
    /// STRUCTURAL edit (`{0,1}`, a clean line insert) where the boundary line
    /// only SHIFTS and must NOT regress to a rebuild — which is exactly why
    /// the fix is gated on `removed == added == 0`.
    #[test]
    fn whole_doc_incremental_rebuilds_intra_line_edited_row() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        let snap1 = snap_of_versioned("word\nbb\ncc", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5, // whole-doc
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let dm_cell = display_cell_for(&matrix_cell);
        let d1 = dm_cell.load_full();
        assert_eq!(&*d1.row_at_source_line(0).unwrap().text, "word");
        let bb_pre = Arc::clone(&d1.row_at_source_line(1).unwrap().text);
        let cc_pre = Arc::clone(&d1.row_at_source_line(2).unwrap().text);

        // Insert a space at col 0 of line 0 → " word\nbb\ncc". Pure
        // intra-line: no newline crossed ⇒ EditDelta {start:0, removed:0,
        // added:0} (line count unchanged). This is the keystroke shape.
        let snap2 = snap_of_versioned(" word\nbb\ncc", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(0, 0, 0);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);
        let d2 = dm_cell.load_full();
        // THE REGRESSION ASSERTION: the edited row is text-CURRENT, not the
        // reused-stale "word". Pre-fix this was "word" and the typed space
        // lagged a frame behind the cursor.
        assert_eq!(
            &*d2.row_at_source_line(0).unwrap().text,
            " word",
            "intra-line edited row must rebuild to the CURRENT text, never reuse the stale pre-edit row"
        );
        // Unchanged lines still reuse their prior row Arc (colour preserved —
        // no whole-viewport recolour flicker).
        assert!(
            Arc::ptr_eq(&bb_pre, &d2.row_at_source_line(1).unwrap().text),
            "unchanged line 1 must reuse its prior DisplayLine text Arc"
        );
        assert!(
            Arc::ptr_eq(&cc_pre, &d2.row_at_source_line(2).unwrap().text),
            "unchanged line 2 must reuse its prior DisplayLine text Arc"
        );
    }

    /// 2026-06-04: whole-doc incremental rebuild must REUSE the prior
    /// chunk's `DisplayLine`s for unchanged lines (same `text` Arc),
    /// not rebuild them — that reuse is what keeps their syntax colours
    /// through the post-edit window when `per_line_spans` lags (the
    /// markdown whole-viewport stutter). Asserts Arc identity of an
    /// unchanged prefix row and a shifted suffix row across the edit.
    /// B2.2: the reuse guarantee moved from the cell grid (now
    /// re-projected each rebuild) to the canonical `DisplayMatrix`.
    #[test]
    fn whole_doc_incremental_reuses_unchanged_rows() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        let snap1 = snap_of_versioned("aa\nbb\ncc", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5, // whole-doc
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        // B2.2: the row-reuse guarantee lives on the canonical
        // `DisplayMatrix` now (the cell grid is re-projected each
        // rebuild, so cell-row Arcs intentionally differ). Assert the
        // unchanged `DisplayLine`s reuse their `text` Arc across the
        // edit — that reuse is what keeps their colours through the
        // post-edit window.
        let dm_cell = display_cell_for(&matrix_cell);
        let d1 = dm_cell.load_full();
        let aa_pre = Arc::clone(&d1.row_at_source_line(0).unwrap().text);
        let bb_pre = Arc::clone(&d1.row_at_source_line(1).unwrap().text);

        // Insert a line at line 1 → "aa\nNEW\nbb\ncc".
        let snap2 = snap_of_versioned("aa\nNEW\nbb\ncc", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(1, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);
        let d2 = dm_cell.load_full();
        assert!(
            Arc::ptr_eq(&aa_pre, &d2.row_at_source_line(0).unwrap().text),
            "unchanged prefix row (line 0) must reuse the prior DisplayLine text Arc — keeps its colours"
        );
        assert!(
            Arc::ptr_eq(&bb_pre, &d2.row_at_source_line(2).unwrap().text),
            "shifted suffix row (\"bb\": line 1 → 2) must reuse the prior DisplayLine text Arc — keeps its colours"
        );
    }

    /// B2.3 (2026-06-04): the synchronous edit-path rebuild
    /// (`sync_rebuild_pane_on_edit`) makes the canonical `DisplayMatrix`
    /// text-current (`version.text` == the post-edit snapshot) WITHOUT
    /// highlighting — even with a *current* syntax handle attached the
    /// rebuilt line's runs are all `Style::Default`, proving the
    /// `allow_highlight: false` path keeps `highlight_lines` off the
    /// edit-critical actor thread. Unchanged lines `Arc`-reuse their prior
    /// `DisplayLine` (keeping whatever colour they had).
    #[tokio::test]
    async fn sync_rebuild_on_edit_is_text_current_and_unhighlighted() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        // v1: plain seed (no handle) → prior whole-doc display matrix.
        let snap1 = snap_of_versioned("let a = 1;\nlet b = 2;\nlet c = 3;\n", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let dm_cell = display_cell_for(&matrix_cell);
        let row0_pre = Arc::clone(&dm_cell.load_full().row_at_source_line(0).unwrap().text);

        // v2: edit line 1 in place → introduces `fn`. Attach a Rust handle
        // parsed + seeded at v2 so syntax IS available and current — the
        // sync path must STILL not colour the rebuilt line.
        let text2 = "let a = 1;\nfn b() {}\nlet c = 3;\n";
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .unwrap();
        s.parse_at(text2, 2);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded_with_runtime(
            s,
            &tokio::runtime::Handle::current(),
            None,
        ));
        let snap2 = snap_of_versioned(text2, 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(1, 1, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            Some(handle),
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        let loaded = rs2.load_full();
        let cells = loaded.cells.load();
        let pane = &cells.panes[0];
        assert!(
            sync_rebuild_pane_on_edit(
                pane,
                CellTheme {
                    resolved: &cells.resolved_theme,
                    ids: &cells.theme_ids,
                },
                &cells.whitespace
            ),
            "single in-place edit is eligible for the sync rebuild"
        );

        let dm = dm_cell.load_full();
        assert_eq!(
            dm.version.text, 2,
            "display matrix is text-current after the sync rebuild"
        );
        let row1 = dm.row_at_source_line(1).unwrap();
        assert_eq!(&*row1.text, "fn b() {}");
        assert!(
            row1.runs
                .iter()
                .all(|r| matches!(r.style, lattice_syntax::Style::Default)),
            "sync rebuild must NOT highlight — all runs default-styled despite a current syntax handle"
        );
        assert!(
            Arc::ptr_eq(&row0_pre, &dm.row_at_source_line(0).unwrap().text),
            "unchanged prefix row reuses its prior DisplayLine (keeps its colour)"
        );
    }

    /// B2.3: a non-edit publish (no `last_edit`) is ineligible for the
    /// sync rebuild — it returns `false` and leaves `display_matrix`
    /// untouched, deferring to the async worker's full/highlighted build.
    #[test]
    fn sync_rebuild_skips_non_edit_publish() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of_versioned("aa\nbb\ncc", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let dm_cell = display_cell_for(&matrix_cell);
        let before = Arc::as_ptr(&dm_cell.load_full());

        // A theme-only republish (no last_edit): bump the theme axis,
        // keep text/snapshot identical.
        let snap2 = snap_of_versioned("aa\nbb\ncc", 1);
        let v2 = MatrixVersion {
            text: 1,
            syntax: 1,
            theme: 99,
            ..MatrixVersion::ZERO
        };
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        let loaded = rs2.load_full();
        let cells = loaded.cells.load();
        let pane = &cells.panes[0];
        assert!(
            !sync_rebuild_pane_on_edit(
                pane,
                CellTheme {
                    resolved: &cells.resolved_theme,
                    ids: &cells.theme_ids,
                },
                &cells.whitespace
            ),
            "non-edit publish is ineligible for the sync rebuild"
        );
        assert_eq!(
            before,
            Arc::as_ptr(&dm_cell.load_full()),
            "ineligible sync rebuild must not touch display_matrix"
        );
    }

    /// B2.3: after the actor's sync rebuild makes `display_matrix`
    /// text-current but leaves the projected cell grid a frame behind (the
    /// projection stays off the actor thread per the threading guarantee),
    /// the async worker's next `recompute` reconciles the lagging cells —
    /// projecting the current display matrix into `pane.matrix` and
    /// reporting `Recomputed` so the cell renderers repaint current content.
    #[test]
    fn worker_projects_lagging_cells_after_sync_rebuild() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of_versioned("aa\nbb\ncc", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        assert_eq!(matrix_cell.load().version.text, 1);

        // Edit: insert a line. Build rs2 and run ONLY the sync rebuild
        // (mimicking the actor publish tail) — it updates display_matrix
        // but deliberately not the cell grid. `syntax: 1` mirrors reality:
        // the reparse hasn't landed at edit-publish time.
        let snap2 = snap_of_versioned("aa\nNEW\nbb\ncc", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(1, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        {
            let loaded = rs2.load_full();
            let cells = loaded.cells.load();
            let pane = &cells.panes[0];
            assert!(sync_rebuild_pane_on_edit(
                pane,
                CellTheme {
                    resolved: &cells.resolved_theme,
                    ids: &cells.theme_ids,
                },
                &cells.whitespace
            ));
        }
        let dm_cell = display_cell_for(&matrix_cell);
        assert_eq!(
            dm_cell.load().version.text,
            2,
            "display matrix is current after sync"
        );
        assert_eq!(
            matrix_cell.load().version.text,
            1,
            "cell grid still lags (sync deliberately did not project)"
        );

        // The async worker reconciles the lagging cells on its next wake.
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
        let cm = matrix_cell.load();
        assert_eq!(
            cm.version.text, 2,
            "worker projected the current display matrix into the cells"
        );
        let row1: String = cm
            .slice(0, 10)
            .iter()
            .nth(1)
            .unwrap()
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('?'))
            .collect();
        assert_eq!(row1, "NEW");
    }

    /// H.1 (2026-06-04): the range-scoped highlight must colour the
    /// EDITED line correctly — i.e. `spans_base` relative indexing lands
    /// the scoped spans on the right line. Edits line 1 in place to
    /// introduce a Rust `fn` keyword, with the syntax snapshot parsed at
    /// the post-edit version; the incremental rebuild highlights only
    /// `[edit_lo, affected_hi)` (base = edit_lo), so a base/index slip
    /// would either miscolour or skip the keyword.
    #[tokio::test]
    async fn h1_scoped_highlight_colours_the_edited_line() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let keyword_fg = resolve_style(ct, lattice_syntax::Style::Keyword).0;

        // v1: plain seed (no syntax handle) → prior whole-doc matrix.
        let snap1 = snap_of_versioned("let a = 1;\nlet b = 2;\nlet c = 3;\n", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme.clone(),
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);

        // v2: edit line 1 in place → introduces `fn`. Rust syntax parsed
        // at v2 + seeded so the scoped highlight has fresh spans.
        let text2 = "let a = 1;\nfn b() {}\nlet c = 3;\n";
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .unwrap();
        s.parse_at(text2, 2);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded_with_runtime(
            s,
            &tokio::runtime::Handle::current(),
            None,
        ));
        let snap2 = snap_of_versioned(text2, 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(1, 1, 1); // in-place edit of line 1
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            Some(handle),
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);

        let m = matrix_cell.load_full();
        let line1 = m.row_at_source_line(1).expect("line 1 row present");
        assert!(
            line1.cells.iter().any(|c| c.fg == keyword_fg),
            "the scoped highlight (base = edit_lo) must colour the edited line's \
             `fn` keyword — guards `spans_base` relative indexing"
        );
    }

    /// Chunked mode + single-edit reuses prefix chunks (by `Arc`
    /// identity) and shifts suffix chunks. Concretely: 25-line
    /// document, viewport 5 → chunk_size 16, an insert at line 2
    /// rebuilds chunk 0 only; chunk 1 (starts at 16) shifts to
    /// start at 17.
    #[test]
    fn chunked_incremental_reuses_prefix_and_shifts_suffix() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        // 25 lines: "l0\nl1\n...\nl24".
        let text1: String = (0..25)
            .map(|i| format!("l{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap1 = snap_of_versioned(&text1, 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5, // 4×5 = 20 < 25 ⇒ chunked, chunk_size = 16
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let m1 = matrix_cell.load();
        assert_eq!(m1.chunk_size, 16);
        assert_eq!(m1.chunks.len(), 2);
        // Hold an `Arc` clone of cell chunk 1 for the post-edit
        // identity check (the projected cell chunk is rebuilt, so it
        // must be a NEW Arc). B2.2: the payload-sharing guarantee now
        // lives on the canonical `DisplayMatrix`, so also capture its
        // suffix chunk's `DisplayLine` `text` Arcs to assert reuse.
        let chunk1_pre: Arc<CellChunk> = Arc::clone(&m1.chunks[1]);
        let dm_cell = display_cell_for(&matrix_cell);
        let d1 = dm_cell.load_full();
        assert_eq!(d1.chunks.len(), 2, "display matrix mirrors cell chunking");
        let disp_chunk1_pre: Vec<Arc<str>> =
            d1.chunks[1].rows.iter().map(|r| Arc::clone(&r.text)).collect();

        // Insert one line at line 2.
        let text2: String = {
            let mut lines: Vec<String> = (0..25).map(|i| format!("l{}", i)).collect();
            lines.insert(2, "INS".to_string());
            lines.join("\n")
        };
        let snap2 = snap_of_versioned(&text2, 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(2, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);
        let m2 = matrix_cell.load();
        // Post-edit shape: chunked, chunk_size 16, 26 source lines.
        // Partitioning:
        // - prefix-reuse: none (old chunk 0 ends at 16 > edit_lo=2)
        // - rebuild zone: [0, 17) (suffix-shift's first chunk lands
        //   at start=17). Carved into chunks at start=0 (16 rows)
        //   and start=16 (1 row).
        // - suffix-shift: old chunk 1 → start=17 (9 rows).
        // ⇒ three chunks at starts [0, 16, 17] totalling 26 rows.
        assert!(!m2.is_whole_doc());
        assert_eq!(m2.chunk_size, 16);
        assert_eq!(m2.source_line_count, 26);
        assert_eq!(m2.visible_line_count, 26);
        assert_eq!(m2.chunks.len(), 3);
        assert_eq!(m2.chunks[0].start_source_line, 0);
        assert_eq!(m2.chunks[1].start_source_line, 16);
        assert_eq!(m2.chunks[2].start_source_line, 17);

        // m2.chunks[2] is the shifted-from-m1.chunks[1] one.
        // Rows whose source_line was 16..25 now have source_lines
        // 17..26.
        let shifted_lines: Vec<u32> = m2.chunks[2].rows.iter().map(|r| r.source_line).collect();
        assert_eq!(
            shifted_lines,
            (17u32..26).collect::<Vec<_>>(),
            "suffix chunk rows must be the same as before but shifted by +1"
        );

        // The projected cell chunk is always a fresh Arc (cells are
        // re-projected each rebuild), so it differs from the pre-edit
        // one — the payload-sharing guarantee lives on the display
        // matrix now, asserted below.
        assert!(
            !Arc::ptr_eq(&chunk1_pre, &m2.chunks[2]),
            "post-shift projected cell chunk is a new Arc<CellChunk>"
        );

        // B2.2: DisplayLine payload sharing across the shift — the
        // canonical guarantee. The suffix chunk's surviving
        // `DisplayLine`s reuse their `text` Arc (shifted source_line,
        // shared payload), which is what keeps their colours stable.
        let d2 = dm_cell.load_full();
        let disp_suffix = d2
            .chunks
            .iter()
            .find(|c| c.start_source_line == 17)
            .expect("display matrix has the shifted suffix chunk at start 17");
        assert_eq!(disp_suffix.rows.len(), disp_chunk1_pre.len());
        for (pre_text, post_row) in disp_chunk1_pre.iter().zip(disp_suffix.rows.iter()) {
            assert!(
                Arc::ptr_eq(pre_text, &post_row.text),
                "DisplayLine text Arc must be shared across the suffix shift"
            );
        }
    }

    /// 2026-06-04 (B-flicker fix): in CHUNKED mode, a single in-place edit
    /// in the MIDDLE of a chunk must rebuild ONLY the edited line; every
    /// other line in that chunk reuses its prior `DisplayLine` payload
    /// (`text` AND the colour-carrying `runs` Arc). The prior code rebuilt
    /// the whole `chunk_size`-aligned zone wholesale, so on the sync edit
    /// path (`allow_highlight: false`) ~`chunk_size` lines lost their syntax
    /// colour on every keystroke — the whole-viewport markdown flicker,
    /// since the viewport sits inside one chunk. Whole-doc mode already
    /// reused rows (`whole_doc_incremental_reuses_unchanged_rows`); this is
    /// the missing chunked-mode guarantee. (feedback_decorations_update_in_place)
    #[test]
    fn chunked_incremental_reuses_rows_in_rebuild_zone() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        // 25 lines → viewport 5 ⇒ chunked, chunk_size 16, chunks [0,16)+[16,25).
        let text1: String = (0..25)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap1 = snap_of_versioned(&text1, 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let dm_cell = display_cell_for(&matrix_cell);
        let d1 = dm_cell.load_full();
        assert_eq!(d1.chunk_size, 16);
        assert_eq!(d1.chunks.len(), 2, "25 lines ⇒ two chunks");

        // Capture prior payload Arcs for lines that must survive the edit:
        // a prefix line (0), the line just before the edit (4), and a line
        // AFTER the edit but still inside the same chunk (10). All three
        // were rebuilt fresh by the buggy wholesale-zone path.
        let l0 = d1.row_at_source_line(0).unwrap();
        let (l0_text, l0_runs) = (Arc::clone(&l0.text), Arc::clone(&l0.runs));
        let l4_text = Arc::clone(&d1.row_at_source_line(4).unwrap().text);
        let l10 = d1.row_at_source_line(10).unwrap();
        let (l10_text, l10_runs) = (Arc::clone(&l10.text), Arc::clone(&l10.runs));
        let l5_text_pre = Arc::clone(&d1.row_at_source_line(5).unwrap().text);

        // Edit line 5 IN PLACE (removed 1, added 1 → net 0): the edit lands
        // inside chunk 0, so the rebuild zone is the whole chunk [0,16).
        let text2: String = {
            let mut lines: Vec<String> = (0..25).map(|i| format!("line{}", i)).collect();
            lines[5] = "EDITED".to_string();
            lines.join("\n")
        };
        let snap2 = snap_of_versioned(&text2, 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(5, 1, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);
        let d2 = dm_cell.load_full();
        assert_eq!(d2.source_line_count, 25);

        // Unchanged lines reuse their prior DisplayLine payload — both the
        // text AND the colour-carrying runs Arc. This is the regression: the
        // old wholesale-zone rebuild produced fresh Arcs here (blanking colour
        // on the sync path), now confined to the single edited line.
        let l0_post = d2.row_at_source_line(0).unwrap();
        assert!(
            Arc::ptr_eq(&l0_text, &l0_post.text) && Arc::ptr_eq(&l0_runs, &l0_post.runs),
            "prefix line 0 must reuse its prior text+runs Arcs (keeps colour)"
        );
        assert!(
            Arc::ptr_eq(&l4_text, &d2.row_at_source_line(4).unwrap().text),
            "line 4 (just before edit) must reuse its prior text Arc"
        );
        let l10_post = d2.row_at_source_line(10).unwrap();
        assert!(
            Arc::ptr_eq(&l10_text, &l10_post.text) && Arc::ptr_eq(&l10_runs, &l10_post.runs),
            "line 10 (after edit, same chunk) must reuse its prior text+runs Arcs — \
             the buggy wholesale-zone rebuild lost these"
        );

        // Only the edited line is rebuilt: fresh payload, new content.
        let l5_post = d2.row_at_source_line(5).unwrap();
        assert!(
            !Arc::ptr_eq(&l5_text_pre, &l5_post.text),
            "edited line 5 must be rebuilt (fresh text Arc)"
        );
        assert_eq!(&*l5_post.text, "EDITED", "edited line carries the new content");
    }

    /// Eligibility falls back to full rebuild when `last_edit` is
    /// `None` (no single-edit since last publish — e.g.
    /// undo/redo/multi-edit batch).
    #[test]
    fn no_last_edit_falls_back_to_full_rebuild() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of_versioned("aa\nbb", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);

        // text bumps but last_edit stays None (multi-edit batch
        // semantics).
        let snap2 = snap_of_versioned("AA\nBB", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
    }

    /// Eligibility falls back to full rebuild when a non-text
    /// axis (e.g. theme) also bumped — incremental can't safely
    /// reuse cell colours.
    #[test]
    fn theme_axis_change_disables_incremental() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of_versioned("aa\nbb", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            theme: 100,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);

        // Theme bumps alongside text — incremental must bail.
        let snap2 = snap_of_versioned("aa\nNEW\nbb", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            theme: 200,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(1, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        // Full rebuild — incremental rejected because theme axis
        // differs.
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
    }

    /// Mismatched line-count guard: even with a single-edit
    /// delta, if the published matrix's `source_line_count` plus
    /// `net_delta` doesn't match the new snapshot's line count,
    /// incremental bails (defensive against doc-switches where
    /// versions coincidentally line up).
    #[test]
    fn line_count_mismatch_disables_incremental() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let snap1 = snap_of_versioned("aa\nbb", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);

        // New snapshot has 5 lines, but the edit says
        // lines_added=1 against pre 2 → expects 3, not 5. Bail
        // to full rebuild.
        let snap2 = snap_of_versioned("a\nb\nc\nd\ne", 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(0, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
    }

    /// Eligibility falls back when the mode would change between
    /// pre and post edit (e.g. small-doc whole-doc → chunked
    /// after adding enough lines).
    #[test]
    fn mode_change_disables_incremental() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // 5 lines + viewport 5 → whole-doc (5 <= 20).
        let snap1 = snap_of_versioned("a\nb\nc\nd\ne", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        assert!(matrix_cell.load().is_whole_doc());

        // Bump to 30 lines (5 + 25 inserted). Now 30 > 20 → chunked
        // mode. Single-edit delta says lines_added=25; that crosses
        // the threshold.
        let text2: String = (0..30)
            .map(|i| format!("l{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap2 = snap_of_versioned(&text2, 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(5, 0, 25);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        // Mode flipped; incremental must bail.
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
        assert!(!matrix_cell.load().is_whole_doc());
    }

    /// Chunked deletion: removing lines also takes the incremental
    /// branch; downstream chunks shift by the negative delta.
    #[test]
    fn chunked_incremental_handles_deletion() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        // 30 lines so we land squarely in chunked mode at viewport
        // 5 (chunk_size 16, ceil(30/16) = 2 chunks).
        let text1: String = (0..30)
            .map(|i| format!("l{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let snap1 = snap_of_versioned(&text1, 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs1 = rs_with_everything(
            Some(snap1),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let m1 = matrix_cell.load();
        assert_eq!(m1.chunk_size, 16);
        assert_eq!(m1.chunks.len(), 2);

        // Delete two lines at line 3.
        let text2: String = {
            let mut lines: Vec<String> = (0..30).map(|i| format!("l{}", i)).collect();
            lines.drain(3..5);
            lines.join("\n")
        };
        let snap2 = snap_of_versioned(&text2, 2);
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let edit = edit_delta(3, 2, 0);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);
        let m2 = matrix_cell.load();
        assert_eq!(m2.source_line_count, 28);
        assert_eq!(m2.visible_line_count, 28);
        // Walk source_lines via slice — must be 0..28 contiguous.
        let source_lines: Vec<u32> = m2.slice(0, 100).iter().map(|r| r.source_line).collect();
        assert_eq!(source_lines, (0u32..28).collect::<Vec<_>>());
    }

    // ---- S2.5 — coalescing + paint_request + end-to-end ----

    /// `recompute` walks the [`WorkerDecision`] state machine
    /// monotonically when wakes are interleaved with publishes:
    /// first publish ⇒ Recomputed; same-version wake ⇒ CacheHit;
    /// new edit ⇒ RecomputedIncremental; multi-axis change ⇒
    /// Recomputed (full). This is the synchronous projection of
    /// the burst-coalescing contract: each iteration of the async
    /// `run` loop reads the *latest* RenderState and never
    /// processes stale intermediate states.
    #[test]
    fn coalescing_walks_decision_state_machine() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();

        // Tick 1: initial publish, no prior matrix → full Recomputed.
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let snap1 = snap_of_versioned("aa\nbb\ncc", 1);
        let rs1 = rs_with_everything(
            Some(snap1.clone()),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);

        // Tick 2: same RenderState, redundant wake ⇒ CacheHit.
        assert_eq!(recompute(&rs1), WorkerDecision::CacheHit);
        assert_eq!(recompute(&rs1), WorkerDecision::CacheHit);

        // Tick 3: single edit, all other axes unchanged →
        // incremental.
        let v2 = MatrixVersion {
            text: 2,
            syntax: 2,
            ..MatrixVersion::ZERO
        };
        let snap2 = snap_of_versioned("aa\nNEW\nbb\ncc", 2);
        let edit = edit_delta(1, 0, 1);
        let rs2 = rs_with_everything(
            Some(snap2),
            v2,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit),
            5,
        );
        assert_eq!(recompute(&rs2), WorkerDecision::RecomputedIncremental);

        // Tick 4: theme axis bump alongside text — incremental
        // bails, full rebuild runs.
        let v3 = MatrixVersion {
            text: 3,
            syntax: 3,
            theme: 7,
            ..MatrixVersion::ZERO
        };
        let snap3 = snap_of_versioned("aa\nNEW\nbb\ncc\nDD", 3);
        let edit3 = edit_delta(4, 0, 1);
        let rs3 = rs_with_everything(
            Some(snap3),
            v3,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            Some(edit3),
            5,
        );
        assert_eq!(recompute(&rs3), WorkerDecision::Recomputed);
    }

    /// Cells worker and highlights worker write to independent
    /// `ArcSwap` cells. Driving cells `recompute` does NOT touch
    /// the spans cell, and (vice versa) driving the highlights
    /// worker does not corrupt the matrix cell. The shared
    /// `RenderState` substrate stays consistent across both.
    #[test]
    fn cells_worker_does_not_corrupt_spans_cell() {
        let theme = crate::ui::theme::Theme::default();
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let spans_cell: Arc<ArcSwap<crate::render_state::VisibleSpans>> = Arc::default();
        let rows_cell: Arc<ArcSwap<crate::render_state::VisibleRows>> = Arc::default();
        let overlay_cell: Arc<ArcSwap<crate::render_state::StaticOverlayQuads>> = Arc::default();

        // Seed a sentinel value into the spans cell so we can
        // detect any unintended mutation.
        let sentinel_key = crate::render_state::VisibleHighlightsKey {
            snapshot_ptr: 0xdead_beef,
            ..Default::default()
        };
        spans_cell.store(Arc::new(crate::render_state::VisibleSpans {
            spans: Arc::from(Vec::new().into_boxed_slice()),
            computed_for_key: sentinel_key,
        }));
        let pre_spans_ptr = Arc::as_ptr(&spans_cell.load_full());

        // Run a cells recompute against a RenderState that
        // happens to share the same `render_state` Arc shape. The
        // cells path must not touch spans_cell / rows_cell /
        // overlay_cell.
        let snap = snap_of_versioned("aa\nbb", 1);
        let v1 = MatrixVersion {
            text: 1,
            syntax: 1,
            ..MatrixVersion::ZERO
        };
        let rs = rs_with_everything(
            Some(snap),
            v1,
            matrix_cell.clone(),
            None,
            theme,
            Vec::new(),
            Vec::new(),
            true,
            None,
            5,
        );
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        // Spans cell remains at the sentinel — Arc identity
        // unchanged.
        let post_spans_ptr = Arc::as_ptr(&spans_cell.load_full());
        assert_eq!(
            pre_spans_ptr, post_spans_ptr,
            "cells recompute must not touch the spans cell"
        );

        // Matrix cell has been updated.
        assert!(!matrix_cell.load().is_empty());

        // Hold the cells unused so they're not optimised away.
        let _ = (rows_cell, overlay_cell);
    }

    /// End-to-end smoke test through the actual `run` loop in a
    /// tokio runtime. Drives:
    /// - publish + wake → matrix populates → paint_request fires;
    /// - second publish with new text → matrix updates; second
    ///   paint_request wake;
    /// - same-state wake → matrix unchanged (cache-hit); no
    ///   additional paint_request beyond the first two.
    ///
    /// Uses `tokio::time::timeout` so a regression that leaves the
    /// worker parked surfaces as a test failure, not a hang.
    #[test]
    fn end_to_end_through_tokio_run_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async move {
            use tokio::time::{Duration, timeout};

            let theme = crate::ui::theme::Theme::default();
            let render_state: Arc<ArcSwap<RenderState>> =
                Arc::new(ArcSwap::from_pointee(RenderState::default()));
            let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
            let wake = crate::editor::CellsWake::default();
            let paint_request: Arc<tokio::sync::Notify> = Arc::default();

            // Spawn the worker.
            let handle = tokio::spawn(crate::cells_worker::run(
                render_state.clone(),
                wake.clone(),
                paint_request.clone(),
            ));

            // Helper: build + atomically publish a fresh
            // RenderState (single-pane), then fire the wake.
            let publish =
                |text: &str, text_version: u64, last_edit: Option<lattice_cells::EditDelta>| {
                    let snap = snap_of_versioned(text, text_version);
                    let v = MatrixVersion {
                        text: text_version,
                        syntax: text_version,
                        ..MatrixVersion::ZERO
                    };
                    let inputs = crate::render_state::PaneCellsInputs {
                        pane_id: lattice_core::ui::pane::PaneId::default(),
                        buffer_id: lattice_core::BufferId::default(),
                        matrix: matrix_cell.clone(),
                        display_matrix: Arc::new(ArcSwap::from_pointee(
                            crate::display_matrix::DisplayMatrix::empty(),
                        )),
                        virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(
                            lattice_cells::VirtualRowMatrix::empty(),
                        )),
                        version: v,
                        snapshot: Some(snap.clone()),
                        syntax_handle: None,
                        inlay_hints: Arc::from(
                            Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                        ),
                        folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
                        viewport_height: 5,
                        scroll: 0,
                        viewport_width: 0,
                        wrap: false,
                        foldenable: true,
                        last_edit,
                        excerpt_syntax: Arc::from([]),
                    };
                    let cells = CellsRenderState {
                        matrix: matrix_cell.clone(),
                        version: v,
                        snapshot: Some(snap),
                        syntax_handle: None,
                        inlay_hints: Arc::from(
                            Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                        ),
                        folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
                        viewport_height: 5,
                        foldenable: true,
                        last_edit,
                        theme,
                        resolved_theme: std::sync::Arc::new(
                            crate::ui::theme::ResolvedTheme::default(),
                        ),
                        theme_ids: crate::ui::theme::BuiltinElementIds::default(),
                        whitespace: WhitespaceConfig::default(),
                        panes: Arc::from(vec![inputs.clone()].into_boxed_slice()),
                        pane_matrices: {
                            let mut m = std::collections::HashMap::new();
                            m.insert(inputs.pane_id, inputs.matrix);
                            Arc::new(m)
                        },
                        display_matrix: Arc::new(ArcSwap::from_pointee(
                            crate::display_matrix::DisplayMatrix::empty(),
                        )),
                        display_pane_matrices: Arc::new(std::collections::HashMap::new()),
                    };
                    let rs = RenderState {
                        cells: Arc::new(ArcSwap::from_pointee(cells)),
                        ..RenderState::default()
                    };
                    render_state.store(Arc::new(rs));
                    wake.0.notify_one();
                };

            // Publish #1 — fresh document.
            publish("aa\nbb\ncc", 1, None);
            timeout(Duration::from_secs(2), paint_request.notified())
                .await
                .expect("paint_request must fire after first publish");
            assert_eq!(matrix_cell.load().source_line_count, 3);

            // Publish #2 — single-line insert; should take
            // incremental path.
            publish(
                "aa\nNEW\nbb\ncc",
                2,
                Some(lattice_cells::EditDelta {
                    start_line: 1,
                    lines_removed: 0,
                    lines_added: 1,
                }),
            );
            timeout(Duration::from_secs(2), paint_request.notified())
                .await
                .expect("paint_request must fire after second publish");
            let m2 = matrix_cell.load();
            assert_eq!(m2.source_line_count, 4);
            // Row content reflects post-edit text.
            let texts: Vec<String> = m2
                .slice(0, 10)
                .iter()
                .map(|r| {
                    r.cells
                        .iter()
                        .map(|c| char::from_u32(c.codepoint).unwrap_or('?'))
                        .collect()
                })
                .collect();
            assert_eq!(texts, vec!["aa", "NEW", "bb", "cc"]);

            // Redundant wake against the same RenderState — worker
            // must hit CacheHit and NOT fire paint_request.
            wake.0.notify_one();
            // Brief wait window for the worker to process; expect
            // timeout (no paint wake fires).
            let no_paint = timeout(Duration::from_millis(150), paint_request.notified()).await;
            assert!(
                no_paint.is_err(),
                "redundant wake must produce CacheHit, not a paint signal"
            );

            handle.abort();
            let _ = handle.await;
        });
    }

    /// Burst-coalescing smoke test: fire many wakes in quick
    /// succession with the *same* RenderState; the worker must
    /// process them as a coalesced batch and produce at most one
    /// paint signal (after the initial publish). Mirrors the
    /// design comment: a burst of N publishes during one build
    /// produces exactly 2 builds (the original + one tail catch-
    /// up).
    #[test]
    fn burst_wakes_coalesce_via_notify_permit() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async move {
            use tokio::time::{Duration, timeout};

            let theme = crate::ui::theme::Theme::default();
            let render_state: Arc<ArcSwap<RenderState>> =
                Arc::new(ArcSwap::from_pointee(RenderState::default()));
            let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
            let wake = crate::editor::CellsWake::default();
            let paint_request: Arc<tokio::sync::Notify> = Arc::default();

            let handle = tokio::spawn(crate::cells_worker::run(
                render_state.clone(),
                wake.clone(),
                paint_request.clone(),
            ));

            // Publish one RenderState and fire ten wakes in a
            // row. `Notify` collapses them to at most one
            // additional permit beyond the first.
            let snap = snap_of_versioned("hello\nworld", 1);
            let v = MatrixVersion {
                text: 1,
                syntax: 1,
                ..MatrixVersion::ZERO
            };
            let inputs = crate::render_state::PaneCellsInputs {
                pane_id: lattice_core::ui::pane::PaneId::default(),
                buffer_id: lattice_core::BufferId::default(),
                matrix: matrix_cell.clone(),
                display_matrix: Arc::new(ArcSwap::from_pointee(
                    crate::display_matrix::DisplayMatrix::empty(),
                )),
                virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(
                    lattice_cells::VirtualRowMatrix::empty(),
                )),
                version: v,
                snapshot: Some(snap.clone()),
                syntax_handle: None,
                inlay_hints: Arc::from(
                    Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                ),
                folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
                viewport_height: 5,
                scroll: 0,
                viewport_width: 0,
                wrap: false,
                foldenable: true,
                last_edit: None,
                excerpt_syntax: Arc::from([]),
            };
            let cells = CellsRenderState {
                matrix: matrix_cell.clone(),
                version: v,
                snapshot: Some(snap),
                syntax_handle: None,
                inlay_hints: Arc::from(
                    Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                ),
                folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
                viewport_height: 5,
                foldenable: true,
                last_edit: None,
                theme,
                resolved_theme: std::sync::Arc::new(crate::ui::theme::ResolvedTheme::default()),
                theme_ids: crate::ui::theme::BuiltinElementIds::default(),
                whitespace: WhitespaceConfig::default(),
                panes: Arc::from(vec![inputs.clone()].into_boxed_slice()),
                pane_matrices: {
                    let mut m = std::collections::HashMap::new();
                    m.insert(inputs.pane_id, inputs.matrix);
                    Arc::new(m)
                },
                display_matrix: Arc::new(ArcSwap::from_pointee(
                    crate::display_matrix::DisplayMatrix::empty(),
                )),
                display_pane_matrices: Arc::new(std::collections::HashMap::new()),
            };
            render_state.store(Arc::new(RenderState {
                cells: Arc::new(ArcSwap::from_pointee(cells)),
                ..RenderState::default()
            }));
            for _ in 0..10 {
                wake.0.notify_one();
            }

            // First paint fires for the initial Recomputed.
            timeout(Duration::from_secs(2), paint_request.notified())
                .await
                .expect("paint_request must fire for first build");

            // The remaining wakes all see the same RenderState
            // → CacheHit → no further paint signals. Wait briefly
            // and confirm no extra wake arrives.
            let drained = timeout(Duration::from_millis(150), paint_request.notified()).await;
            assert!(
                drained.is_err(),
                "redundant wakes must coalesce to CacheHit, no extra paint signal"
            );

            handle.abort();
            let _ = handle.await;
        });
    }

    // ---- D.4.d.1.b (per-pane iteration) ----

    /// Build a single `PaneCellsInputs` with sensible defaults for
    /// the multi-pane tests below. Each test bumps the bits it
    /// cares about; everything else stays at the default.
    fn pane_inputs(
        matrix: Arc<ArcSwap<CellMatrix>>,
        snapshot: Option<Arc<DocumentSnapshot>>,
        version: MatrixVersion,
        viewport_height: u32,
    ) -> crate::render_state::PaneCellsInputs {
        crate::render_state::PaneCellsInputs {
            pane_id: lattice_core::ui::pane::PaneId::default(),
            buffer_id: lattice_core::BufferId::default(),
            matrix,
            display_matrix: Arc::new(ArcSwap::from_pointee(
                crate::display_matrix::DisplayMatrix::empty(),
            )),
            virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(
                lattice_cells::VirtualRowMatrix::empty(),
            )),
            version,
            snapshot,
            syntax_handle: None,
            inlay_hints: Arc::from(
                Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
            ),
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            viewport_height,
            scroll: 0,
            viewport_width: 0,
            wrap: false,
            foldenable: true,
            last_edit: None,
            excerpt_syntax: Arc::from([]),
        }
    }

    /// Build an `ArcSwap<RenderState>` whose `cells.panes` carries
    /// the caller-supplied entries verbatim. Top-level inputs stay
    /// at default — the worker now reads from `panes`.
    fn rs_with_panes(entries: Vec<crate::render_state::PaneCellsInputs>) -> ArcSwap<RenderState> {
        let cells = CellsRenderState {
            panes: Arc::from(entries.into_boxed_slice()),
            ..CellsRenderState::default()
        };
        ArcSwap::from_pointee(RenderState {
            cells: Arc::new(ArcSwap::from_pointee(cells)),
            ..RenderState::default()
        })
    }

    /// W.2 (A2): the worker stamps `CellMatrix.wrap_width` from the
    /// pane's effective wrap width, and a wrap toggle invalidates
    /// the cache even when the content version is unchanged.
    #[test]
    fn recompute_pane_stamps_wrap_width_and_invalidates_on_toggle() {
        let snap = snap_of("a line that is clearly wider than the wrap width\n");
        let matrix_cell = Arc::new(ArcSwap::from_pointee(CellMatrix::empty()));
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };
        let ws = WhitespaceConfig::default();

        // Wrap off ⇒ stamped width 0 (one display row per line).
        let mut p = pane_inputs(matrix_cell.clone(), Some(snap), v(1), 10);
        let _ = recompute_pane(&p, ct, &ws);
        assert_eq!(matrix_cell.load().wrap_width, 0);
        assert_eq!(matrix_cell.load().segment_count(0), 1);

        // Wrap on at width 8 — same content version v(1), so only
        // the wrap_width differs. The cache-hit guard must still
        // force a rebuild that re-stamps the new width, and the
        // long line now spans multiple display segments.
        p.wrap = true;
        p.viewport_width = 8;
        let decision = recompute_pane(&p, ct, &ws);
        assert!(
            matches!(
                decision,
                WorkerDecision::Recomputed | WorkerDecision::RecomputedIncremental
            ),
            "wrap toggle at unchanged version must rebuild, got {decision:?}"
        );
        let m = matrix_cell.load();
        assert_eq!(m.wrap_width, 8);
        assert!(m.segment_count(0) > 1, "long line wraps into multiple segments");
    }

    /// W.4.t: a hard tab expands to `tabstop` columns of cells, so
    /// `col_count` reflects the true display width (one width model
    /// for host scroll + renderers). Whitespace off ⇒ plain spaces;
    /// whitespace on with a tab glyph ⇒ the marker leads, spaces
    /// fill (respects `display.whitespace.tab`).
    #[test]
    fn recompute_pane_expands_tabs_to_tabstop_width() {
        let snap = snap_of("\tab");
        let (resolved, ids) = test_cell_theme();
        let ct = CellTheme {
            resolved: &resolved,
            ids: &ids,
        };

        // Whitespace off, tabstop 4 ⇒ leading tab → 4 space cells,
        // then `ab` ⇒ col_count 6. No WS_MARKER, no literal `\t`.
        let plain = WhitespaceConfig {
            tabstop: 4,
            ..Default::default()
        };
        let matrix_cell = Arc::new(ArcSwap::from_pointee(CellMatrix::empty()));
        let p = pane_inputs(matrix_cell.clone(), Some(snap.clone()), v(1), 10);
        let _ = recompute_pane(&p, ct, &plain);
        let m = matrix_cell.load();
        let row = m.row_at_source_line(0).expect("row 0");
        assert_eq!(row.col_count(), 6, "tab(4) + 'ab'(2)");
        assert!(
            row.cells[..4].iter().all(|c| c.codepoint == ' ' as u32),
            "leading tab expands to 4 spaces"
        );
        assert!(
            row.cells[..4]
                .iter()
                .all(|c| !c.is_ws_marker()),
            "whitespace off ⇒ no marker flag"
        );

        // Whitespace on with a tab glyph ⇒ first column is the
        // marker, the next 3 are space fill, all WS_MARKER.
        let marked = WhitespaceConfig {
            show: true,
            tab: Some('→'),
            tabstop: 4,
            ..Default::default()
        };
        let matrix_cell2 = Arc::new(ArcSwap::from_pointee(CellMatrix::empty()));
        let p2 = pane_inputs(matrix_cell2.clone(), Some(snap), v(1), 10);
        let _ = recompute_pane(&p2, ct, &marked);
        let m2 = matrix_cell2.load();
        let row2 = m2.row_at_source_line(0).expect("row 0");
        assert_eq!(row2.col_count(), 6);
        assert_eq!(row2.cells[0].codepoint, '→' as u32, "marker leads the tab");
        assert!(row2.cells[0].is_ws_marker());
        assert!(
            row2.cells[1..4].iter().all(|c| c.codepoint == ' ' as u32),
            "fill columns are spaces"
        );
    }

    /// Two visible Document panes with distinct buffers (distinct
    /// matrix cells) both rebuild on a single tick. Each pane's
    /// own cell receives a fresh `CellMatrix`; cross-pane writes
    /// don't bleed.
    #[test]
    fn two_panes_with_distinct_buffers_both_rebuild() {
        let snap_a = snap_of_versioned("aa\nbb", 1);
        let snap_b = snap_of_versioned("xx\nyy\nzz", 1);
        let cell_a: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let cell_b: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let mut a = pane_inputs(cell_a.clone(), Some(snap_a), v(1), 5);
        let mut b = pane_inputs(cell_b.clone(), Some(snap_b), v(1), 5);
        a.buffer_id = lattice_core::BufferId(1);
        b.buffer_id = lattice_core::BufferId(2);
        let rs = rs_with_panes(vec![a, b]);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        // Pane A: 2 source lines from "aa\nbb".
        assert_eq!(cell_a.load().source_line_count, 2);
        // Pane B: 3 source lines from "xx\nyy\nzz".
        assert_eq!(cell_b.load().source_line_count, 3);
    }

    /// Cache-hit per pane: one pane sees a version bump and
    /// rebuilds; the other pane's inputs match its published
    /// matrix and skip work. The aggregate decision is
    /// `Recomputed` because at least one pane changed.
    #[test]
    fn per_pane_cache_hit_skips_unchanged_pane() {
        let snap_a = snap_of_versioned("aa\nbb", 1);
        let snap_b = snap_of_versioned("xx\nyy", 1);
        let cell_a: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let cell_b: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let mut a = pane_inputs(cell_a.clone(), Some(snap_a.clone()), v(1), 5);
        let mut b = pane_inputs(cell_b.clone(), Some(snap_b.clone()), v(1), 5);
        a.buffer_id = lattice_core::BufferId(1);
        b.buffer_id = lattice_core::BufferId(2);
        // First tick: both build.
        let rs1 = rs_with_panes(vec![a.clone(), b.clone()]);
        assert_eq!(recompute(&rs1), WorkerDecision::Recomputed);
        let a_arc_after_tick1 = cell_a.load_full();
        let b_arc_after_tick1 = cell_b.load_full();
        // Second tick: bump pane A's text version; pane B unchanged.
        let snap_a2 = snap_of_versioned("aa\nbb\ncc", 2);
        a.version = v(2);
        a.snapshot = Some(snap_a2);
        let rs2 = rs_with_panes(vec![a, b]);
        assert_eq!(recompute(&rs2), WorkerDecision::Recomputed);
        // Pane A rebuilt — Arc identity changed.
        assert!(
            !Arc::ptr_eq(&a_arc_after_tick1, &cell_a.load_full()),
            "pane A must rebuild on version bump"
        );
        assert_eq!(cell_a.load().source_line_count, 3);
        // Pane B was a cache hit — Arc identity preserved.
        assert!(
            Arc::ptr_eq(&b_arc_after_tick1, &cell_b.load_full()),
            "pane B was a cache hit; Arc identity must survive"
        );
        assert_eq!(cell_b.load().source_line_count, 2);
    }

    /// Two panes showing the same buffer share one matrix cell.
    /// The first pane rebuilds; the second pane sees a CacheHit
    /// against the freshly-published matrix. Aggregate is
    /// `Recomputed` (one entry produced content).
    #[test]
    fn two_panes_sharing_buffer_share_one_matrix_write() {
        let snap = snap_of_versioned("aa\nbb", 1);
        let shared_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let mut a = pane_inputs(shared_cell.clone(), Some(snap.clone()), v(1), 5);
        let mut b = pane_inputs(shared_cell.clone(), Some(snap), v(1), 5);
        // Same buffer, distinct pane ids — matches what
        // `Editor::cells_matrix_for` returns when two panes show
        // the same buffer.
        let pid = lattice_core::ui::pane::PaneId::default();
        a.pane_id = pid;
        b.pane_id = pid; // ids match because PaneId::default() is the same; the test
        a.buffer_id = lattice_core::BufferId(42);
        b.buffer_id = lattice_core::BufferId(42);
        let rs = rs_with_panes(vec![a, b]);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);
        assert_eq!(shared_cell.load().source_line_count, 2);
    }

    /// Empty `panes` → no work; aggregate is `CacheHit`. Used by
    /// editors with no Document leaves visible (eg every pane
    /// showing a synthetic buffer).
    #[test]
    fn empty_panes_is_cache_hit() {
        let rs = rs_with_panes(Vec::new());
        assert_eq!(recompute(&rs), WorkerDecision::CacheHit);
    }

    /// A pane with `snapshot: None` clears its matrix. The
    /// aggregate decision is `Clear` when no other pane saw a
    /// content-producing rebuild.
    #[test]
    fn pane_without_snapshot_clears_its_matrix() {
        let cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Seed a non-empty matrix so the Clear branch exercises
        // the store path (not the idempotent fast path).
        let pre_chunk = Arc::new(CellChunk::new(
            0,
            vec![CellRow::new(
                vec![Cell::with_codepoint(b'x' as u32)],
                0,
                Vec::<lattice_cells::row::InlayOffset>::new(),
            )],
            v(7),
        ));
        cell.store(Arc::new(CellMatrix::whole_doc(pre_chunk, 1)));
        let pane = pane_inputs(cell.clone(), None, v(7), 5);
        let rs = rs_with_panes(vec![pane]);
        assert_eq!(recompute(&rs), WorkerDecision::Clear);
        assert!(cell.load().is_empty());
    }

    // ---- S3.a — cell modifier flag bits ----

    /// `modifiers_to_flags` packs each host `Modifiers` field into
    /// the corresponding `cell_flags` bit. Standalone helper test
    /// — no worker driving needed.
    #[test]
    fn modifiers_to_flags_packs_each_bit() {
        use crate::ui::theme::Modifiers;
        use lattice_cells::cell_flags;

        let none = Modifiers::default();
        assert_eq!(modifiers_to_flags(&none), 0);

        let bold = Modifiers {
            bold: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers_to_flags(&bold), cell_flags::BOLD);

        let italic = Modifiers {
            italic: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers_to_flags(&italic), cell_flags::ITALIC);

        let under = Modifiers {
            underline: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers_to_flags(&under), cell_flags::UNDERLINE);

        let dim = Modifiers {
            dim: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers_to_flags(&dim), cell_flags::DIM);

        let rev = Modifiers {
            reverse: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers_to_flags(&rev), cell_flags::REVERSE);

        let all = Modifiers {
            bold: true,
            italic: true,
            underline: true,
            dim: true,
            reverse: true,
        };
        let expected = cell_flags::BOLD
            | cell_flags::ITALIC
            | cell_flags::UNDERLINE
            | cell_flags::DIM
            | cell_flags::REVERSE;
        assert_eq!(modifiers_to_flags(&all), expected);
    }

    /// Catppuccin's default theme styles `Keyword` as bold and
    /// `LineComment` as italic. After running the cell-builder
    /// with a Rust syntax handle attached, the cells for the `fn`
    /// keyword carry the BOLD bit and the cells for a `// comment`
    /// line carry ITALIC. Plain source bytes (the identifier
    /// `main`, the paren punctuation) do NOT carry either bit.
    #[test]
    fn keyword_cells_carry_bold_comment_cells_carry_italic() {
        let theme = crate::ui::theme::Theme::default();
        let text = "fn main() {}\n// comment\n";
        let handle = rust_handle(text, 1);
        let snap = snap_of_versioned(text, 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        let rs =
            rs_with_snapshot_themed(Some(snap), v(1), matrix_cell.clone(), Some(handle), theme);
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        let rows: Vec<&CellRow> = m.slice(0, 10).iter().collect();
        assert!(rows.len() >= 2);

        // Line 0: cells 0 and 1 are `f` and `n` (the `fn` keyword).
        // Catppuccin's keyword style is `.bold()`.
        let line0 = rows[0];
        assert_eq!(line0.cells[0].codepoint, b'f' as u32);
        assert!(
            line0.cells[0].is_bold(),
            "`f` of keyword `fn` must carry BOLD flag"
        );
        assert!(
            line0.cells[1].is_bold(),
            "`n` of keyword `fn` must carry BOLD flag"
        );
        // Keyword style has no italic / underline / dim / reverse.
        assert!(!line0.cells[0].is_italic());
        assert!(!line0.cells[0].is_underline());
        assert!(!line0.cells[0].is_dim());
        assert!(!line0.cells[0].is_reverse());

        // Line 1: `// comment` — comment style is `.italic()`.
        let line1 = rows[1];
        assert!(
            line1.cells.iter().all(|c| c.is_italic()),
            "every cell on a comment row must carry ITALIC flag"
        );
        assert!(
            line1.cells.iter().all(|c| !c.is_bold()),
            "comment cells must not carry BOLD (Catppuccin comment is italic-only)"
        );
    }

    /// Inlay cells carry only the `INLAY` flag — never any of the
    /// syntax-style modifier bits. Inlay fg + flags come from the
    /// dedicated inlay path, not from `theme.syntax_style(...)`.
    #[test]
    fn inlay_cells_do_not_inherit_syntax_modifiers() {
        let theme = crate::ui::theme::Theme::default();
        let text = "fn x";
        let handle = rust_handle(text, 1);
        let snap = snap_of_versioned(text, 1);
        let matrix_cell: Arc<ArcSwap<CellMatrix>> = Arc::default();
        // Splice an inlay after `fn` (byte 2) — right in keyword
        // territory. The cell-builder must NOT pick up BOLD on
        // the inlay's spliced cells.
        let hints = vec![inlay(0, 2, ":")];
        let rs = rs_with_snapshot_full(
            Some(snap),
            v(1),
            matrix_cell.clone(),
            Some(handle),
            theme,
            hints,
        );
        assert_eq!(recompute(&rs), WorkerDecision::Recomputed);

        let m = matrix_cell.load();
        let row = m.slice(0, 1).iter().next().cloned().unwrap();
        // combined row: `f n : space x` (`f`, `n`, inlay `:`, ` `, `x`).
        // Cell indices: 0=f keyword, 1=n keyword, 2=`:` INLAY,
        // 3=` ` source, 4=`x` source.
        assert_eq!(row.cells[0].codepoint, b'f' as u32);
        assert!(row.cells[0].is_bold(), "keyword `f` should be BOLD");
        assert_eq!(row.cells[2].codepoint, b':' as u32);
        assert!(row.cells[2].is_inlay(), "inlay cell must carry INLAY");
        assert!(
            !row.cells[2].is_bold(),
            "inlay cell must NOT inherit BOLD from surrounding keyword style"
        );
        assert!(!row.cells[2].is_italic());
        assert!(!row.cells[2].is_underline());
    }
}
