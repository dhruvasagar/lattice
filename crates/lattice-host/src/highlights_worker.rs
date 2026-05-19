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
use tracing::trace;

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
) {
    loop {
        // `notified().await` resolves when `notify_one()` is
        // called (or immediately if a permit is stored).
        // Permit-style coalescing: a burst of publishes wakes
        // us once; we read the LATEST snapshot below, which
        // captures the full effect of the burst.
        wake.0.notified().await;
        let decision = recompute(&render_state, &spans_cell);
        trace!(
            target: "lattice_host::highlights_worker",
            ?decision,
            "highlights worker tick"
        );
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
    let start = syntax.scroll;
    let end = syntax
        .scroll
        .saturating_add(syntax.viewport_height.max(1));
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
