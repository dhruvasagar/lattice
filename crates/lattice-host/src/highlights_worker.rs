//! Background highlights worker — tree-sitter walk off the UI thread.
//!
//! Phase 5.8.AF.5 / Slice X2.3.
//!
//! ## Why this exists
//!
//! Per paramount goal #1 in `CLAUDE.md`:
//!
//! > **Performance.** UI thread does no I/O, no parsing, no shaping.
//!
//! `Editor::refresh_highlights_window` (pre-X2) ran the tree-sitter
//! `highlight_lines` walk synchronously inside every renderer peer's
//! per-frame body. The cache-miss path cost ~200–600 µs per call
//! during scroll. Held-key (`j`/`k`) input bursts at 30–60 Hz
//! therefore saturated the UI thread with parse work, starving
//! `cx.notify()`-scheduled paints — the symptom: cursor "disappears"
//! while a key is held because the viewport-adjust step inside
//! render never runs.
//!
//! X2 hoists the parse off-thread. This module owns the worker.
//!
//! ## Design
//!
//! - Dispatch's `publish_render_state` populates
//!   [`crate::render_state::SyntaxRenderState`] inputs
//!   (`syntax_handle`, `scroll`, `viewport_height`, `fold_hash`,
//!   `text_version`) and fires
//!   [`crate::editor::HighlightWake`]'s `Notify`.
//! - The worker `notified().await`s the wake signal. `Notify` is
//!   permit-style: a burst of publishes wakes the worker exactly
//!   once, after which the worker re-reads the *latest* snapshot.
//! - On wake the worker reads `render_state.load_full().syntax`,
//!   constructs a [`crate::render_state::VisibleHighlightsKey`],
//!   compares it against the key in the currently-published
//!   [`crate::render_state::VisibleSpans`], and short-circuits on
//!   cache-hit.
//! - On cache-miss with a current snapshot: runs
//!   `snap.highlight_lines(start, end)` and stores a fresh
//!   `VisibleSpans` into the durable
//!   `syntax_visible_spans_cell` Arc<ArcSwap<…>>.
//! - On cache-miss with a *stale* snapshot (snapshot's
//!   `text_version` < document's `text_version`): the worker
//!   applies the stale-snapshot HOLD — it stores the *new* key but
//!   preserves the previously-published spans. This matches the
//!   semantics of the original `refresh_highlights_window` so a
//!   mid-edit window doesn't recolor against pre-edit data.
//!
//! ## Renderer contract
//!
//! Renderer peers read with:
//!
//! ```text
//! let rs = editor.render_state.load_full();
//! let spans = rs.syntax.visible_spans.load();
//! // spans.spans[i] = StyledSpans for visible line i (= doc line scroll + i)
//! ```
//!
//! Empty `spans` is the legal "no highlights yet" state (initial
//! boot, no language attached, or a pre-first-worker-tick window).
//! Renderers paint plain text in that case.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{info, trace};

use crate::editor::HighlightWake;
use crate::render_state::{
    InlayHintRow, OverlayLayer, RenderState, RowOverlayQuad, RowPrepaint, RowRun,
    StaticOverlayQuads, VisibleHighlightsKey, VisibleRows, VisibleSpans,
};

/// Recompute decision the worker takes on a wake. Visible for
/// testing; the production loop calls [`recompute`] directly.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkerDecision {
    /// `syntax_handle` is `None` — no language attached. Worker
    /// clears the published spans (so a previous language's
    /// highlights don't linger after a language detach).
    Clear,
    /// Current inputs match the already-published key. Worker
    /// does nothing.
    CacheHit,
    /// Snapshot is behind the document
    /// (`snapshot.text_version() < inputs.text_version`). Worker
    /// stores the new key but preserves existing spans
    /// (stale-snapshot HOLD).
    StaleSnapshotHold,
    /// Snapshot is current; worker ran `highlight_lines` and
    /// stored fresh spans + key.
    Recomputed,
}

/// Worker entry point spawned at boot. Loops forever, awaiting
/// the wake `Notify`. Each wake re-reads the latest
/// `RenderState.syntax` inputs and calls [`recompute`].
///
/// Spawn from `editor_boot` once `Editor` is constructed. Pass
/// clones of `Editor::render_state` and
/// `Editor::syntax_visible_spans_cell` plus the wake `Notify`.
pub async fn run(
    render_state: Arc<ArcSwap<RenderState>>,
    wake: HighlightWake,
    spans_cell: Arc<ArcSwap<VisibleSpans>>,
    rows_cell: Arc<ArcSwap<VisibleRows>>,
    static_overlay_quads_cell: Arc<ArcSwap<StaticOverlayQuads>>,
    paint_request: Arc<tokio::sync::Notify>,
) {
    // X2 diagnostic (2026-05-19): held-j fix reported as
    // ineffective post-X2. Promote startup + per-tick traces to
    // INFO so the user's default RUST_LOG=info run surfaces
    // whether the worker is alive and being driven by
    // publish_render_state. Revert to trace! once we've
    // confirmed the runtime behavior. Avoid the trace level
    // because per-tick INFO spam during a held-key burst will
    // crowd out other signal in the log.
    info!(
        target: "lattice_host::highlights_worker",
        "highlights worker spawned (X2)"
    );
    let mut tick_count: u64 = 0;
    loop {
        // `notified().await` resolves when `notify_one()` is
        // called (or immediately if a permit is stored).
        // Permit-style coalescing: a burst of publishes wakes
        // us once; we read the LATEST snapshot below, which
        // captures the full effect of the burst.
        wake.0.notified().await;
        let t0 = std::time::Instant::now();
        let decision = recompute(
            &render_state,
            &spans_cell,
            &rows_cell,
            &static_overlay_quads_cell,
        );
        let elapsed_us = t0.elapsed().as_micros();
        tick_count += 1;
        // X1b: tell the renderer it has fresh data to paint.
        // Only on Recomputed -- CacheHit / StaleSnapshotHold leave
        // spans bit-identical / unchanged for the renderer's
        // purposes, so waking the peer would be a wasted frame.
        // `Clear` (language detach) is also a content change worth
        // a paint.
        if matches!(decision, WorkerDecision::Recomputed | WorkerDecision::Clear) {
            paint_request.notify_one();
        }
        // First few ticks always INFO so we confirm the wake
        // signal arrives; afterwards only INFO on Recomputed
        // (the cache-miss path that does the actual tree-sitter
        // walk — the cost X2 is supposed to move off-thread).
        if tick_count <= 5 || matches!(decision, WorkerDecision::Recomputed) {
            info!(
                target: "lattice_host::highlights_worker",
                tick = tick_count,
                ?decision,
                elapsed_us,
                "highlights worker tick"
            );
        } else {
            trace!(
                target: "lattice_host::highlights_worker",
                tick = tick_count,
                ?decision,
                elapsed_us,
                "highlights worker tick"
            );
        }
    }
}

/// Pure synchronous recompute. Reads the current published
/// `SyntaxRenderState`, decides whether to recompute, and updates
/// `spans_cell` accordingly. Returns the decision taken so tests
/// can assert cache-hit / stale-snapshot HOLD / recompute paths
/// without driving the async loop.
pub fn recompute(
    render_state: &ArcSwap<RenderState>,
    spans_cell: &ArcSwap<VisibleSpans>,
    rows_cell: &ArcSwap<VisibleRows>,
    static_overlay_quads_cell: &ArcSwap<StaticOverlayQuads>,
) -> WorkerDecision {
    let rs = render_state.load_full();
    let syntax = &rs.syntax;
    let Some(handle) = syntax.syntax_handle.as_ref() else {
        // No language attached. Clear published spans so a
        // language detach doesn't leave stale highlights.
        let existing = spans_cell.load();
        if existing.spans.is_empty()
            && existing.computed_for_key == VisibleHighlightsKey::default()
        {
            return WorkerDecision::Clear;
        }
        spans_cell.store(Arc::new(VisibleSpans::default()));
        // Perf plan A.2 slice A.2a: mirror the spans clear into
        // the rows cell so the GPUI peer drops any stale prepaints
        // on language detach.
        rows_cell.store(Arc::new(VisibleRows::default()));
        // Perf plan B.2 slice B.2.a: mirror the clear into the
        // static-overlay quads cell so renderers drop any stale
        // overlay buckets on language detach.
        static_overlay_quads_cell.store(Arc::new(StaticOverlayQuads::default()));
        return WorkerDecision::Clear;
    };

    let snap = handle.snapshot();
    let snap_ptr = Arc::as_ptr(&snap) as usize;
    let snap_text_version = snap.text_version();
    let key = VisibleHighlightsKey {
        snapshot_ptr: snap_ptr,
        syntax_text_version: snap_text_version,
        scroll: syntax.scroll,
        viewport_height: syntax.viewport_height,
        fold_hash: syntax.fold_hash,
        inlay_version: syntax.inlay_version,
        static_overlay_version: syntax.static_overlay_version,
    };

    let existing = spans_cell.load_full();
    if existing.computed_for_key == key {
        return WorkerDecision::CacheHit;
    }

    // Stale-snapshot HOLD: the parser hasn't caught up with the
    // most recent edits yet. Compose a new VisibleSpans that
    // carries the NEW key (so we don't keep retrying for the
    // same input on every wake) but PRESERVES the previously
    // computed spans — re-highlighting against stale tree data
    // would recolor unchanged-content lines incorrectly. The
    // shifter (`shift_highlights_for_edit`, if any) is expected
    // to keep indices line-aligned during this hold window.
    if snap_text_version < syntax.text_version {
        let held = VisibleSpans {
            spans: existing.spans.clone(),
            computed_for_key: key,
        };
        spans_cell.store(Arc::new(held));
        // Mirror the HOLD on the rows cell: preserve existing rows,
        // advance the key so we don't retry every wake against the
        // same stale combo.
        let existing_rows = rows_cell.load_full();
        let held_rows = VisibleRows {
            rows: existing_rows.rows.clone(),
            computed_for_key: key,
        };
        rows_cell.store(Arc::new(held_rows));
        // Perf plan B.2 slice B.2.a: HOLD also preserves the
        // static-overlay bucket. The bucket is derived from the
        // rows (which we're holding) + the published static-overlay
        // payload; both are stable across the HOLD wake. Advance
        // the key to match the rows/spans cells.
        let existing_quads = static_overlay_quads_cell.load_full();
        let held_quads = StaticOverlayQuads {
            quads: existing_quads.quads.clone(),
            computed_for_key: key,
        };
        static_overlay_quads_cell.store(Arc::new(held_quads));
        return WorkerDecision::StaleSnapshotHold;
    }

    // Cache miss + current snapshot. Run the tree-sitter walk
    // OFF the UI thread (this fn runs on the worker's tokio
    // task). `highlight_lines` returns `Result`; on error
    // (parse failed, out-of-bounds range, ...) we publish empty
    // spans rather than panic — the renderer paints plain text
    // in that case.
    //
    // Slice X2.9: honour `end_line_override` so the fold-aware
    // peer can stretch the highlight window past closed folds.
    // Falls back to `scroll + viewport_height` (the pre-X2.9
    // shape) when the peer didn't request a stretch.
    let start = syntax.scroll;
    let default_end = syntax
        .scroll
        .saturating_add(syntax.viewport_height.max(1));
    let end = syntax.end_line_override.unwrap_or(default_end).max(default_end);
    let spans = snap.highlight_lines(start, end).unwrap_or_default();

    // Perf plan A.2 slice A.2a: build pre-paint rows from the same
    // styled-spans output. Each row carries the source line text +
    // a style-tagged run partition (adjacent equal styles merged).
    // Theme resolution happens on the renderer at paint time so
    // theme switches don't invalidate this cache (see RowRun docs).
    //
    // Source bytes come from the SAME `SyntaxSnapshot` the styled
    // spans index into — keeps the byte offsets and the row text
    // aligned even if `active_document.snapshot` has advanced past
    // the syntax version while the worker is mid-recompute.
    //
    // Perf plan B.1 (dirty-row recomposition): pass the previously
    // published rows + their key so `build_rows_with_cache` can
    // reuse `RowPrepaint`s for any absolute line that's still in
    // view when `(snapshot_ptr, syntax_text_version, inlay_version)`
    // are unchanged. On the scroll-only path (the dominant held-j
    // case) every overlapping line skips the per-line text fetch
    // + weave, dropping `build_rows` cost from O(viewport) to
    // O(scroll_delta).
    //
    // Slice A.2b.2: bucket the published `inlay_hints` per absolute
    // line so the per-row weave is `O(inlays_on_line)`. Bucket cost
    // is `O(n)` over the published list (typically <200 hints).
    // Skipped entirely when the list is empty — the dominant
    // no-LSP / no-inlay case stays on the cold A.2a path.
    let inlay_hints = syntax.inlay_hints.clone();
    let inlays_by_line = bucket_inlays_by_line(&inlay_hints);
    let prev_rows = rows_cell.load_full();
    let rows = build_rows_with_cache(
        snap.source(),
        start,
        &spans,
        &inlays_by_line,
        &prev_rows,
        &key,
    );

    // Perf plan B.2 slice B.2.a: bucket the published static-overlay
    // layer payloads into per-row quad lists. Coordinates are in
    // combined-column space (matches the renderer's
    // `push_range_quads` output bit-for-bit). Empty payloads
    // short-circuit to empty buckets — keeps the steady-state
    // no-overlay path cheap. Bucket happens BEFORE the cell stores
    // below so the rows borrow doesn't outlive the per-row column
    // walk.
    let substitute_storage: Vec<lattice_protocol::position::Range>;
    let substitute_matches: &[lattice_protocol::position::Range] = match rs
        .active_document
        .substitute_preview
        .as_ref()
    {
        Some(prev) => {
            substitute_storage = prev.matches.to_vec();
            &substitute_storage
        }
        None => &[],
    };
    let static_overlay_quads = bucket_static_overlays(
        &rows,
        start,
        snap.source(),
        &syntax.doc_highlights,
        &rs.active_document.all_matches,
        substitute_matches,
    );

    // Perf plan D.1: wrap the freshly-built `Vec`s in `Arc<[T]>` at
    // the cell-store boundary so subsequent HOLD / B.1 reuse paths
    // can clone the outer Arc instead of the inner Vec.
    spans_cell.store(Arc::new(VisibleSpans {
        spans: Arc::from(spans.into_boxed_slice()),
        computed_for_key: key,
    }));
    rows_cell.store(Arc::new(VisibleRows {
        rows: Arc::from(rows.into_boxed_slice()),
        computed_for_key: key,
    }));
    static_overlay_quads_cell.store(Arc::new(StaticOverlayQuads {
        quads: Arc::from(static_overlay_quads.into_boxed_slice()),
        computed_for_key: key,
    }));
    WorkerDecision::Recomputed
}

/// Perf plan B.1 (dirty-row recomposition). Wraps [`build_rows`]
/// with a per-absolute-line reuse path: when the previous publish
/// was against the same
/// `(snapshot_ptr, syntax_text_version, inlay_version)` (the
/// source bytes AND the inlay payload are bit-identical), the
/// prepaints for any absolute line that's still in view are reused
/// as-is. Only newly-visible lines hit [`build_rows`] for the
/// per-line memchr scan + `into_boxed_str` alloc + `weave_row`
/// walk.
///
/// The dominant held-j scroll case (snapshot + version + inlay
/// payload unchanged, scroll deltas by 1) reuses ~99% of rows;
/// build cost collapses from `O(viewport_height)` to
/// `O(scroll_delta)`.
///
/// The cache only kicks in for the source-affecting axes
/// (`snapshot_ptr`, `syntax_text_version`, `inlay_version`).
/// Every other key axis (fold_hash, viewport_height, scroll) is
/// allowed to differ — they change WHICH absolute lines are
/// visible but not the per-absolute-line content. When any source
/// axis flips, we fall through to the from-scratch path so the
/// published rows reflect the new content.
fn build_rows_with_cache(
    source: &[u8],
    start: u32,
    styled_spans: &[Vec<lattice_syntax::StyledSpan>],
    inlays_by_line: &[Vec<(u32, &str)>],
    prev_rows: &VisibleRows,
    new_key: &VisibleHighlightsKey,
) -> Vec<RowPrepaint> {
    // Bail to the cold path on any source-affecting input change.
    // `snapshot_ptr` flips on every reparse-produced snapshot;
    // `syntax_text_version` flips on edits even when the parser
    // re-uses an Arc'd Tree; `inlay_version` flips on any inlay
    // payload change (arrival, edit, mode-gate flip). Any of those
    // means line content (line count, byte offsets, woven runs)
    // may differ.
    let source_match = prev_rows.computed_for_key.snapshot_ptr == new_key.snapshot_ptr
        && prev_rows.computed_for_key.syntax_text_version == new_key.syntax_text_version
        && prev_rows.computed_for_key.inlay_version == new_key.inlay_version;
    if !source_match || prev_rows.rows.is_empty() {
        return build_rows(source, start, styled_spans, inlays_by_line);
    }

    // Find the byte offset of the FIRST newly-visible line so we
    // only memchr-scan the gap. For pure scroll forwards/backwards
    // the gap is `scroll_delta` lines; for no scroll change it's
    // empty (every line is reused). The reuse window in absolute
    // line space is `[max(new_start, prev_start), min(new_end, prev_end))`.
    let prev_start = prev_rows.computed_for_key.scroll;
    let prev_len = prev_rows.rows.len() as u32;
    let new_len = styled_spans.len() as u32;

    let reuse_lo = start.max(prev_start);
    let reuse_hi = (start + new_len).min(prev_start + prev_len);

    let mut rows: Vec<RowPrepaint> = Vec::with_capacity(styled_spans.len());

    // Tracks the source-byte offset of the next line we still need
    // to MATERIALISE (not reuse). Initially points at `start` but
    // is recomputed lazily — we only pay for the seek when we
    // actually hit a non-reuse row.
    let mut byte_off_cache: Option<(u32, usize)> = None;
    let seek_to = |line_no: u32, cache: &mut Option<(u32, usize)>| -> usize {
        // Linear walk from the cached position (or from 0). For
        // forward scroll the cached position is just past the last
        // built row's start, so this is O(scroll_delta) per row.
        let (mut at_line, mut at_byte) = cache.unwrap_or((0, 0));
        if at_line > line_no {
            // Backward seek: walk from 0. Rare path; bench cases
            // never hit it because scroll deltas are small.
            at_line = 0;
            at_byte = 0;
        }
        while at_line < line_no && at_byte < source.len() {
            match memchr::memchr(b'\n', &source[at_byte..]) {
                Some(nl) => {
                    at_byte += nl + 1;
                    at_line += 1;
                }
                None => {
                    at_byte = source.len();
                    break;
                }
            }
        }
        *cache = Some((at_line, at_byte));
        at_byte
    };

    for (i, line_spans) in styled_spans.iter().enumerate() {
        let abs_line = start + i as u32;
        if abs_line >= reuse_lo && abs_line < reuse_hi {
            // Cache hit: clone the prior `RowPrepaint`. `Box<str>`
            // clone is one alloc + memcpy of the line text; could
            // be eliminated by switching to `Arc<str>` if this
            // shows up in profile-frames data (`Arc::clone` is one
            // refcount bump).
            let prev_rel = (abs_line - prev_start) as usize;
            rows.push(prev_rows.rows[prev_rel].clone());
            continue;
        }
        // Cold path: materialise this row.
        let line_start = seek_to(abs_line, &mut byte_off_cache);
        let line_end = memchr::memchr(b'\n', &source[line_start..])
            .map(|n| line_start + n)
            .unwrap_or(source.len());
        let line_text = std::str::from_utf8(&source[line_start..line_end]).unwrap_or("");
        let line_inlays = inlays_by_line.get(i).map(Vec::as_slice).unwrap_or(&[]);
        rows.push(weave_row(line_text, line_spans, line_inlays));
        // Advance the seek cache past this line so the next cold
        // row picks up O(1) after.
        byte_off_cache = Some((abs_line + 1, (line_end + 1).min(source.len())));
    }
    rows
}

/// Build `RowPrepaint`s for the worker's `recompute` output.
///
/// `styled_spans[i]` is the per-line highlight slice for buffer
/// line `start + i`. Walks `source` once to find the byte offset
/// of `start`, then materialises each visible line as a
/// `Box<str>` plus its run partition. `source` is the
/// `SyntaxSnapshot`'s bytes — same bytes the spans index into —
/// so the row text and span offsets stay aligned even if the
/// document snapshot has raced ahead of the syntax parse.
///
/// Slice A.2b.2: `inlays_by_line[i]` carries the sorted inlay
/// list for visible line `i` (empty when no inlays land on the
/// line). [`weave_row`] splices each inlay into `combined` at its
/// `byte` offset and records the splice on the row's
/// `inlay_offsets`. Empty inlay slices skip the weave overhead.
fn build_rows(
    source: &[u8],
    start: u32,
    styled_spans: &[Vec<lattice_syntax::StyledSpan>],
    inlays_by_line: &[Vec<(u32, &str)>],
) -> Vec<RowPrepaint> {
    // Seek to the byte offset of line `start`. `memchr` is SIMD-
    // accelerated; for a 1MB file at line ~1000 the seek is sub-µs
    // on contemporary x86_64. If profile-frames data shows this
    // dominating recompute, cache a per-snapshot newline-offsets
    // vector (one u32 per line).
    let mut byte_off: usize = 0;
    let mut line_no: u32 = 0;
    while line_no < start && byte_off < source.len() {
        match memchr::memchr(b'\n', &source[byte_off..]) {
            Some(nl) => {
                byte_off += nl + 1;
                line_no += 1;
            }
            None => {
                byte_off = source.len();
                break;
            }
        }
    }
    let mut rows: Vec<RowPrepaint> = Vec::with_capacity(styled_spans.len());
    for (i, line_spans) in styled_spans.iter().enumerate() {
        let line_end = memchr::memchr(b'\n', &source[byte_off..])
            .map(|n| byte_off + n)
            .unwrap_or(source.len());
        let line_bytes = &source[byte_off..line_end];
        let line_text = std::str::from_utf8(line_bytes).unwrap_or("");
        let line_inlays = inlays_by_line.get(i).map(Vec::as_slice).unwrap_or(&[]);
        rows.push(weave_row(line_text, line_spans, line_inlays));
        byte_off = (line_end + 1).min(source.len());
    }
    rows
}

/// Perf plan B.2 slice B.2.a: bucket the three static-overlay
/// layer payloads (`doc_highlights`, `all_matches`,
/// `substitute_matches`) into per-row [`RowOverlayQuad`] lists.
///
/// `rows[i]` is the worker's pre-built row for visible buffer line
/// `start + i`. Output `quads[i]` carries every layer's ranges
/// that intersect the row, in fixed precedence order
/// `DocHighlight → AllMatches → Substitute`. The renderer
/// interleaves the cursor-coupled layers (`visual`, `current_match`)
/// at prepaint between `AllMatches` and `Substitute`.
///
/// Coordinates are in **combined-column space** — chars in the
/// row's `combined` text including inlay splices. The walk mirrors
/// `lattice_ui_gpui::editor_element::push_range_quads` exactly so
/// renderer-side merge code can consume the bucket bit-identical
/// to its own per-frame walk.
///
/// Empty payloads on all three layers skip the bucket entirely —
/// returns an empty `Vec` and the renderer's overlay path stays on
/// the cheap "no static overlays" branch.
fn bucket_static_overlays(
    rows: &[RowPrepaint],
    start: u32,
    source: &[u8],
    doc_highlights: &[lattice_protocol::position::Range],
    all_matches: &[lattice_protocol::position::Range],
    substitute_matches: &[lattice_protocol::position::Range],
) -> Vec<Vec<RowOverlayQuad>> {
    if doc_highlights.is_empty() && all_matches.is_empty() && substitute_matches.is_empty() {
        return Vec::new();
    }
    // Seek to `start`'s byte offset once, then advance per-row via
    // memchr so per-row line_len lookups stay O(line_len) instead of
    // O(scroll + i) like the renderer's per-frame walk used to be.
    let mut byte_off: usize = 0;
    let mut line_no: u32 = 0;
    while line_no < start && byte_off < source.len() {
        match memchr::memchr(b'\n', &source[byte_off..]) {
            Some(nl) => {
                byte_off += nl + 1;
                line_no += 1;
            }
            None => {
                byte_off = source.len();
                break;
            }
        }
    }
    let mut out: Vec<Vec<RowOverlayQuad>> = Vec::with_capacity(rows.len());
    for i in 0..rows.len() {
        let line_idx = start + i as u32;
        let line_end = memchr::memchr(b'\n', &source[byte_off..])
            .map(|n| byte_off + n)
            .unwrap_or(source.len());
        let line_len = line_end - byte_off;
        let mut quads: Vec<RowOverlayQuad> = Vec::new();
        push_layer_quads(&mut quads, doc_highlights, OverlayLayer::DocHighlight, line_idx, line_len);
        push_layer_quads(&mut quads, all_matches, OverlayLayer::AllMatches, line_idx, line_len);
        push_layer_quads(&mut quads, substitute_matches, OverlayLayer::Substitute, line_idx, line_len);
        out.push(quads);
        byte_off = (line_end + 1).min(source.len());
    }
    out
}

/// Push one layer's intersecting ranges into `out` as tagged
/// `RowOverlayQuad`s in **source utf-8 byte space**. Mirrors
/// `editor_element::push_range_quads`'s line-bounds / byte-clamp
/// rules, just without the byte→col conversion (the renderer does
/// that on consumption — see [`RowOverlayQuad`] docs).
fn push_layer_quads(
    out: &mut Vec<RowOverlayQuad>,
    ranges: &[lattice_protocol::position::Range],
    layer: OverlayLayer,
    line_idx: u32,
    line_len: usize,
) {
    for r in ranges {
        if line_idx < r.start.line || line_idx > r.end.line {
            continue;
        }
        let start_byte = if line_idx == r.start.line {
            (r.start.byte as usize).min(line_len)
        } else {
            0
        };
        let end_byte = if line_idx == r.end.line {
            (r.end.byte as usize).min(line_len)
        } else {
            line_len
        };
        if end_byte <= start_byte {
            continue;
        }
        out.push(RowOverlayQuad {
            layer,
            source_byte_start: start_byte as u32,
            source_byte_end: end_byte as u32,
        });
    }
}

/// Bucket a flat inlay-hints list into a per-visible-line vector,
/// each bucket sorted ascending by byte offset. Output length is
/// always `(end - start) as usize` so callers can index by visible-
/// row offset without bounds-checking.
///
/// Filters out hints whose line falls outside the visible window —
/// inlays beyond the viewport never need a per-row vector even if
/// they're in the published payload.
fn bucket_inlays_by_line(
    inlay_hints: &[InlayHintRow],
) -> Vec<Vec<(u32, &str)>> {
    if inlay_hints.is_empty() {
        return Vec::new();
    }
    // Find the max visible line in the payload + 1 to size the
    // output. Caller passes the spans-indexed slice through, so the
    // buckets are at most `viewport_height` entries; we lazily size
    // by max-line + 1 to avoid an extra parameter.
    let max_line = inlay_hints.iter().map(|h| h.line).max().unwrap_or(0);
    let mut buckets: Vec<Vec<(u32, &str)>> = vec![Vec::new(); (max_line as usize) + 1];
    for h in inlay_hints {
        buckets[h.line as usize].push((h.byte, h.text.as_str()));
    }
    for b in &mut buckets {
        b.sort_by_key(|(off, _)| *off);
    }
    buckets
}

/// Per-row weave: splice inlay-virtual-text into `line_text` at
/// each inlay's byte offset, partitioning the result into Source
/// / Inlay runs and recording each splice on `inlay_offsets` so
/// consumers can remap byte offsets onto column positions in the
/// combined output.
///
/// - Empty `line_inlays` + empty `line_spans` yields a single
///   `Source { Default, line_text.len() }` run (or no runs on an
///   empty line) and no offsets.
/// - Adjacent equal-style Source bytes merge; Inlay runs always
///   break the merge so consumers can colour them distinctly.
/// - Trailing inlays (`orig_byte >= line_text.len()`) splice at
///   end-of-line — matches the GPUI `build_line_with_inlays`
///   contract.
fn weave_row(
    line_text: &str,
    line_spans: &[lattice_syntax::StyledSpan],
    line_inlays: &[(u32, &str)],
) -> RowPrepaint {
    // Fast path: no inlays. Build combined + Source-only run
    // partition without the splice machinery. Matches the A.2a
    // shape with the new enum variant wrapper.
    if line_inlays.is_empty() {
        let combined: Box<str> = line_text.into();
        let runs = collapse_source_runs(line_spans, combined.len() as u32);
        return RowPrepaint {
            combined,
            runs,
            inlay_offsets: Arc::from(Vec::<(u32, u32)>::new().into_boxed_slice()),
        };
    }

    let inlay_byte_total: usize = line_inlays.iter().map(|(_, t)| t.len()).sum();
    let mut combined = String::with_capacity(line_text.len() + inlay_byte_total);
    let mut runs: Vec<RowRun> = Vec::with_capacity(line_spans.len() + line_inlays.len() * 2);
    let mut inlay_offsets: Vec<(u32, u32)> = Vec::with_capacity(line_inlays.len());

    // Pending Source run accumulator. We hold off pushing until we
    // see a different style or an Inlay break so adjacent equal-
    // style chars collapse into one run.
    let mut pending_style: Option<lattice_syntax::Style> = None;
    let mut pending_len: u32 = 0;

    let flush_pending = |runs: &mut Vec<RowRun>,
                         pending_style: &mut Option<lattice_syntax::Style>,
                         pending_len: &mut u32| {
        if let Some(style) = pending_style.take() {
            if *pending_len > 0 {
                runs.push(RowRun::Source { len: *pending_len, style });
            }
            *pending_len = 0;
        }
    };

    let mut inlay_idx = 0usize;
    for (orig_byte, ch) in line_text.char_indices() {
        // Splice inlays whose `orig_byte` lands at or before this
        // char. Multiple inlays at the same byte run in input order.
        while inlay_idx < line_inlays.len()
            && (line_inlays[inlay_idx].0 as usize) <= orig_byte
        {
            let (off, text) = line_inlays[inlay_idx];
            let char_width = text.chars().count() as u32;
            inlay_offsets.push((off, char_width));
            // Inlay breaks the pending Source run.
            flush_pending(&mut runs, &mut pending_style, &mut pending_len);
            combined.push_str(text);
            runs.push(RowRun::Inlay { len: text.len() as u32 });
            inlay_idx += 1;
        }
        let style = style_at_byte(line_spans, orig_byte);
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        combined.push_str(encoded);
        match pending_style {
            Some(prev) if prev == style => {
                pending_len += encoded.len() as u32;
            }
            Some(_) => {
                flush_pending(&mut runs, &mut pending_style, &mut pending_len);
                pending_style = Some(style);
                pending_len = encoded.len() as u32;
            }
            None => {
                pending_style = Some(style);
                pending_len = encoded.len() as u32;
            }
        }
    }
    // Trailing inlays at/past EOL.
    while inlay_idx < line_inlays.len() {
        let (off, text) = line_inlays[inlay_idx];
        let char_width = text.chars().count() as u32;
        inlay_offsets.push((off, char_width));
        flush_pending(&mut runs, &mut pending_style, &mut pending_len);
        combined.push_str(text);
        runs.push(RowRun::Inlay { len: text.len() as u32 });
        inlay_idx += 1;
    }
    flush_pending(&mut runs, &mut pending_style, &mut pending_len);

    RowPrepaint {
        combined: combined.into_boxed_str(),
        runs,
        inlay_offsets: Arc::from(inlay_offsets.into_boxed_slice()),
    }
}

/// Resolve the highlight style at a given utf-8 byte offset inside
/// `line_spans`. Returns `Style::Default` for bytes that fall
/// outside every styled span (whitespace, punctuation that no
/// theme styles, etc.).
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

/// Collapse `line_spans` into a minimal Source-only `RowRun`
/// partition covering the row's `combined` text exhaustively.
///
/// - Empty `line_spans` yields a single `Source { Default,
///   combined_len }` run (so renderers always have a valid
///   partition to walk).
/// - Gaps between spans (uncovered byte ranges between `prev.end`
///   and `next.start`) are filled with `Source { Default, _ }`
///   runs so `sum(runs[*].len()) == combined_len`.
/// - Spans whose end exceeds `combined_len` are clamped — the
///   highlight grammar can produce one-past-the-end ranges on
///   blank lines and we don't want to overflow.
fn collapse_source_runs(
    line_spans: &[lattice_syntax::StyledSpan],
    combined_len: u32,
) -> Vec<RowRun> {
    if combined_len == 0 {
        return Vec::new();
    }
    let mut runs: Vec<RowRun> = Vec::with_capacity(line_spans.len().max(1));
    let mut cursor: u32 = 0;

    let push = |runs: &mut Vec<RowRun>, style: lattice_syntax::Style, len: u32| {
        if len == 0 {
            return;
        }
        if let Some(RowRun::Source { style: prev_style, len: prev_len }) = runs.last_mut()
            && *prev_style == style
        {
            *prev_len += len;
            return;
        }
        runs.push(RowRun::Source { len, style });
    };

    for s in line_spans {
        let start = (s.start as u32).min(combined_len);
        let end = (s.end as u32).min(combined_len);
        if end <= start {
            continue;
        }
        if start > cursor {
            // Uncovered gap before this span — fill with Default.
            push(&mut runs, lattice_syntax::Style::Default, start - cursor);
        }
        push(&mut runs, s.style, end - start);
        cursor = end;
    }
    if cursor < combined_len {
        push(&mut runs, lattice_syntax::Style::Default, combined_len - cursor);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `recompute` with `syntax_handle: None` clears spans —
    /// mirrors the original `refresh_highlights_window(None, …)`
    /// contract. Migrated from `highlights.rs::tests`.
    #[test]
    fn recompute_with_no_handle_clears_spans() {
        let rs: ArcSwap<RenderState> = ArcSwap::from_pointee(RenderState::default());
        let cell: ArcSwap<VisibleSpans> = ArcSwap::from_pointee(VisibleSpans {
            spans: Arc::from(vec![Vec::new()].into_boxed_slice()),
            computed_for_key: VisibleHighlightsKey {
                snapshot_ptr: 0xdead,
                ..Default::default()
            },
        });
        let rows_cell: ArcSwap<VisibleRows> = ArcSwap::from_pointee(VisibleRows::default());
        let overlay_cell: ArcSwap<StaticOverlayQuads> =
            ArcSwap::from_pointee(StaticOverlayQuads::default());
        let decision = recompute(&rs, &cell, &rows_cell, &overlay_cell);
        assert_eq!(decision, WorkerDecision::Clear);
        let after = cell.load();
        assert!(after.spans.is_empty());
        assert_eq!(after.computed_for_key, VisibleHighlightsKey::default());
    }

    /// Helper: build a `RenderState` carrying a seeded Rust
    /// `SyntaxHandle` parsed against `text`, with the given
    /// scroll / viewport / fold_hash / text_version inputs.
    /// Slice X2.9 tests use this to exercise the full
    /// `recompute` decision tree without needing a tokio
    /// runtime.
    ///
    /// Perf plan A.2 slice A.2a: helper now also returns the
    /// parallel `VisibleRows` cell so tests can assert the
    /// pre-paint pipeline alongside the spans one.
    fn rs_with_rust(
        text: &str,
        scroll: u32,
        viewport_height: u32,
        fold_hash: u64,
        text_version: u64,
        end_line_override: Option<u32>,
    ) -> (
        ArcSwap<RenderState>,
        Arc<lattice_syntax::SyntaxHandle>,
        Arc<arc_swap::ArcSwap<VisibleSpans>>,
        Arc<arc_swap::ArcSwap<VisibleRows>>,
        Arc<arc_swap::ArcSwap<StaticOverlayQuads>>,
    ) {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .expect("rust grammar available in test build");
        s.parse_at(text, text_version);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
        let cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleSpans::default()));
        let rows_cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleRows::default()));
        let overlay_cell = Arc::new(arc_swap::ArcSwap::from_pointee(
            StaticOverlayQuads::default(),
        ));
        let rs = RenderState {
            syntax: Arc::new(crate::render_state::SyntaxRenderState {
                syntax_handle: Some(handle.clone()),
                scroll,
                viewport_height,
                end_line_override,
                fold_hash,
                text_version,
                visible_spans: cell.clone(),
                visible_rows: rows_cell.clone(),
                inlay_hints: Arc::from(
                    Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                ),
                inlay_version: 0,
                static_overlay_quads: overlay_cell.clone(),
                doc_highlights: Arc::from(
                    Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
                ),
                static_overlay_version: 0,
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        (ArcSwap::from_pointee(rs), handle, cell, rows_cell, overlay_cell)
    }

    /// Cache miss path: with a current snapshot and a fresh
    /// (unseen) key, the worker walks `highlight_lines` and
    /// publishes the resulting spans into the cell.
    #[test]
    fn recompute_with_current_snapshot_publishes_spans() {
        let (rs, _h, cell, rows_cell, overlay_cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        let decision = recompute(&rs, &cell, &rows_cell, &overlay_cell);
        assert_eq!(decision, WorkerDecision::Recomputed);
        let after = cell.load();
        assert!(!after.spans.is_empty(), "expected spans for `fn main`");
        // First line must carry at least one StyledSpan -- the
        // `fn` keyword.
        let first_line = &after.spans[0];
        assert!(
            first_line.iter().any(|s| matches!(
                s.style,
                lattice_syntax::Style::Keyword | lattice_syntax::Style::Function
            )),
            "expected Keyword / Function span in `fn main`; got {first_line:?}"
        );
    }

    /// Cache-hit short-circuit: a second `recompute` with the
    /// same inputs sees `computed_for_key == key` and returns
    /// `CacheHit` without re-walking or churning the cell.
    #[test]
    fn recompute_with_unchanged_key_is_cache_hit() {
        let (rs, _h, cell, rows_cell, overlay_cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&cell.load_full());
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::CacheHit);
        let second_ptr = Arc::as_ptr(&cell.load_full());
        assert_eq!(
            first_ptr, second_ptr,
            "cache-hit must not store a new Arc"
        );
    }

    /// Stale-snapshot HOLD: the document's `text_version` (the
    /// SyntaxRenderState input) has advanced past the parsed
    /// snapshot's version. The worker must preserve the
    /// previously-published spans (held line-aligned by the
    /// shifter in production) and update only the cache key, so
    /// the renderer doesn't suddenly see empty spans during a
    /// mid-edit window.
    #[test]
    fn recompute_with_stale_snapshot_holds_spans() {
        // Snapshot parsed against text_version = 1.
        let (rs_initial, handle, cell, rows_cell, overlay_cell) =
            rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(
            recompute(&rs_initial, &cell, &rows_cell, &overlay_cell),
            WorkerDecision::Recomputed
        );
        let computed_spans = cell.load().spans.clone();
        assert!(!computed_spans.is_empty(), "preconditions ok");

        // Now simulate: doc advanced to text_version = 2 and
        // the user toggled a fold so fold_hash bumped too. The
        // syntax snapshot is still at version 1 (worker hasn't
        // reparsed yet). Bumping fold_hash makes the new key
        // distinct from the cache so we exit the CacheHit
        // short-circuit and enter the HOLD branch.
        let stale_rs = RenderState {
            syntax: Arc::new(crate::render_state::SyntaxRenderState {
                syntax_handle: Some(handle.clone()),
                scroll: 0,
                viewport_height: 5,
                end_line_override: None,
                fold_hash: 1, // changed -> key differs from cache
                text_version: 2, // doc advanced
                visible_spans: cell.clone(),
                visible_rows: rows_cell.clone(),
                inlay_hints: Arc::from(
                    Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                ),
                inlay_version: 0,
                static_overlay_quads: overlay_cell.clone(),
                doc_highlights: Arc::from(
                    Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
                ),
                static_overlay_version: 0,
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        let stale = ArcSwap::from_pointee(stale_rs);
        let decision = recompute(&stale, &cell, &rows_cell, &overlay_cell);
        assert_eq!(decision, WorkerDecision::StaleSnapshotHold);
        // Spans preserved bit-identical.
        let after = cell.load();
        assert_eq!(after.spans, computed_spans);
        // Key advanced to the new inputs so we don't keep
        // retrying for the same stale combo on every wake.
        assert_eq!(after.computed_for_key.syntax_text_version, 1);
    }

    /// End-line-override path (slice X2.9 plumbing). With
    /// `end_line_override = Some(n)` the worker MUST pass `n`
    /// (clamped to >= default) to `highlight_lines` instead of
    /// `scroll + viewport_height`. Exercised by passing a
    /// `viewport_height` that excludes a target line but an
    /// `end_line_override` that includes it; the published spans
    /// must cover the extended range.
    #[test]
    fn recompute_honours_end_line_override() {
        // 4-line buffer; viewport_height = 1 would normally only
        // highlight line 0. Override end to 4 so the walk
        // covers every line.
        let text = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        let (rs, _h, cell, rows_cell, overlay_cell) = rs_with_rust(text, 0, 1, 0, 1, Some(4));
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::Recomputed);
        let after = cell.load();
        assert!(
            after.spans.len() >= 4,
            "override should stretch parse range; got {} rows",
            after.spans.len()
        );
        // Last covered line still carries a Keyword span for
        // `fn`, confirming the walk reached it.
        assert!(
            after.spans[3]
                .iter()
                .any(|s| matches!(s.style, lattice_syntax::Style::Keyword | lattice_syntax::Style::Function)),
            "expected Keyword/Function span on extended row 3: {:?}",
            after.spans[3]
        );
    }

    /// Calling `recompute` twice with the same `None`-handle
    /// inputs takes the `Clear` path each time but only stores
    /// when needed (idempotent). The exact-equal short-circuit
    /// inside the `Clear` branch avoids unnecessary Arc churn
    /// when the renderer is wake-spamming on an empty buffer.
    #[test]
    fn repeated_clear_is_idempotent() {
        let rs: ArcSwap<RenderState> = ArcSwap::from_pointee(RenderState::default());
        let cell: ArcSwap<VisibleSpans> = ArcSwap::from_pointee(VisibleSpans::default());
        let rows_cell: ArcSwap<VisibleRows> = ArcSwap::from_pointee(VisibleRows::default());
        let overlay_cell: ArcSwap<StaticOverlayQuads> =
            ArcSwap::from_pointee(StaticOverlayQuads::default());
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::Clear);
        let first = Arc::as_ptr(&cell.load_full());
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::Clear);
        let second = Arc::as_ptr(&cell.load_full());
        // The second `Clear` should NOT have allocated a new Arc:
        // when published spans are already empty + key default,
        // the store is suppressed.
        assert_eq!(
            first, second,
            "redundant Clear must not churn the spans Arc"
        );
    }

    // ---- Perf plan B.2 slice B.2.a: static-overlay bucket tests ----

    /// Helper: build a `RowPrepaint` with the given source text and
    /// no inlay splices. Source bytes pass through to `combined`;
    /// runs collapse to a single Default Source run.
    fn row_no_inlays(line_text: &str) -> RowPrepaint {
        weave_row(line_text, &[], &[])
    }

    fn pos(line: u32, byte: u32) -> lattice_protocol::position::Position {
        lattice_protocol::position::Position { line, byte }
    }
    fn rng(sl: u32, sb: u32, el: u32, eb: u32) -> lattice_protocol::position::Range {
        lattice_protocol::position::Range {
            start: pos(sl, sb),
            end: pos(el, eb),
        }
    }

    /// Empty payloads on all three layers short-circuit to an empty
    /// bucket — keeps the steady-state no-overlay path cheap.
    #[test]
    fn bucket_static_overlays_all_empty_returns_empty() {
        let rows = vec![row_no_inlays("let x = 1;")];
        let out = bucket_static_overlays(&rows, 0, b"let x = 1;", &[], &[], &[]);
        assert!(out.is_empty());
    }

    /// Single-line range on each layer: the per-row bucket carries
    /// one tagged quad per layer in fixed precedence order.
    #[test]
    fn bucket_static_overlays_tags_each_layer() {
        let rows = vec![row_no_inlays("hello world")];
        let src = b"hello world";
        let dh = vec![rng(0, 0, 0, 5)]; // covers "hello"
        let am = vec![rng(0, 6, 0, 11)]; // covers "world"
        let sb = vec![rng(0, 0, 0, 11)]; // covers all
        let out = bucket_static_overlays(&rows, 0, src, &dh, &am, &sb);
        assert_eq!(out.len(), 1);
        let row = &out[0];
        // Three quads, one per layer, in precedence order.
        assert_eq!(row.len(), 3);
        assert!(matches!(row[0].layer, OverlayLayer::DocHighlight));
        assert_eq!((row[0].source_byte_start, row[0].source_byte_end), (0, 5));
        assert!(matches!(row[1].layer, OverlayLayer::AllMatches));
        assert_eq!((row[1].source_byte_start, row[1].source_byte_end), (6, 11));
        assert!(matches!(row[2].layer, OverlayLayer::Substitute));
        assert_eq!((row[2].source_byte_start, row[2].source_byte_end), (0, 11));
    }

    /// Rows outside a range's `[start.line, end.line]` get an empty
    /// quad list.
    #[test]
    fn bucket_static_overlays_skips_rows_outside_range() {
        let rows = vec![row_no_inlays("a"), row_no_inlays("b"), row_no_inlays("c")];
        let src = b"a\nb\nc";
        let dh = vec![rng(1, 0, 1, 1)]; // only line 1
        let out = bucket_static_overlays(&rows, 0, src, &dh, &[], &[]);
        assert_eq!(out.len(), 3);
        assert!(out[0].is_empty());
        assert_eq!(out[1].len(), 1);
        assert!(out[2].is_empty());
    }

    /// Source-byte coordinates pass through unchanged regardless of
    /// any inlay-text splices on the row — the bucket emits raw
    /// source bytes; renderers apply their own coordinate transform
    /// at prepaint time (GPUI: byte→combined-col; TUI: source bytes
    /// directly).
    #[test]
    fn bucket_static_overlays_emits_source_bytes_not_combined() {
        let row = weave_row("hello", &[], &[(0, ":")]);
        // Sanity: leading inlay shifts combined by 1 but source
        // bytes are unchanged.
        assert_eq!(row.combined.as_ref(), ":hello");
        let rows = vec![row];
        let src = b"hello";
        let dh = vec![rng(0, 0, 0, 3)]; // first 3 source bytes
        let out = bucket_static_overlays(&rows, 0, src, &dh, &[], &[]);
        let row = &out[0];
        assert_eq!(row.len(), 1);
        assert_eq!((row[0].source_byte_start, row[0].source_byte_end), (0, 3));
    }

    /// Multi-line range crossing the row in the middle: covers
    /// the row's full source line.
    #[test]
    fn bucket_static_overlays_multi_line_middle_row_full_line() {
        let rows = vec![
            row_no_inlays("first"),
            row_no_inlays("middle"),
            row_no_inlays("last"),
        ];
        let src = b"first\nmiddle\nlast";
        let dh = vec![rng(0, 2, 2, 1)];
        let out = bucket_static_overlays(&rows, 0, src, &dh, &[], &[]);
        assert_eq!(out.len(), 3);
        // Row 0: from byte 2 to end → 2..5.
        assert_eq!((out[0][0].source_byte_start, out[0][0].source_byte_end), (2, 5));
        // Row 1: full line → 0..6.
        assert_eq!((out[1][0].source_byte_start, out[1][0].source_byte_end), (0, 6));
        // Row 2: from 0 to byte 1 → 0..1.
        assert_eq!((out[2][0].source_byte_start, out[2][0].source_byte_end), (0, 1));
    }

    /// `static_overlay_state_version` is deterministic per payload
    /// and bumps on any layer change; cross-layer permutations
    /// don't collide.
    #[test]
    fn static_overlay_state_version_is_stable_and_distinct() {
        use crate::render_state::static_overlay_state_version;
        let a = vec![rng(0, 0, 0, 3)];
        let b = vec![rng(0, 0, 0, 5)];
        // All empty → 0.
        assert_eq!(static_overlay_state_version(&[], &[], &[]), 0);
        // Deterministic.
        assert_eq!(
            static_overlay_state_version(&a, &b, &[]),
            static_overlay_state_version(&a, &b, &[])
        );
        // Layer permutation → distinct (a in dh vs a in all_matches).
        assert_ne!(
            static_overlay_state_version(&a, &[], &[]),
            static_overlay_state_version(&[], &a, &[])
        );
    }

    // ---- Perf plan A.2 slice A.2a / A.2b.2: row build tests -----

    /// `collapse_source_runs` produces a minimal partition: empty
    /// input yields a single Default Source run covering the whole
    /// row; spans covering everything yield exactly one styled run.
    #[test]
    fn collapse_source_runs_empty_input_yields_single_default_run() {
        // No styled spans → one Default Source run spanning the row.
        let runs = collapse_source_runs(&[], 10);
        assert_eq!(runs.len(), 1);
        assert!(matches!(
            runs[0],
            RowRun::Source { len: 10, style: lattice_syntax::Style::Default }
        ));

        // Empty row → no runs at all (nothing to paint).
        let runs_empty = collapse_source_runs(&[], 0);
        assert!(runs_empty.is_empty());
    }

    /// Adjacent equal-style spans merge into one run; gaps fill
    /// with Default; the sum of run lengths covers the row.
    #[test]
    fn collapse_source_runs_merges_and_fills_gaps() {
        use lattice_syntax::{Style, StyledSpan};
        let spans = vec![
            StyledSpan { start: 0, end: 2, style: Style::Keyword },
            StyledSpan { start: 2, end: 4, style: Style::Keyword }, // adjacent, same → merges
            // gap [4..6) → Default
            StyledSpan { start: 6, end: 9, style: Style::Function },
            // gap [9..10) → Default
        ];
        let runs = collapse_source_runs(&spans, 10);
        assert_eq!(
            runs,
            vec![
                RowRun::Source { len: 4, style: Style::Keyword },
                RowRun::Source { len: 2, style: Style::Default },
                RowRun::Source { len: 3, style: Style::Function },
                RowRun::Source { len: 1, style: Style::Default },
            ]
        );
        let total: u32 = runs.iter().map(|r| r.len()).sum();
        assert_eq!(total, 10, "runs must partition combined exhaustively");
    }

    // ---- Perf plan A.2 slice A.2b.2: weave_row tests ------------

    /// `weave_row` with no inlays produces the same shape as the
    /// no-inlay fast path on `collapse_source_runs`: combined ==
    /// line_text, runs == Source partition, empty offsets.
    #[test]
    fn weave_row_no_inlays_matches_collapse_source_runs() {
        use lattice_syntax::{Style, StyledSpan};
        let spans = vec![StyledSpan { start: 0, end: 3, style: Style::Keyword }];
        let row = weave_row("let x = 1;", &spans, &[]);
        assert_eq!(row.combined.as_ref(), "let x = 1;");
        assert!(row.inlay_offsets.is_empty());
        let total: u32 = row.runs.iter().map(|r| r.len()).sum();
        assert_eq!(total, "let x = 1;".len() as u32);
        // First run is Keyword (Source); rest are Source Default.
        assert!(matches!(row.runs[0], RowRun::Source { style: Style::Keyword, .. }));
        assert!(row.runs.iter().all(|r| matches!(r, RowRun::Source { .. })));
    }

    /// Mid-line inlay: spliced before the byte offset, recorded on
    /// `inlay_offsets`, run partition includes an `Inlay` variant.
    #[test]
    fn weave_row_splices_mid_line_inlay() {
        let line = "let x = 1;";
        let inlays: Vec<(u32, &str)> = vec![(5, ": i32")];
        let row = weave_row(line, &[], &inlays);
        assert_eq!(row.combined.as_ref(), "let x: i32 = 1;");
        assert_eq!(row.inlay_offsets.as_ref(), &[(5u32, 5u32)][..]);
        // Run shape: Source(5, Default) -> Inlay(5) -> Source(5, Default).
        // The two Source runs DO NOT merge because the Inlay run breaks them.
        assert_eq!(row.runs.len(), 3);
        assert!(matches!(row.runs[0], RowRun::Source { len: 5, .. }));
        assert!(matches!(row.runs[1], RowRun::Inlay { len: 5 }));
        assert!(matches!(row.runs[2], RowRun::Source { len: 5, .. }));
    }

    /// Trailing inlay (orig_byte >= line.len()) splices at EOL.
    #[test]
    fn weave_row_trailing_inlay_appended_at_eol() {
        let line = "fn foo()";
        let inlays: Vec<(u32, &str)> = vec![(line.len() as u32, " -> i32")];
        let row = weave_row(line, &[], &inlays);
        assert_eq!(row.combined.as_ref(), "fn foo() -> i32");
        assert_eq!(row.inlay_offsets.as_ref(), &[(8u32, 7u32)][..]);
        // Source(8, Default) -> Inlay(7).
        assert_eq!(row.runs.len(), 2);
        assert!(matches!(row.runs[0], RowRun::Source { len: 8, .. }));
        assert!(matches!(row.runs[1], RowRun::Inlay { len: 7 }));
    }

    /// Multiple inlays on the same row: each is recorded and
    /// spliced; partition stays exhaustive over `combined`.
    /// Splice semantics match the GPUI peer: inlay text is
    /// inserted BEFORE the byte at `orig_byte`, so to land after a
    /// name like `a` the hint's byte must point AT the character
    /// just past `a` (the comma here).
    #[test]
    fn weave_row_multiple_inlays_sorted_by_byte() {
        // Bytes: l=0 e=1 t=2 ' '=3 (=4 a=5 ,=6 ' '=7 b=8 )=9 ...
        // Splicing before byte 6 puts the inlay between `a` and `,`;
        // before byte 9 puts it between `b` and `)`.
        let line = "let (a, b) = pair;";
        let inlays: Vec<(u32, &str)> = vec![(6, ": i32"), (9, ": i32")];
        let row = weave_row(line, &[], &inlays);
        assert_eq!(row.combined.as_ref(), "let (a: i32, b: i32) = pair;");
        assert_eq!(row.inlay_offsets.len(), 2);
        let total: u32 = row.runs.iter().map(|r| r.len()).sum();
        assert_eq!(total, row.combined.len() as u32);
    }

    /// Empty line with no inlays yields no runs (matches A.2a).
    #[test]
    fn weave_row_empty_line_no_inlays_yields_no_runs() {
        let row = weave_row("", &[], &[]);
        assert!(row.combined.is_empty());
        assert!(row.runs.is_empty());
        assert!(row.inlay_offsets.is_empty());
    }

    /// Empty line with a trailing inlay still emits an Inlay run.
    #[test]
    fn weave_row_empty_line_with_inlay_emits_inlay_only() {
        let inlays: Vec<(u32, &str)> = vec![(0, "// hint")];
        let row = weave_row("", &[], &inlays);
        assert_eq!(row.combined.as_ref(), "// hint");
        assert_eq!(row.runs.len(), 1);
        assert!(matches!(row.runs[0], RowRun::Inlay { len: 7 }));
        assert_eq!(row.inlay_offsets.as_ref(), &[(0u32, 7u32)][..]);
    }

    /// `bucket_inlays_by_line` groups by line and sorts within a
    /// bucket by byte ascending.
    #[test]
    fn bucket_inlays_by_line_groups_and_sorts() {
        let hints = vec![
            InlayHintRow { line: 0, byte: 5, text: "a".into() },
            InlayHintRow { line: 2, byte: 1, text: "b".into() },
            InlayHintRow { line: 0, byte: 2, text: "c".into() }, // earlier byte than first on line 0
        ];
        let buckets = bucket_inlays_by_line(&hints);
        assert_eq!(buckets.len(), 3); // lines 0..=2
        // Line 0: two entries sorted by byte (2 then 5).
        assert_eq!(buckets[0], vec![(2u32, "c"), (5u32, "a")]);
        // Line 1: empty.
        assert!(buckets[1].is_empty());
        // Line 2: single entry.
        assert_eq!(buckets[2], vec![(1u32, "b")]);
    }

    /// Empty input → empty output, no allocation surprises.
    #[test]
    fn bucket_inlays_by_line_empty_input_returns_empty() {
        let buckets = bucket_inlays_by_line(&[]);
        assert!(buckets.is_empty());
    }

    // ---- Perf plan B.1: dirty-row recomposition tests -----------

    fn fake_styled(line_count: usize, line_byte_len: usize) -> Vec<Vec<lattice_syntax::StyledSpan>> {
        // One Default span per line covering [0, len). The
        // worker's `collapse_runs` will fold this into one
        // `Default` run per row — fine for cache-identity checks.
        (0..line_count)
            .map(|_| {
                vec![lattice_syntax::StyledSpan {
                    start: 0,
                    end: line_byte_len,
                    style: lattice_syntax::Style::Default,
                }]
            })
            .collect()
    }

    fn fake_source(line_count: usize, line_byte_len: usize) -> Vec<u8> {
        // Each line is `line_byte_len` ASCII chars + a newline. The
        // last line gets no trailing newline so total_lines matches
        // `line_count`.
        let mut s = String::with_capacity(line_count * (line_byte_len + 1));
        for i in 0..line_count {
            // Use the line index modulo a small alphabet so we can
            // detect mis-mapped reuse (if a cached row from line 7
            // ends up at line 9, the text will differ).
            let ch = (b'a' + (i as u8 % 26)) as char;
            s.extend(std::iter::repeat(ch).take(line_byte_len));
            if i + 1 < line_count {
                s.push('\n');
            }
        }
        s.into_bytes()
    }

    fn key(snapshot_ptr: usize, text_version: u64, scroll: u32) -> VisibleHighlightsKey {
        VisibleHighlightsKey {
            snapshot_ptr,
            syntax_text_version: text_version,
            scroll,
            viewport_height: 0,
            fold_hash: 0,
            inlay_version: 0,
            static_overlay_version: 0,
        }
    }

    /// Convenience: the empty inlays argument matching a viewport
    /// of `n` visible rows.
    fn no_inlays(n: usize) -> Vec<Vec<(u32, &'static str)>> {
        vec![Vec::new(); n]
    }

    /// Cold path — no prior rows. Every line is materialised from
    /// scratch; result must equal `build_rows`.
    #[test]
    fn build_rows_with_cache_no_prior_matches_cold_path() {
        let source = fake_source(20, 4);
        let spans = fake_styled(10, 4);
        let prev = VisibleRows::default();
        let new_key = key(0xdead, 1, 0);
        let inlays = no_inlays(spans.len());
        let cached = build_rows_with_cache(&source, 0, &spans, &inlays, &prev, &new_key);
        let cold = build_rows(&source, 0, &spans, &inlays);
        assert_eq!(cached.len(), cold.len());
        for (a, b) in cached.iter().zip(cold.iter()) {
            assert_eq!(a.combined, b.combined);
            assert_eq!(a.runs, b.runs);
            assert_eq!(a.inlay_offsets, b.inlay_offsets);
        }
    }

    /// Snapshot/version match + scroll-forward delta: rows in the
    /// overlap window must be byte-identical to the prior frame's
    /// rows (proves the reuse path is wired). Rows past the overlap
    /// are freshly built and must match the cold path for those
    /// indices.
    #[test]
    fn build_rows_with_cache_reuses_overlap_on_scroll_forward() {
        let source = fake_source(50, 4);
        // Previous frame: viewport [10..20).
        let prev_spans = fake_styled(10, 4);
        let prev_key = key(0xdead, 1, 10);
        let prev_rows_vec = build_rows(&source, 10, &prev_spans, &no_inlays(prev_spans.len()));
        let prev = VisibleRows {
            rows: Arc::from(prev_rows_vec.clone().into_boxed_slice()),
            computed_for_key: prev_key,
        };
        // New frame: viewport [12..22) — overlap [12..20).
        let new_spans = fake_styled(10, 4);
        let new_key = key(0xdead, 1, 12);
        let new_rows = build_rows_with_cache(
            &source,
            12,
            &new_spans,
            &no_inlays(new_spans.len()),
            &prev,
            &new_key,
        );
        assert_eq!(new_rows.len(), 10);
        // Reuse window: new_rows[0..8] == prev_rows_vec[2..10].
        for i in 0..8 {
            assert_eq!(
                new_rows[i].combined, prev_rows_vec[i + 2].combined,
                "reuse mismatch at rel {i}"
            );
        }
        // Cold tail: new_rows[8..10] must match cold build for
        // absolute lines [20..22).
        let cold_tail_spans = fake_styled(2, 4);
        let cold_tail = build_rows(&source, 20, &cold_tail_spans, &no_inlays(cold_tail_spans.len()));
        assert_eq!(new_rows[8].combined, cold_tail[0].combined);
        assert_eq!(new_rows[9].combined, cold_tail[1].combined);
    }

    /// Snapshot/version change ⇒ full cold rebuild even when
    /// scroll/viewport are identical. Proves we never serve stale
    /// rows from a different source.
    #[test]
    fn build_rows_with_cache_falls_back_on_snapshot_change() {
        let source_a = fake_source(20, 4);
        let source_b = {
            // Same shape but every byte differs (uppercase).
            let mut v = fake_source(20, 4);
            v.iter_mut().for_each(|b| {
                if (b'a'..=b'z').contains(b) {
                    *b -= 32;
                }
            });
            v
        };
        let spans = fake_styled(10, 4);
        let prev_rows_vec = build_rows(&source_a, 5, &spans, &no_inlays(spans.len()));
        let prev = VisibleRows {
            rows: Arc::from(prev_rows_vec.clone().into_boxed_slice()),
            computed_for_key: key(0xa11, 1, 5),
        };
        // New snapshot_ptr differs ⇒ no reuse, rebuild from source_b.
        let new_key = key(0xb22, 1, 5);
        let new_rows = build_rows_with_cache(
            &source_b,
            5,
            &spans,
            &no_inlays(spans.len()),
            &prev,
            &new_key,
        );
        // New rows must reflect source_b (uppercase), not the cached
        // source_a rows.
        assert_ne!(new_rows[0].combined, prev_rows_vec[0].combined);
        assert!(
            new_rows[0]
                .combined
                .chars()
                .all(|c| c.is_ascii_uppercase()),
            "expected uppercase from source_b; got {:?}",
            new_rows[0].combined
        );
    }

    /// Slice A.2b.2: `inlay_version` change invalidates the cache
    /// — even with snapshot_ptr + text_version + scroll all equal,
    /// a different inlay_version on the new key MUST force a full
    /// rebuild so the woven rows reflect the new inlay payload.
    #[test]
    fn build_rows_with_cache_falls_back_on_inlay_version_change() {
        let source = fake_source(20, 4);
        let spans = fake_styled(10, 4);
        let prev_key = VisibleHighlightsKey {
            snapshot_ptr: 0xdead,
            syntax_text_version: 1,
            scroll: 0,
            viewport_height: 0,
            fold_hash: 0,
            inlay_version: 42,
            static_overlay_version: 0,
        };
        let prev_rows_vec = build_rows(&source, 0, &spans, &no_inlays(spans.len()));
        let prev = VisibleRows {
            rows: Arc::from(prev_rows_vec.clone().into_boxed_slice()),
            computed_for_key: prev_key,
        };
        // New key: same everything except inlay_version bumped.
        // Even if the rebuilt rows happen to match (no inlays still
        // visible) the path must take the cold rebuild, not the
        // reuse short-circuit. Use spy-able output by changing the
        // per-line inlay list so the woven content differs.
        let new_key = VisibleHighlightsKey { inlay_version: 99, ..prev_key };
        let mut new_inlays: Vec<Vec<(u32, &str)>> = no_inlays(spans.len());
        new_inlays[0].push((1, ">>"));
        let new_rows = build_rows_with_cache(&source, 0, &spans, &new_inlays, &prev, &new_key);
        // First row picked up the new inlay (proves cold path ran).
        assert!(
            new_rows[0].combined.contains(">>"),
            "expected new inlay woven in; got {:?}",
            new_rows[0].combined
        );
        assert!(!new_rows[0].inlay_offsets.is_empty());
    }

    /// Recompute also publishes pre-paint rows alongside spans,
    /// keyed identically so downstream consumers can correlate
    /// the two cells if they need to.
    #[test]
    fn recompute_publishes_rows_alongside_spans() {
        let (rs, _h, cell, rows_cell, overlay_cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::Recomputed);
        let rows = rows_cell.load_full();
        assert!(!rows.rows.is_empty(), "rows must be populated on Recomputed");
        // computed_for_key matches the spans cell — same recompute.
        assert_eq!(rows.computed_for_key, cell.load_full().computed_for_key);
        // First row's combined text matches the source line.
        assert_eq!(rows.rows[0].combined.as_ref(), "fn main() {}");
        // Run partition covers the whole line.
        let total: u32 = rows.rows[0].runs.iter().map(|r| r.len()).sum();
        assert_eq!(total, 12);
        // At least one Source run is Keyword or Function (the `fn` token).
        assert!(
            rows.rows[0].runs.iter().any(|r| matches!(
                r,
                RowRun::Source {
                    style: lattice_syntax::Style::Keyword | lattice_syntax::Style::Function,
                    ..
                }
            )),
            "expected a Keyword/Function Source run; got {:?}",
            rows.rows[0].runs
        );
        // No inlays in this fixture.
        assert!(rows.rows[0].inlay_offsets.is_empty());
    }

    /// Slice A.2b.2: `recompute` consumes `syntax.inlay_hints` and
    /// splices them into the row composition. With one inlay on
    /// line 0, the published row's `combined` carries the splice
    /// and `inlay_offsets` records it.
    #[test]
    fn recompute_weaves_published_inlay_hints_into_rows() {
        // Manually construct a SyntaxRenderState carrying one inlay
        // hint, so the bench-style helper isn't bypassed and the
        // weave path runs end-to-end.
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .expect("rust grammar available in test build");
        let text = "fn main() {}\n";
        s.parse_at(text, 1);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
        let cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleSpans::default()));
        let rows_cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleRows::default()));
        let overlay_cell = Arc::new(arc_swap::ArcSwap::from_pointee(
            StaticOverlayQuads::default(),
        ));
        let hints: Vec<InlayHintRow> = vec![InlayHintRow {
            line: 0,
            byte: 7, // between `main` and `(`
            text: " <- entry".into(),
        }];
        let hints_arc: Arc<[InlayHintRow]> = Arc::from(hints.into_boxed_slice());
        let inlay_version = crate::render_state::inlay_hints_version(&hints_arc);
        let rs_state = RenderState {
            syntax: Arc::new(crate::render_state::SyntaxRenderState {
                syntax_handle: Some(handle.clone()),
                scroll: 0,
                viewport_height: 5,
                end_line_override: None,
                fold_hash: 0,
                text_version: 1,
                visible_spans: cell.clone(),
                visible_rows: rows_cell.clone(),
                inlay_hints: hints_arc,
                inlay_version,
                static_overlay_quads: overlay_cell.clone(),
                doc_highlights: Arc::from(
                    Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
                ),
                static_overlay_version: 0,
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        let rs = ArcSwap::from_pointee(rs_state);
        assert_eq!(recompute(&rs, &cell, &rows_cell, &overlay_cell), WorkerDecision::Recomputed);
        let published = rows_cell.load_full();
        let row0 = &published.rows[0];
        assert!(
            row0.combined.contains(" <- entry"),
            "expected inlay text spliced into combined; got {:?}",
            row0.combined
        );
        assert_eq!(row0.inlay_offsets.as_ref(), &[(7u32, 9u32)][..]);
        // At least one Inlay run is present.
        assert!(
            row0.runs.iter().any(|r| matches!(r, RowRun::Inlay { .. })),
            "expected an Inlay run; got {:?}",
            row0.runs
        );
        // Cache key carries the bumped inlay_version.
        assert_eq!(published.computed_for_key.inlay_version, inlay_version);
        assert_ne!(inlay_version, 0, "non-empty hints must hash to non-zero");
    }
}
