//! Background overlay worker — static-overlay bucketing off the UI thread.
//!
//! Phase 5.8.AF.5 / Slice X2.3 (origin); display-line B4.2 (gut + rename).
//!
//! ## Why this exists
//!
//! Per paramount goal #1 in `CLAUDE.md`:
//!
//! > **Performance.** UI thread does no I/O, no parsing, no shaping.
//!
//! This worker pre-buckets the active document's *static* overlay
//! layers (hlsearch matches, LSP document-highlights, `:s///`
//! substitute preview) into per-row quad lists so neither renderer
//! peer has to walk every overlay range against every visible row
//! inside its per-frame paint body.
//!
//! ## History (B4.2 gut + rename)
//!
//! Until display-line slice B4.2 this module was `highlights_worker`
//! and carried TWO jobs:
//!
//! - the **span / row prepaint cache** (`VisibleSpans` /
//!   `VisibleRows` / `RowPrepaint`, built by `build_rows` /
//!   `build_rows_with_cache` / `weave_row`) — consumed by the
//!   renderers' shaping path. The cells / display-matrix migration
//!   (B-series) severed every read of that cache, so B4.2 deleted it
//!   wholesale.
//! - the **static-overlay bucket** (`bucket_static_overlays`) —
//!   still consumed every frame by both renderers' overlay paint
//!   paths (TUI `render.rs`, GPUI `editor_element.rs`). This is the
//!   live half and is all that remains here.
//!
//! The module was renamed `overlay_worker` because it no longer
//! produces highlights — only overlay quads.
//!
//! ## Design
//!
//! - Dispatch's `publish_render_state` populates
//!   [`crate::render_state::SyntaxRenderState`] inputs
//!   (`syntax_handle`, `scroll`, `viewport_height`, `fold_hash`,
//!   `text_version`, `doc_highlights`, `static_overlay_version`)
//!   and fires [`crate::editor::OverlayWake`]'s `Notify`.
//! - The worker `notified().await`s the wake signal. `Notify` is
//!   permit-style: a burst of publishes wakes the worker exactly
//!   once, after which the worker re-reads the *latest* snapshot.
//! - On wake the worker reads `render_state.load_full().syntax`,
//!   constructs a [`crate::render_state::VisibleHighlightsKey`],
//!   compares it against the key in the currently-published
//!   [`crate::render_state::StaticOverlayQuads`], and short-circuits
//!   on cache-hit.
//! - On cache-miss with a current snapshot: re-buckets the overlay
//!   layers into per-row quad lists and stores a fresh
//!   `StaticOverlayQuads` into the durable
//!   `syntax_static_overlay_quads_cell` `Arc<ArcSwap<…>>`.
//! - On cache-miss with a *stale* snapshot (snapshot's
//!   `text_version` < document's `text_version`): the worker applies
//!   the stale-snapshot HOLD — it stores the *new* key but preserves
//!   the previously-published quads, so a mid-edit window doesn't
//!   recolour overlay backgrounds against pre-edit data.
//!
//! ## Renderer contract
//!
//! Renderer peers read with:
//!
//! ```text
//! let rs = editor.render_state.load_full();
//! let quads = rs.syntax.static_overlay_quads.load();
//! // quads.quads[i] = per-row RowOverlayQuad list for visible line i
//! ```
//!
//! Empty `quads` is the legal "no overlays yet" state (initial boot,
//! no overlay layers active, or a pre-first-worker-tick window).
//! Renderers paint no overlay backgrounds in that case.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{debug, trace};

use crate::editor::OverlayWake;
use crate::render_state::{
    OverlayLayer, RenderState, RowOverlayQuad, StaticOverlayQuads, VisibleHighlightsKey,
};

/// Recompute decision the worker takes on a wake. Visible for
/// testing; the production loop calls [`recompute`] directly.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkerDecision {
    /// `syntax_handle` is `None` — no language attached. Worker
    /// clears the published overlay quads (so a previous language's
    /// overlay buckets don't linger after a language detach).
    Clear,
    /// Current inputs match the already-published key. Worker
    /// does nothing.
    CacheHit,
    /// Snapshot is behind the document
    /// (`snapshot.text_version() < inputs.text_version`). Worker
    /// stores the new key but preserves existing quads
    /// (stale-snapshot HOLD).
    StaleSnapshotHold,
    /// Snapshot is current; worker re-bucketed the overlay layers
    /// and stored fresh quads + key.
    Recomputed,
}

/// Worker entry point spawned at boot. Loops forever, awaiting
/// the wake `Notify`. Each wake re-reads the latest
/// `RenderState.syntax` inputs and calls [`recompute`].
///
/// Spawn from `editor_boot` once `Editor` is constructed. Pass
/// clones of `Editor::render_state` and
/// `Editor::syntax_static_overlay_quads_cell` plus the wake `Notify`.
pub async fn run(
    render_state: Arc<ArcSwap<RenderState>>,
    wake: OverlayWake,
    static_overlay_quads_cell: Arc<ArcSwap<StaticOverlayQuads>>,
    paint_request: Arc<tokio::sync::Notify>,
) {
    debug!(
        target: "lattice_host::overlay_worker",
        "overlay worker spawned"
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
        let decision = recompute(&render_state, &static_overlay_quads_cell);
        let elapsed_us = t0.elapsed().as_micros();
        tick_count += 1;
        // X1b: tell the renderer it has fresh data to paint.
        // Only on Recomputed -- CacheHit / StaleSnapshotHold leave
        // quads bit-identical / unchanged for the renderer's
        // purposes, so waking the peer would be a wasted frame.
        // `Clear` (language detach) is also a content change worth
        // a paint.
        if matches!(decision, WorkerDecision::Recomputed | WorkerDecision::Clear) {
            paint_request.notify_one();
        }
        trace!(
            target: "lattice_host::overlay_worker",
            tick = tick_count,
            ?decision,
            elapsed_us,
            "overlay worker tick"
        );
    }
}

/// Pure synchronous recompute. Reads the current published
/// `SyntaxRenderState`, decides whether to recompute, and updates
/// `static_overlay_quads_cell` accordingly. Returns the decision
/// taken so tests can assert cache-hit / stale-snapshot HOLD /
/// recompute paths without driving the async loop.
pub fn recompute(
    render_state: &ArcSwap<RenderState>,
    static_overlay_quads_cell: &ArcSwap<StaticOverlayQuads>,
) -> WorkerDecision {
    let rs = render_state.load_full();
    let syntax = &rs.syntax;
    let Some(handle) = syntax.syntax_handle.as_ref() else {
        // No language attached. Clear published quads so a
        // language detach doesn't leave stale overlay buckets.
        let existing = static_overlay_quads_cell.load();
        if existing.quads.is_empty() && existing.computed_for_key == VisibleHighlightsKey::default()
        {
            return WorkerDecision::Clear;
        }
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

    let existing = static_overlay_quads_cell.load_full();
    if existing.computed_for_key == key {
        return WorkerDecision::CacheHit;
    }

    // Stale-snapshot HOLD: the parser hasn't caught up with the
    // most recent edits yet. Compose a new StaticOverlayQuads that
    // carries the NEW key (so we don't keep retrying for the same
    // input on every wake) but PRESERVES the previously bucketed
    // quads — re-bucketing against stale source-line extents would
    // mis-place overlay backgrounds on unchanged-content lines.
    if snap_text_version < syntax.text_version {
        let existing_quads = static_overlay_quads_cell.load_full();
        let held_quads = StaticOverlayQuads {
            quads: existing_quads.quads.clone(),
            computed_for_key: key,
        };
        static_overlay_quads_cell.store(Arc::new(held_quads));
        return WorkerDecision::StaleSnapshotHold;
    }

    // Cache miss + current snapshot. Re-bucket the overlay layers
    // OFF the UI thread (this fn runs on the worker's tokio task).
    //
    // Slice X2.9: honour `end_line_override` so the fold-aware peer
    // can stretch the overlay window past closed folds. Falls back
    // to `scroll + viewport_height` (the pre-X2.9 shape) when the
    // peer didn't request a stretch. This matches the row count the
    // (now-deleted) `highlight_lines(start, end)` walk produced, so
    // the bucket length stays aligned with the renderer's visible
    // row window.
    let start = syntax.scroll;
    let default_end = syntax.scroll.saturating_add(syntax.viewport_height.max(1));
    let end = syntax
        .end_line_override
        .unwrap_or(default_end)
        .max(default_end);
    let source = snap.source();
    let row_count = visible_row_count(source, start, end);

    // Perf plan B.2 slice B.2.a: bucket the published static-overlay
    // layer payloads into per-row quad lists. Coordinates are in
    // source utf-8 byte space (the renderer applies its own
    // coordinate transform at prepaint). Empty payloads short-circuit
    // to empty buckets — keeps the steady-state no-overlay path cheap.
    // I.5.1: active-document substate is now an inner `ArcSwap`; load
    // it once and read `substitute_preview` / `all_matches` off the guard.
    let ad = rs.active_document.load();
    let substitute_storage: Vec<lattice_protocol::position::Range>;
    let substitute_matches: &[lattice_protocol::position::Range] =
        match ad.substitute_preview.as_ref() {
            Some(prev) => {
                substitute_storage = prev.matches.to_vec();
                &substitute_storage
            }
            None => &[],
        };
    let static_overlay_quads = bucket_static_overlays(
        row_count,
        start,
        source,
        &syntax.doc_highlights,
        &ad.all_matches,
        substitute_matches,
    );

    // Perf plan D.1: wrap the freshly-built `Vec` in `Arc<[T]>` at
    // the cell-store boundary so subsequent HOLD reuse paths can
    // clone the outer Arc instead of the inner Vec.
    static_overlay_quads_cell.store(Arc::new(StaticOverlayQuads {
        quads: Arc::from(static_overlay_quads.into_boxed_slice()),
        computed_for_key: key,
    }));
    WorkerDecision::Recomputed
}

/// Count the visible rows in `[start, end)` against `source`,
/// matching the row count the deleted `highlight_lines(start, end)`
/// walk produced. `end` is clamped to `total_lines + 1` (the syntax
/// snapshot's convention — a one-past-the-end empty trailing row is
/// included) so the bucket length stays aligned with the renderer's
/// visible row window even when the viewport extends past EOF.
///
/// `total_lines` is `newlines(source) + 1` (each `\n` terminates a
/// line; the bytes after the last `\n` are the final line). An empty
/// source has one line.
fn visible_row_count(source: &[u8], start: u32, end: u32) -> usize {
    if end <= start {
        return 0;
    }
    let total_lines = (memchr::memchr_iter(b'\n', source).count() as u32) + 1;
    let end = end.min(total_lines + 1);
    if start >= end {
        return 0;
    }
    (end - start) as usize
}

/// Perf plan B.2 slice B.2.a: bucket the three static-overlay layer
/// payloads (`doc_highlights`, `all_matches`, `substitute_matches`)
/// into per-row [`RowOverlayQuad`] lists.
///
/// `row_count` is the number of visible rows starting at `start`
/// (visible buffer line `start + i` for row `i`); the worker derives
/// it from the snapshot's source via [`visible_row_count`] so the
/// bucket length matches the renderer's visible window. Output
/// `quads[i]` carries every layer's ranges that intersect visible
/// line `start + i`, in fixed precedence order
/// `DocHighlight → AllMatches → Substitute`. The renderer
/// interleaves the cursor-coupled layers (`visual`, `current_match`)
/// at prepaint between `AllMatches` and `Substitute`.
///
/// Coordinates are in **source utf-8 byte space** — byte offsets
/// into the row's source line text. The walk mirrors
/// `lattice_ui_gpui::editor_element::push_range_quads` exactly so
/// renderer-side merge code can consume the bucket bit-identical to
/// its own per-frame walk.
///
/// Empty payloads on all three layers skip the bucket entirely —
/// returns an empty `Vec` and the renderer's overlay path stays on
/// the cheap "no static overlays" branch.
///
/// B4.2 refactor: this no longer takes `&[RowPrepaint]`. It only
/// ever read each row's SOURCE byte extent (which it already
/// re-derives via memchr on `source`); the styled / woven row
/// contents were never used. It now takes the visible `row_count`
/// directly and seeks the source itself, severing the dependency on
/// the deleted prepaint cache.
fn bucket_static_overlays(
    row_count: usize,
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
    let mut out: Vec<Vec<RowOverlayQuad>> = Vec::with_capacity(row_count);
    for i in 0..row_count {
        let line_idx = start + i as u32;
        let line_end = memchr::memchr(b'\n', &source[byte_off..])
            .map(|n| byte_off + n)
            .unwrap_or(source.len());
        let line_len = line_end - byte_off;
        let mut quads: Vec<RowOverlayQuad> = Vec::new();
        push_layer_quads(
            &mut quads,
            doc_highlights,
            OverlayLayer::DocHighlight,
            line_idx,
            line_len,
        );
        push_layer_quads(
            &mut quads,
            all_matches,
            OverlayLayer::AllMatches,
            line_idx,
            line_len,
        );
        push_layer_quads(
            &mut quads,
            substitute_matches,
            OverlayLayer::Substitute,
            line_idx,
            line_len,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `recompute` with `syntax_handle: None` clears overlay quads —
    /// mirrors the original `Clear` contract.
    #[test]
    fn recompute_with_no_handle_clears_quads() {
        let rs: ArcSwap<RenderState> = ArcSwap::from_pointee(RenderState::default());
        let overlay_cell: ArcSwap<StaticOverlayQuads> = ArcSwap::from_pointee(StaticOverlayQuads {
            quads: Arc::from(vec![Vec::new()].into_boxed_slice()),
            computed_for_key: VisibleHighlightsKey {
                snapshot_ptr: 0xdead,
                ..Default::default()
            },
        });
        let decision = recompute(&rs, &overlay_cell);
        assert_eq!(decision, WorkerDecision::Clear);
        let after = overlay_cell.load();
        assert!(after.quads.is_empty());
        assert_eq!(after.computed_for_key, VisibleHighlightsKey::default());
    }

    /// Helper: build a `RenderState` carrying a seeded Rust
    /// `SyntaxHandle` parsed against `text`, with the given inputs,
    /// plus the static-overlay quads cell + the active-document
    /// `all_matches` payload (for the AllMatches layer).
    fn rs_with_rust(
        text: &str,
        scroll: u32,
        viewport_height: u32,
        fold_hash: u64,
        text_version: u64,
        end_line_override: Option<u32>,
        doc_highlights: Vec<lattice_protocol::position::Range>,
        all_matches: Vec<lattice_protocol::position::Range>,
    ) -> (
        ArcSwap<RenderState>,
        Arc<lattice_syntax::SyntaxHandle>,
        Arc<arc_swap::ArcSwap<StaticOverlayQuads>>,
    ) {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .expect("rust grammar available in test build");
        s.parse_at(text, text_version);
        let handle = Arc::new(lattice_syntax::SyntaxHandle::seeded(s));
        let overlay_cell = Arc::new(arc_swap::ArcSwap::from_pointee(
            StaticOverlayQuads::default(),
        ));
        let static_overlay_version =
            crate::render_state::static_overlay_state_version(&doc_highlights, &all_matches, &[]);
        let active_document =
            arc_swap::ArcSwap::from_pointee(crate::render_state::ActiveDocumentRenderState {
                all_matches: Arc::from(all_matches.into_boxed_slice()),
                ..crate::render_state::ActiveDocumentRenderState::default()
            });
        let rs = RenderState {
            active_document: Arc::new(active_document),
            syntax: Arc::new(crate::render_state::SyntaxRenderState {
                syntax_handle: Some(handle.clone()),
                scroll,
                viewport_height,
                end_line_override,
                fold_hash,
                text_version,
                inlay_hints: Arc::from(
                    Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
                ),
                inlay_version: 0,
                static_overlay_quads: overlay_cell.clone(),
                doc_highlights: Arc::from(doc_highlights.into_boxed_slice()),
                static_overlay_version,
            }),
            ..RenderState::default()
        };
        (ArcSwap::from_pointee(rs), handle, overlay_cell)
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

    /// B4.2 GUARD TEST: the live overlay path must STILL produce
    /// quads for a search-match after the `RowPrepaint` refactor.
    /// This is the regression guard for the whole gut-and-rename:
    /// deleting the dead span/row cache must NOT kill the
    /// static-overlay bucket that both renderers paint every frame.
    #[test]
    fn recompute_buckets_search_match_quads_after_refactor() {
        // A search match ("main") on line 0 of `fn main() {}`.
        // Bytes: f=0 n=1 ' '=2 m=3 a=4 i=5 n=6 → "main" is 3..7.
        let all_matches = vec![rng(0, 3, 0, 7)];
        let (rs, _h, overlay_cell) =
            rs_with_rust("fn main() {}", 0, 5, 0, 1, None, Vec::new(), all_matches);
        let decision = recompute(&rs, &overlay_cell);
        assert_eq!(decision, WorkerDecision::Recomputed);
        let after = overlay_cell.load();
        assert!(
            !after.quads.is_empty(),
            "overlay bucket must be populated for an active search match"
        );
        // Row 0 carries exactly one AllMatches quad covering 3..7.
        let row0 = &after.quads[0];
        assert_eq!(
            row0.len(),
            1,
            "row 0 should have one match quad; got {row0:?}"
        );
        assert!(matches!(row0[0].layer, OverlayLayer::AllMatches));
        assert_eq!((row0[0].source_byte_start, row0[0].source_byte_end), (3, 7));
    }

    /// Cache-hit short-circuit: a second `recompute` with the same
    /// inputs sees `computed_for_key == key` and returns `CacheHit`
    /// without re-bucketing or churning the cell.
    #[test]
    fn recompute_with_unchanged_key_is_cache_hit() {
        let all_matches = vec![rng(0, 3, 0, 7)];
        let (rs, _h, overlay_cell) =
            rs_with_rust("fn main() {}", 0, 5, 0, 1, None, Vec::new(), all_matches);
        assert_eq!(recompute(&rs, &overlay_cell), WorkerDecision::Recomputed);
        let first_ptr = Arc::as_ptr(&overlay_cell.load_full());
        assert_eq!(recompute(&rs, &overlay_cell), WorkerDecision::CacheHit);
        let second_ptr = Arc::as_ptr(&overlay_cell.load_full());
        assert_eq!(first_ptr, second_ptr, "cache-hit must not store a new Arc");
    }

    /// Calling `recompute` twice with `None`-handle inputs takes the
    /// `Clear` path each time but only stores when needed
    /// (idempotent — no Arc churn on a redundant Clear).
    #[test]
    fn repeated_clear_is_idempotent() {
        let rs: ArcSwap<RenderState> = ArcSwap::from_pointee(RenderState::default());
        let overlay_cell: ArcSwap<StaticOverlayQuads> =
            ArcSwap::from_pointee(StaticOverlayQuads::default());
        assert_eq!(recompute(&rs, &overlay_cell), WorkerDecision::Clear);
        let first = Arc::as_ptr(&overlay_cell.load_full());
        assert_eq!(recompute(&rs, &overlay_cell), WorkerDecision::Clear);
        let second = Arc::as_ptr(&overlay_cell.load_full());
        assert_eq!(
            first, second,
            "redundant Clear must not churn the quads Arc"
        );
    }

    // ---- bucket_static_overlays unit tests (adapted from the
    //      RowPrepaint signature to the row_count signature) ----

    /// Empty payloads on all three layers short-circuit to an empty
    /// bucket — keeps the steady-state no-overlay path cheap.
    #[test]
    fn bucket_static_overlays_all_empty_returns_empty() {
        let out = bucket_static_overlays(1, 0, b"let x = 1;", &[], &[], &[]);
        assert!(out.is_empty());
    }

    /// Single-line range on each layer: the per-row bucket carries
    /// one tagged quad per layer in fixed precedence order.
    #[test]
    fn bucket_static_overlays_tags_each_layer() {
        let src = b"hello world";
        let dh = vec![rng(0, 0, 0, 5)]; // covers "hello"
        let am = vec![rng(0, 6, 0, 11)]; // covers "world"
        let sb = vec![rng(0, 0, 0, 11)]; // covers all
        let out = bucket_static_overlays(1, 0, src, &dh, &am, &sb);
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
        let src = b"a\nb\nc";
        let dh = vec![rng(1, 0, 1, 1)]; // only line 1
        let out = bucket_static_overlays(3, 0, src, &dh, &[], &[]);
        assert_eq!(out.len(), 3);
        assert!(out[0].is_empty());
        assert_eq!(out[1].len(), 1);
        assert!(out[2].is_empty());
    }

    /// Source-byte coordinates are emitted as raw source bytes
    /// regardless of any renderer-side coordinate transform.
    #[test]
    fn bucket_static_overlays_emits_source_bytes() {
        let src = b"hello";
        let dh = vec![rng(0, 0, 0, 3)]; // first 3 source bytes
        let out = bucket_static_overlays(1, 0, src, &dh, &[], &[]);
        let row = &out[0];
        assert_eq!(row.len(), 1);
        assert_eq!((row[0].source_byte_start, row[0].source_byte_end), (0, 3));
    }

    /// Multi-line range crossing the row in the middle: covers the
    /// row's full source line.
    #[test]
    fn bucket_static_overlays_multi_line_middle_row_full_line() {
        let src = b"first\nmiddle\nlast";
        let dh = vec![rng(0, 2, 2, 1)];
        let out = bucket_static_overlays(3, 0, src, &dh, &[], &[]);
        assert_eq!(out.len(), 3);
        // Row 0: from byte 2 to end → 2..5.
        assert_eq!(
            (out[0][0].source_byte_start, out[0][0].source_byte_end),
            (2, 5)
        );
        // Row 1: full line → 0..6.
        assert_eq!(
            (out[1][0].source_byte_start, out[1][0].source_byte_end),
            (0, 6)
        );
        // Row 2: from 0 to byte 1 → 0..1.
        assert_eq!(
            (out[2][0].source_byte_start, out[2][0].source_byte_end),
            (0, 1)
        );
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

    /// `visible_row_count` matches the deleted `highlight_lines`
    /// row-count semantics: `min(end, total_lines + 1) - start`.
    #[test]
    fn visible_row_count_clamps_to_source_lines() {
        // 3 lines: "a\nb\nc" → total_lines = 3.
        let src = b"a\nb\nc";
        // Viewport fully inside the file.
        assert_eq!(visible_row_count(src, 0, 3), 3);
        // Viewport extends past EOF: clamps to total_lines + 1 = 4.
        assert_eq!(visible_row_count(src, 0, 100), 4);
        // start past EOF.
        assert_eq!(visible_row_count(src, 10, 12), 0);
        // empty / inverted window.
        assert_eq!(visible_row_count(src, 5, 5), 0);
        // empty source is one line.
        assert_eq!(visible_row_count(b"", 0, 5), 2);
    }
}
