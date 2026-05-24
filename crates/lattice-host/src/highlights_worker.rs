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
    RenderState, RowPrepaint, RowRun, VisibleHighlightsKey, VisibleRows, VisibleSpans,
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
        let decision = recompute(&render_state, &spans_cell, &rows_cell);
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
    // view when `(snapshot_ptr, syntax_text_version)` are
    // unchanged. On the scroll-only path (the dominant held-j
    // case) every overlapping line skips the per-line text fetch
    // + collapse, dropping `build_rows` cost from O(viewport) to
    // O(scroll_delta).
    let prev_rows = rows_cell.load_full();
    let rows = build_rows_with_cache(
        snap.source(),
        start,
        &spans,
        &prev_rows,
        &key,
    );

    spans_cell.store(Arc::new(VisibleSpans {
        spans,
        computed_for_key: key,
    }));
    rows_cell.store(Arc::new(VisibleRows {
        rows,
        computed_for_key: key,
    }));
    WorkerDecision::Recomputed
}

/// Perf plan B.1 (dirty-row recomposition). Wraps [`build_rows`]
/// with a per-absolute-line reuse path: when the previous publish
/// was against the same `(snapshot_ptr, syntax_text_version)`
/// (the source bytes are bit-identical), the prepaints for any
/// absolute line that's still in view are reused as-is. Only
/// newly-visible lines hit `build_rows` for the per-line memchr
/// scan + `into_boxed_str` alloc + `collapse_runs` walk.
///
/// The dominant held-j scroll case (snapshot + version unchanged,
/// scroll deltas by 1) reuses ~99% of rows; build cost collapses
/// from `O(viewport_height)` to `O(scroll_delta)`.
///
/// The cache only kicks in for `(snapshot_ptr, text_version)` parity
/// — every other key axis (fold_hash, viewport_height, scroll) is
/// allowed to differ. Mismatched axes change WHICH absolute lines
/// are visible but not the per-absolute-line content. When the
/// snapshot or version flips (any edit), we fall through to the
/// from-scratch path so the published rows reflect the new source.
fn build_rows_with_cache(
    source: &[u8],
    start: u32,
    styled_spans: &[Vec<lattice_syntax::StyledSpan>],
    prev_rows: &VisibleRows,
    new_key: &VisibleHighlightsKey,
) -> Vec<RowPrepaint> {
    // Bail to the cold path on any source-affecting input change.
    // `snapshot_ptr` flips on every reparse-produced snapshot;
    // `syntax_text_version` flips on edits even when the parser
    // re-uses an Arc'd Tree. Either change means line content (or
    // line count, or byte offsets) may differ.
    let snapshot_match = prev_rows.computed_for_key.snapshot_ptr == new_key.snapshot_ptr
        && prev_rows.computed_for_key.syntax_text_version == new_key.syntax_text_version;
    if !snapshot_match || prev_rows.rows.is_empty() {
        return build_rows(source, start, styled_spans);
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
        let combined: Box<str> = line_text.into();
        let runs = collapse_runs(line_spans, combined.len() as u32);
        rows.push(RowPrepaint { combined, runs });
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
/// `Box<str>` plus its collapsed `RowRun` partition. `source` is
/// the SyntaxSnapshot's bytes — same bytes the spans index into —
/// so the row text and span offsets stay aligned even if the
/// document snapshot has raced ahead of the syntax parse.
///
/// Inlay weave is deferred to A.2b — `combined` always equals the
/// source line text in A.2a.
fn build_rows(
    source: &[u8],
    start: u32,
    styled_spans: &[Vec<lattice_syntax::StyledSpan>],
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
    for line_spans in styled_spans {
        let line_end = memchr::memchr(b'\n', &source[byte_off..])
            .map(|n| byte_off + n)
            .unwrap_or(source.len());
        let line_bytes = &source[byte_off..line_end];
        let line_text = std::str::from_utf8(line_bytes).unwrap_or("");
        let combined: Box<str> = line_text.into();
        let runs = collapse_runs(line_spans, combined.len() as u32);
        rows.push(RowPrepaint { combined, runs });
        byte_off = (line_end + 1).min(source.len());
    }
    rows
}

/// Collapse adjacent equal-`Style` spans into a minimal `RowRun`
/// partition covering the row's `combined` text exhaustively.
///
/// - Empty `line_spans` yields a single `Style::Default` run of
///   length `combined_len` (so renderers always have a valid
///   partition to walk).
/// - Gaps between spans (uncovered byte ranges between `prev.end`
///   and `next.start`) are filled with `Style::Default` runs so
///   `sum(runs[*].len) == combined_len`.
/// - Spans whose end exceeds `combined_len` are clamped — the
///   highlight grammar can produce one-past-the-end ranges on
///   blank lines and we don't want to overflow.
fn collapse_runs(line_spans: &[lattice_syntax::StyledSpan], combined_len: u32) -> Vec<RowRun> {
    if combined_len == 0 {
        return Vec::new();
    }
    let mut runs: Vec<RowRun> = Vec::with_capacity(line_spans.len().max(1));
    let mut cursor: u32 = 0;

    let push = |runs: &mut Vec<RowRun>, style: lattice_syntax::Style, len: u32| {
        if len == 0 {
            return;
        }
        if let Some(last) = runs.last_mut() {
            if last.style == style {
                last.len += len;
                return;
            }
        }
        runs.push(RowRun { len, style });
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
            spans: vec![Vec::new()],
            computed_for_key: VisibleHighlightsKey {
                snapshot_ptr: 0xdead,
                ..Default::default()
            },
        });
        let rows_cell: ArcSwap<VisibleRows> = ArcSwap::from_pointee(VisibleRows::default());
        let decision = recompute(&rs, &cell, &rows_cell);
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
    ) {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .expect("rust grammar available in test build");
        s.parse_at(text, text_version);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
        let cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleSpans::default()));
        let rows_cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleRows::default()));
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
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        (ArcSwap::from_pointee(rs), handle, cell, rows_cell)
    }

    /// Cache miss path: with a current snapshot and a fresh
    /// (unseen) key, the worker walks `highlight_lines` and
    /// publishes the resulting spans into the cell.
    #[test]
    fn recompute_with_current_snapshot_publishes_spans() {
        let (rs, _h, cell, rows_cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        let decision = recompute(&rs, &cell, &rows_cell);
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
        let (rs, _h, cell, rows_cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs, &cell, &rows_cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&cell.load_full());
        assert_eq!(recompute(&rs, &cell, &rows_cell), WorkerDecision::CacheHit);
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
        let (rs_initial, handle, cell, rows_cell) =
            rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs_initial, &cell, &rows_cell), WorkerDecision::Recomputed);
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
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        let stale = ArcSwap::from_pointee(stale_rs);
        let decision = recompute(&stale, &cell, &rows_cell);
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
        let (rs, _h, cell, rows_cell) = rs_with_rust(text, 0, 1, 0, 1, Some(4));
        assert_eq!(recompute(&rs, &cell, &rows_cell), WorkerDecision::Recomputed);
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
        assert_eq!(recompute(&rs, &cell, &rows_cell), WorkerDecision::Clear);
        let first = Arc::as_ptr(&cell.load_full());
        assert_eq!(recompute(&rs, &cell, &rows_cell), WorkerDecision::Clear);
        let second = Arc::as_ptr(&cell.load_full());
        // The second `Clear` should NOT have allocated a new Arc:
        // when published spans are already empty + key default,
        // the store is suppressed.
        assert_eq!(
            first, second,
            "redundant Clear must not churn the spans Arc"
        );
    }

    // ---- Perf plan A.2 slice A.2a: VisibleRows-specific tests ----

    /// `collapse_runs` produces a minimal partition: empty input
    /// yields a single Default run covering the whole row; spans
    /// covering everything yield exactly one styled run.
    #[test]
    fn collapse_runs_empty_input_yields_single_default_run() {
        // No styled spans → one Default run spanning the row.
        let runs = collapse_runs(&[], 10);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 10);
        assert_eq!(runs[0].style, lattice_syntax::Style::Default);

        // Empty row → no runs at all (nothing to paint).
        let runs_empty = collapse_runs(&[], 0);
        assert!(runs_empty.is_empty());
    }

    /// Adjacent equal-style spans merge into one run; gaps fill
    /// with Default; the sum of run lengths covers the row.
    #[test]
    fn collapse_runs_merges_and_fills_gaps() {
        use lattice_syntax::{Style, StyledSpan};
        let spans = vec![
            StyledSpan { start: 0, end: 2, style: Style::Keyword },
            StyledSpan { start: 2, end: 4, style: Style::Keyword }, // adjacent, same → merges
            // gap [4..6) → Default
            StyledSpan { start: 6, end: 9, style: Style::Function },
            // gap [9..10) → Default
        ];
        let runs = collapse_runs(&spans, 10);
        assert_eq!(
            runs,
            vec![
                RowRun { len: 4, style: Style::Keyword },
                RowRun { len: 2, style: Style::Default },
                RowRun { len: 3, style: Style::Function },
                RowRun { len: 1, style: Style::Default },
            ]
        );
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, 10, "runs must partition combined exhaustively");
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
        }
    }

    /// Cold path — no prior rows. Every line is materialised from
    /// scratch; result must equal `build_rows`.
    #[test]
    fn build_rows_with_cache_no_prior_matches_cold_path() {
        let source = fake_source(20, 4);
        let spans = fake_styled(10, 4);
        let prev = VisibleRows::default();
        let new_key = key(0xdead, 1, 0);
        let cached = build_rows_with_cache(&source, 0, &spans, &prev, &new_key);
        let cold = build_rows(&source, 0, &spans);
        assert_eq!(cached.len(), cold.len());
        for (a, b) in cached.iter().zip(cold.iter()) {
            assert_eq!(a.combined, b.combined);
            assert_eq!(a.runs, b.runs);
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
        let prev_rows_vec = build_rows(&source, 10, &prev_spans);
        let prev = VisibleRows {
            rows: prev_rows_vec.clone(),
            computed_for_key: prev_key,
        };
        // New frame: viewport [12..22) — overlap [12..20).
        let new_spans = fake_styled(10, 4);
        let new_key = key(0xdead, 1, 12);
        let new_rows = build_rows_with_cache(&source, 12, &new_spans, &prev, &new_key);
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
        let cold_tail = build_rows(&source, 20, &fake_styled(2, 4));
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
        let prev_rows_vec = build_rows(&source_a, 5, &spans);
        let prev = VisibleRows {
            rows: prev_rows_vec.clone(),
            computed_for_key: key(0xa11, 1, 5),
        };
        // New snapshot_ptr differs ⇒ no reuse, rebuild from source_b.
        let new_key = key(0xb22, 1, 5);
        let new_rows = build_rows_with_cache(&source_b, 5, &spans, &prev, &new_key);
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

    /// Recompute also publishes pre-paint rows alongside spans,
    /// keyed identically so downstream consumers can correlate
    /// the two cells if they need to.
    #[test]
    fn recompute_publishes_rows_alongside_spans() {
        let (rs, _h, cell, rows_cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs, &cell, &rows_cell), WorkerDecision::Recomputed);
        let rows = rows_cell.load_full();
        assert!(!rows.rows.is_empty(), "rows must be populated on Recomputed");
        // computed_for_key matches the spans cell — same recompute.
        assert_eq!(rows.computed_for_key, cell.load_full().computed_for_key);
        // First row's combined text matches the source line.
        assert_eq!(rows.rows[0].combined.as_ref(), "fn main() {}");
        // Run partition covers the whole line.
        let total: u32 = rows.rows[0].runs.iter().map(|r| r.len).sum();
        assert_eq!(total, 12);
        // At least one run is a Keyword or Function (the `fn` token).
        assert!(
            rows.rows[0]
                .runs
                .iter()
                .any(|r| matches!(r.style, lattice_syntax::Style::Keyword | lattice_syntax::Style::Function)),
            "expected a Keyword/Function run; got {:?}",
            rows.rows[0].runs
        );
    }
}
