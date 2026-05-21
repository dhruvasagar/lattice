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
use crate::render_state::{RenderState, VisibleHighlightsKey, VisibleSpans};

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
        let decision = recompute(&render_state, &spans_cell);
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
    spans_cell.store(Arc::new(VisibleSpans {
        spans,
        computed_for_key: key,
    }));
    WorkerDecision::Recomputed
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
        let decision = recompute(&rs, &cell);
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
    ) {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .expect("rust grammar available in test build");
        s.parse_at(text, text_version);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
        let cell = Arc::new(arc_swap::ArcSwap::from_pointee(VisibleSpans::default()));
        let rs = RenderState {
            syntax: Arc::new(crate::render_state::SyntaxRenderState {
                syntax_handle: Some(handle.clone()),
                scroll,
                viewport_height,
                end_line_override,
                fold_hash,
                text_version,
                visible_spans: cell.clone(),
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        (ArcSwap::from_pointee(rs), handle, cell)
    }

    /// Cache miss path: with a current snapshot and a fresh
    /// (unseen) key, the worker walks `highlight_lines` and
    /// publishes the resulting spans into the cell.
    #[test]
    fn recompute_with_current_snapshot_publishes_spans() {
        let (rs, _h, cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        let decision = recompute(&rs, &cell);
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
        let (rs, _h, cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs, &cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&cell.load_full());
        assert_eq!(recompute(&rs, &cell), WorkerDecision::CacheHit);
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
        let (rs_initial, handle, cell) = rs_with_rust("fn main() {}", 0, 5, 0, 1, None);
        assert_eq!(recompute(&rs_initial, &cell), WorkerDecision::Recomputed);
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
                pane_highlights: Arc::new(std::collections::HashMap::new()),
            }),
            ..RenderState::default()
        };
        let stale = ArcSwap::from_pointee(stale_rs);
        let decision = recompute(&stale, &cell);
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
        let (rs, _h, cell) = rs_with_rust(text, 0, 1, 0, 1, Some(4));
        assert_eq!(recompute(&rs, &cell), WorkerDecision::Recomputed);
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
        assert_eq!(recompute(&rs, &cell), WorkerDecision::Clear);
        let first = Arc::as_ptr(&cell.load_full());
        assert_eq!(recompute(&rs, &cell), WorkerDecision::Clear);
        let second = Arc::as_ptr(&cell.load_full());
        // The second `Clear` should NOT have allocated a new Arc:
        // when published spans are already empty + key default,
        // the store is suppressed.
        assert_eq!(
            first, second,
            "redundant Clear must not churn the spans Arc"
        );
    }
}
