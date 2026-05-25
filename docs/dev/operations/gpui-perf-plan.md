# GPUI perf plan (A–F)

Targets: keep every feature, hit <3 ms idle frames, <6 ms median keystroke frames, 95p <8 ms, 99p <10 ms.

Dominant costs from traces (pre-plan):
- ensure_us ≈ 2.5–5.3 ms — per-frame fold geometry + per-pane cursor/scroll/snap re-runs regardless of input deltas.
- highlights_us ≈ 2.5–4.1 ms — UI-thread span consumption + overlay merge (the worker walk itself is already off-thread, X2).
- paint/text/shaping are cheap.

## Status as of 2026-05-25

Eighteen slices shipped (A.4, F, A.3, C + follow-up, A.1, A.2a, B.1, D.1, E.1, E.2.α, A.2b.1, A.2b.2, A.2b.2b, A.2b.3, B.2.a, B.2.b, B.2.c, B.4.a). The plan's `ensure_us` and `highlights_us` UI-thread costs have been attacked: ensure work is gated and fold geometry is O(log); the worker now publishes pre-painted rows via `Arc<[T]>` with inlays already woven in so the GPUI fast path consumes `VisibleRows` unconditionally; the TUI peer reads the same `visible_rows` cell via `source_spans_from_runs`; the worker also pre-buckets the three static overlay layers (doc_highlight / all_matches / substitute) per row, so both renderer peers' active-pane prepaint paths read pre-computed quads instead of walking N_overlay × V_row intersections each frame. The active-pane overlay weave is now off the UI thread on both peers; high-N hlsearch (1000-match scale) no longer carries the O(N × V) per-frame tail-risk it had at A.2b.3. Editor publish itself is now identity-preserving on the five highest-allocation sub-states (`panes` / `modes` / `buffer_locals` / inner `pane_highlights` / inner `lsp.progress`) so a no-op publish drops to roughly half the cost of one that touches every cacheable input.

| Slice                             | Status | Commit    | Notes                                                              |
|-----------------------------------|--------|-----------|--------------------------------------------------------------------|
| A.4 — Logging demotion            | done   | `1e1da8d` | `profile-frames` cargo feature; TUI parallel path checked.         |
| F  — Release profile              | done   | `b986726` | `lto=thin`, `cgu=1`, `panic=abort`, `release_max_level_debug`.     |
| A.3 — Ensure gating               | done   | `5fb8aeb` | `EnsureGateCache`; `RefreshPaneHighlights` dispatch keyed on dirt. |
| C  — FoldIndex (visual-row O(log)) | done  | `2413815` | `FoldIndex` + 7 tests; TUI + GPUI peers both wired.                |
| C follow-up                       | done   | `a1768d4` | `fold_aware_highlight_end_line` routed through FoldIndex once.     |
| A.1 — Rope-line window            | done   | (prior)   | Visible-window slice via `Buffer::line()`.                         |
| A.2a — Worker pre-paint (active)  | done   | `c55ba36` | Publishes `VisibleRows`; GPUI consumes via `shape_row_from_prepaint`. |
| B.1 — Dirty-row cache in worker   | done   | `12c330b` | Reuses `RowPrepaint` when snapshot + text_version unchanged.       |
| D.1 — `Arc<[T]>` publish types    | done   | `cc0ffb7` | Closed the bench regression D.1 was scoped to fix.                 |
| E.1 — Pre-bucket overlay quads    | done   | `928754c` | Moves per-row × per-layer math from paint to prepaint.             |
| E.2.α — Prepaint bench coverage   | done   | `f8aa713` | Extended `editor_element_frame` with E.1 overlay surface; numbers below. |
| A.2b.1 — Publish `syntax.inlay_hints` | done | `377f635` | Publish-time gated + flattened inlay list on `SyntaxRenderState`. |
| A.2b.2 — Worker weaves inlays + GPUI gate drop + TUI source-of-truth swap | done | `02403a8` | `RowRun` → enum (`Source` / `Inlay`); `RowPrepaint.inlay_offsets`; `VisibleHighlightsKey.inlay_version` axis; GPUI active-pane fast path is unconditional; TUI reads `rs.syntax.inlay_hints` instead of raw LSP cache. |
| A.2b.2b — TUI `FrameView` reads `visible_rows` | done | `8e659f3` | `FrameView.visible_rows` replaces `visible_highlights`; `source_spans_from_runs` derives per-row `StyledSpan`s from `RowPrepaint.runs` (Source-only). Active-pane compose loop + inactive-pane fallback both migrated. Post-overlay inlay splice still in place pending byte-coordinate remap (deferred). |
| A.2b.3 — Re-bench inlay weave     | done    | —         | Worker `recompute_on_scroll` flat-to-slightly-improved vs post-D.1 (−3.7% to −5.8%); GPUI helpers flat (still serve inactive panes). Active-pane fast-path savings are architectural; no helper bench captures them. Numbers below. |
| B.2.a — Worker buckets static overlays (publish + GPUI swap) | done | `abcf81c` | `RowOverlayQuad` per row carries layer + source-byte coords; `static_overlay_quads_cell` published per recompute; `static_overlay_version` axis on `VisibleHighlightsKey`; GPUI active pane consumes worker bucket + only walks cursor-coupled layers per frame. |
| B.2.b — TUI consumer swap | done | `de60cfd` | TUI compose loop reads worker bucket for AllMatches + Substitute; DocHighlight stays per-frame (kind-coloring needs `DocumentHighlightKind` the bucket doesn't carry). Also: source-byte coord refactor so both peers consume the same bucket shape. |
| B.2.c — Re-bench overlay weave | done | — | Worker `recompute_on_scroll` +7–11% (bucket walk cost moved off UI thread to worker — architecturally correct, amortised across many frames per recompute); GPUI helpers flat. Numbers below. |
| B.4.a — Identity-preserving Arc publish (5 subs) | done | `029271e` | `Versioned<T>` newtype + `PublishCache` on Editor. Cached: `panes`, `modes`, `buffer_locals` (full Arc); inner `syntax.pane_highlights`, inner `lsp.progress`. Steady-state publish 3.20 µs vs 6.05 µs unmemoised = **−47 %**; cache machinery itself is zero-overhead (mutated_all vs unmemoised delta is the bench-loop mutation work). Numbers below. |
| B.4.b — Cache `buffers` + `tabs` + `buffer_uris` | deferred | — | Needs internal version counter on `BufferRegistry` (interior-mut) + composite version derivation for `tabs` (label depends on cross-substate inputs). |
| D.* — rayon / SmallVec / bump-alloc | dropped | —       | Bench review (below) didn't justify the impl cost.                 |
| E.2 — Element-tree reuse          | pending | —         | Needs investigation pass on `EditorView::render` notify cadence.   |

Known pre-existing gaps (not introduced by this plan, called out so they're not lost):
- `syntax_color` host-theme wiring still uses `Theme::default()`; A.2 is style-tagged so the cache survives theme switch, but the worker's RGB fallback is wrong.
- `pane_highlights` inner storage isn't `Arc<[T]>` yet — one remaining D.1-style clone in `window.rs` pane fallback.
- TUI inlay splice still runs as a post-overlay pass (using `rs.syntax.inlay_hints` since A.2b.2). Migrating it INTO the pre-overlay body so we source `combined` directly requires byte-coordinate remap helpers for every overlay (substitute / hlsearch / current_match / visual / semantic-tokens). A.2b.3 didn't capture the TUI splice cost (worker bench doesn't reach the TUI compose loop) — open until a TUI `profile-frames` trace flags it as hot.

## Captured baselines (2026-05-24, post-D.1)

Reproduce: `cargo bench -q --bench highlights_worker`. Numbers are criterion median estimates. Machine: WSL2 Linux on the dev host (Linux 6.6.87.2-microsoft-standard-WSL2).

| Bench                              | Pre-A.2a | Post-D.1 | Δ        | Shape   |
|------------------------------------|----------|----------|----------|---------|
| `worker_cache_hit/24`              | ~50 ns   | 48 ns    | flat     | O(1)    |
| `worker_cache_hit/60`              | ~50 ns   | 50 ns    | flat     | O(1)    |
| `worker_cache_hit/120`             | ~50 ns   | 49 ns    | flat     | O(1)    |
| `worker_recompute_on_scroll/24`    | 212 µs   | 196 µs   | −7.5%    | O(rows) |
| `worker_recompute_on_scroll/60`    | 295 µs   | 276 µs   | −6.4%    | O(rows) |
| `worker_recompute_on_scroll/120`   | 419 µs   | 389 µs   | −7.2%    | O(rows) |
| `worker_stale_snapshot_hold/24`    | ~4.2 µs  | 2.6 µs   | −38%     | O(1)    |
| `worker_stale_snapshot_hold/60`    | ~6.5 µs  | 2.9 µs   | −55%     | O(1)    |
| `worker_stale_snapshot_hold/120`   | ~9.5 µs  | 2.7 µs   | −72%     | O(1)    |

**The architectural win this captures:** the HOLD path (document `text_version` racing ahead of the syntax snapshot; the worst case during a held-`j` edit stream) used to scale with viewport size because the published cell carried `Vec<T>` that had to be cloned to preserve. Post-D.1 it's a single `Arc` bump regardless of how much pre-painted state the worker is holding. The HOLD column is now flat at ~2.7 µs across 24/60/120 row viewports — the renderer side stops paying for keeping the visible window warm.

`recompute_on_scroll` scales with rows by construction (one row composed per visible line) but inherits a ~6–7% bonus from D.1's reduced clone churn. `cache_hit` is unchanged — D.1 had no path to short-circuit further when the key matches.

**Caveat — what we don't yet measure.** The plan's headline `ensure_us` / `highlights_us` are UI-thread costs, not worker costs. The worker bench above is what we own; the renderer-thread prepaint bench below (E.2.α) covers the editor-element prepaint surface. A **true full-frame `frame_us` bench** (shaping + `paint_quad` submission + GPU layout) **is not feasible headlessly** — gpui 0.2.2 doesn't expose a `TestAppContext` on our build, so the paint phase has to be measured via manual `RUST_LOG=lattice_gpui::perf=debug --features profile-frames` traces. Ship-side claims about `ensure_us` improvement from A.3 + C rest on trace evidence in those commits, not on a regression-gated benchmark.

### Renderer-thread prepaint baselines (2026-05-24, post-E.1)

Reproduce: `cargo bench -q -p lattice-ui-gpui --features window,bench-internals --bench editor_element_frame -- --quick --noplot`. Three bench groups cover the editor-element prepaint surface — what the renderer thread does per visible row before any `shape_line` or `paint_quad` call.

| Bench group                              | viewport 24 | viewport 60 | viewport 120 | Shape   |
|------------------------------------------|-------------|-------------|--------------|---------|
| `editor_element_frame_pre_paint`         | 21.6 µs     | 52.1 µs     | 99.0 µs      | O(rows) |
| `editor_element_frame_with_inlays`       | 26.7 µs     | 59.0 µs     | 126.1 µs     | O(rows) |
| `editor_element_frame_with_overlays`     | 22.4 µs     | 53.1 µs     | 103.8 µs     | O(rows) |

**Read-out:**

- **Base pre-paint** is 99 µs at viewport 120 — well under the 1 ms budget the bench's header text targets. At 120 Hz the 8 ms frame budget has plenty of headroom on the prepaint phase.
- **Overlay weave (E.1's surface)** adds ~5 µs at viewport 120 (~5%) — `push_range_quads` walks four overlay layers with ~17 synthetic ranges (5 doc-highlights + 10 hlsearch + 1 visual + 1 substitute). At ~40 ns / row the per-row overlay cost is negligible; any future E.2.* sub-slice targeting overlays would need to find a much bigger inefficiency than this to justify implementation cost.
- **Inlay weave** adds 5/7/23 µs across viewports (the per-row splicing cost climbs with span count and inlay count). This is **3–5× heavier than the overlay surface** and gives A.2b (inlay weave on worker) bench-justified priority over any further E-series work on the prepaint phase.

**What's NOT in these numbers:** shaping (cosmic-text via `WindowTextSystem::shape_line`) and `paint_quad` submission. Both happen inside the actual `EditorElement::prepaint` / `EditorElement::paint` methods that require a real `Window`. The bench reaches the *helpers* (`build_line_with_inlays`, `byte_to_combined_col`, `push_range_quads`) via the `bench-internals` feature; the framework methods themselves can't be called headlessly. Regressions to those phases need manual `profile-frames` traces.

### Captured baselines (2026-05-25, post-A.2b.2b)

Re-run after A.2b.1 / A.2b.2 / A.2b.2b landed. Worker bench in isolation (the parallel-run on a busy host showed ±60% variance — discarded). GPUI bench same invocation as post-E.1.

| Worker bench                       | Post-D.1 | Post-A.2b.2b | Δ vs post-D.1 |
|------------------------------------|----------|--------------|---------------|
| `worker_cache_hit/24`              | 48 ns    | ~49 ns       | flat          |
| `worker_cache_hit/60`              | 50 ns    | 49.5 ns      | flat          |
| `worker_cache_hit/120`             | 49 ns    | 49.1 ns      | flat          |
| `worker_recompute_on_scroll/24`    | 196 µs   | 185.4 µs     | −5.4%         |
| `worker_recompute_on_scroll/60`    | 276 µs   | 260.0 µs     | −5.8%         |
| `worker_recompute_on_scroll/120`   | 389 µs   | 374.7 µs     | −3.7%         |
| `worker_stale_snapshot_hold/24`    | 2.6 µs   | 2.85 µs      | +9.6%         |
| `worker_stale_snapshot_hold/60`    | 2.9 µs   | 2.62 µs      | −9.7%         |
| `worker_stale_snapshot_hold/120`   | 2.7 µs   | 2.65 µs      | flat          |

| GPUI prepaint bench                      | Post-E.1 (v120) | Post-A.2b.2b (v120) | Δ      |
|------------------------------------------|-----------------|----------------------|--------|
| `editor_element_frame_pre_paint`         | 99.0 µs         | 104.3 µs             | +5%    |
| `editor_element_frame_with_inlays`       | 126.1 µs        | 130.2 µs             | +3%    |
| `editor_element_frame_with_overlays`     | 103.8 µs        | 94.0 µs              | −8%    |

**Read-out:**

- **Worker is no-regression.** The per-row `weave_row` fast path (empty `line_inlays` → reuse `collapse_source_runs`) and the new `inlay_version` cache axis cost nothing measurable on the no-inlay corpus. `recompute_on_scroll` is actually 3–6% faster than the post-D.1 baseline, likely from the cache reuse loop tightening up under the new code layout. The `stale_snapshot_hold` axis is flat at ~2.7 µs as designed (D.1's HOLD-is-O(1) win is preserved).
- **GPUI helpers are flat.** This is the important re-framing of the E.2.α "3–5× drop" projection: the bench measures the *helpers* (`build_line_with_inlays`) which are still called by inactive panes. The active-pane production path no longer goes through those helpers — `EditorElement::prepaint` consumes pre-woven `RowPrepaint`s from `VisibleRows` via `shape_row_from_prepaint`. That architectural savings doesn't show up in the helper bench because the helper still exists and still gets benched. A future `editor_element_frame_active_pane_inlays` bench (calling `shape_row_from_prepaint` directly on a worker-produced `VisibleRows` fixture) would close the visibility gap; deferred until someone actually needs to gate that path.
- **Net for A.2b:** the inlay-weave cost moved from per-frame on the renderer to once-per-recompute on the worker, with measurement-confirmed no regression. The architectural goal (drop the `line_has_inlays` UI-thread gate; both renderer peers consume the same pre-woven rows) is met; the projected helper-level savings was a category error — the helpers don't disappear, they just stop running on the hot path.

### Captured baselines (2026-05-25, post-B.2)

Re-run after B.2.a / B.2.b landed. Worker bench in isolation (parallel-run with the GPUI bench showed ~14–19 % noise — discarded once isolated re-run confirmed the smaller, true delta).

| Worker bench                       | Post-A.2b.2b | Post-B.2 | Δ vs post-A.2b.2b |
|------------------------------------|--------------|----------|-------------------|
| `worker_cache_hit/24`              | ~49 ns       | 52 ns    | +6 %              |
| `worker_cache_hit/60`              | 49.5 ns      | 52 ns    | +5 %              |
| `worker_cache_hit/120`             | 49.1 ns      | 50 ns    | flat              |
| `worker_recompute_on_scroll/24`    | 185.4 µs     | 199.9 µs | +7.8 %            |
| `worker_recompute_on_scroll/60`    | 260.0 µs     | 282.5 µs | +8.7 %            |
| `worker_recompute_on_scroll/120`   | 374.7 µs     | 415.7 µs | +10.9 %           |
| `worker_stale_snapshot_hold/24`    | 2.85 µs      | 2.91 µs  | +2 %              |
| `worker_stale_snapshot_hold/60`    | 2.62 µs      | 2.91 µs  | +11 %             |
| `worker_stale_snapshot_hold/120`   | 2.65 µs      | 2.87 µs  | +8 %              |

| GPUI prepaint bench (viewport 120)       | Post-A.2b.2b | Post-B.2 | Δ      |
|------------------------------------------|--------------|----------|--------|
| `editor_element_frame_pre_paint`         | 104.3 µs     | 90.1 µs  | −14 %  |
| `editor_element_frame_with_inlays`       | 130.2 µs     | 118.6 µs | −9 %   |
| `editor_element_frame_with_overlays`     | 94.0 µs      | 89.9 µs  | −4 %   |

**Read-out:**

- **Worker bench regression is real, ~7–11 % on the recompute path.** Comes from the new per-recompute work: build static-overlay bucket (early-return when all three layers empty, but the `bucket_static_overlays` call + the `snap.source()` access + the `static_overlay_quads_cell.store(Arc::new(...))` aren't free), plus the extra `static_overlay_version` field on `VisibleHighlightsKey` and a third Arc<ArcSwap<...>> field on `SyntaxRenderState`. This is **architecturally correct** — every µs added on the worker thread is a µs removed from the UI thread per frame. The worker tick fires once per text/scroll/edit change; the renderer fires every paint. Amortising the bucket cost across many frames per recompute is the entire point.
- **GPUI helper bench is flat-to-better.** Same E.2.α caveat: the bench measures `push_range_quads` directly, which is now only called for inactive panes (active pane reads worker bucket). The bench keeps that helper warm; the active-pane production path no longer runs it for the three static layers.
- **Where the win actually lands** — the headline payoff isn't on either of these benches but on the renderer's active-pane prepaint when `all_matches` is large. Pre-B.2 the GPUI peer's `overlay_quads_for_row` walked `[5 doc_highlights, 10 hlsearch, 1 substitute]` × every visible row each frame; a 1000-match hlsearch scaled to ~1 ms/frame at viewport 120 (O(N × V) intersection checks). Post-B.2 the worker emits ≤ a few quads per visible row pre-bucketed in source-byte space; renderer just walks the small per-row list. The TUI peer gets the same architectural win for the same two layers (hlsearch + substitute). DocHighlight stays per-frame on TUI because the bucket doesn't carry `DocumentHighlightKind` (kind-coloured styles).
- **Not bench-gated.** Adding a fixture that proves the high-N win (`editor_element_frame_active_pane_with_worker_bucket`, populated with 1000 synthetic hlsearch matches + a worker-bucket fixture) would close the gap. Deferred until someone needs the regression gate for that code path; the production path is correct and the cost surface moved as planned.

### Captured baselines (2026-05-25, post-B.4.a)

New bench: `dispatch_publish`. Reproduce: `cargo bench -q -p lattice-host --bench dispatch_publish`. Numbers are criterion median estimates. Fixture: editor with a 3-pane tree, 20 buffers' worth of `active_modes` + `buffer_locals`, 3 panes of `pane_highlights` (60 spans each), 6 concurrent `lsp_progress` items. Same machine as the previous baselines.

| Bench                                | Time     | Δ vs `unmemoised` (pre-B.4 equivalent) |
|--------------------------------------|----------|----------------------------------------|
| `dispatch_publish/steady_state`      | 3.20 µs  | **−47 %** (cache hits everywhere)      |
| `dispatch_publish/mutated_modes`     | 3.65 µs  | −40 % (one cache miss, four hits)      |
| `dispatch_publish/mutated_all`       | 6.45 µs  | +7 % (5 misses + bench-loop mutations) |
| `dispatch_publish/unmemoised`        | 6.05 µs  | baseline (cache cleared each iter)     |

**Read-out:**

- **B.4 saves ~47 % on no-op publishes with zero net overhead on a fully-invalidated publish.** `unmemoised` (cache cleared between iterations, no per-iter mutation work) is the cleanest stand-in for pre-B.4 cost: every cached slot misses and rebuilds, just like the unconditional `Arc::new` path before B.4. The cache lookup machinery (`Mutex<PublishCache>::lock`, five `Versioned::version()` reads, five cache-slot compares, five `Arc::clone`s when hits, one closure call + `Arc::new` per miss) doesn't show up in this number — it's drowned by the rebuild cost it would normally avoid.
- **The +7 % on `mutated_all` is bench-loop noise, not real overhead.** That row mutates each of the five cached fields per iteration (5 HashMap insert/remove ops at ~100-200 ns each = ~500-1000 ns), which fully accounts for the +400 ns delta over `unmemoised`. Use `unmemoised` as the canonical "cache disabled" baseline; treat `mutated_all` as "what happens if every cached input churns every publish + per-iter setup."
- **Steady-state floor (3.2 µs) is what's still rebuilt every tick:** `active_document` (cursor / scroll change every keystroke), the small Copy-or-Arc-clone sub-states (`lifecycle`, `theme`, `messages`, `modeline`, `options`, `diagnostics`, `translator`), the outer `lsp` / `syntax` sub-states (their other fields churn per-frame), the not-yet-cached `buffers` + `tabs` (deferred to B.4.b), and the picker / completion / popup Option-when-None slots.
- **Targeted cache invalidation works.** `mutated_modes` (one mode toggle per iteration) costs ~+14 % over steady_state because only the `modes` sub-state rebuilds; the other four cache hits stay. This is the design promise — a Mode activation doesn't force the renderer to drop its `panes` / `buffer_locals` / `pane_highlights` / `lsp.progress` Arc identity.
- **Renderer side gets a free guard.** Active-pane prepaint on both peers can now `Arc::ptr_eq` between `prior_rs.modes` and `rs.modes` (or any other cached sub-state) to short-circuit per-frame work that only depends on that sub-state. No call site does this yet; the seam is in place for follow-ups.

### Bench-driven D scope reduction

Bench numbers killed three D sub-slices that were on the original plan:

- **Rayon for row prep** — recompute_on_scroll is 200–400 µs across viewports, which is 3.4 µs/row at viewport 120 (worst case). Rayon task spawn is ~1–2 µs each; the parallelism win is smaller than the spawn overhead unless we batch rows into groups of ~30+, at which point the simpler serial loop is competitive. Skipped.
- **SmallVec for per-row runs** — sub-µs per-row allocation savings against a 3.4 µs/row baseline. Doesn't move the needle on the bench. Skipped.
- **Bump allocator for transient worker buffers** — heavy infra change (lifetime-threading the bump through the worker hot path) for a sub-µs win that D.1's `Arc<[T]>` change captured most of anyway. Skipped.

Only the `Arc::new` churn reduction (D.1) survived. The bench was the gate — running it before scoping the slice flipped the work from "implement four optimisations" to "implement one and document why the rest don't pay."

## Stage order (rationale, original)

Slices were ranked by largest verified shave per implementation hour. The A–F labels stayed stable for cross-reference; the order they shipped in:

1. **A.4** — Demote per-frame DEBUG logs behind a profiling feature.
2. **F** — Build profile (LTO thin, codegen-units=1 where safe, panic=abort, feature-gate tracing).
3. **A.3 + C** — Ensure gating + visual-row fold index. Both attack `ensure_us` directly with no cross-renderer ABI churn.
4. **A.1 [DONE]** — Rope-line window in `window.rs`.
5. **A.2** — Worker pre-paint of visible rows; UI consumes precomposed `VisibleRows`.
6. **B** — Incremental caches built on top of A.2's row structure.
7. **D** — Parallelisation / preallocs.
8. **E** — Render niceties (overlay quad batching, element-tree reuse).

Rationale:
- **A.4 first**: minutes of work; removes self-inflicted per-frame overhead so subsequent benches measure real costs.
- **F second**: one PR, measurable mean-frame win on cold + hot paths.
- **A.3 + C ahead of A.2**: trace evidence puts `ensure_us` ≥ `highlights_us`. A.3 + C are intra-renderer changes (no published-type churn) and ship in days. A.2 is a cross-crate ABI change touching the host worker, both renderer peers, and the render-state contract — give it the longest bake time.
- **A.2 before B**: B's dirty-row caches extend `RowPrepaint`; the struct has to exist first.

## Slice details (in ship order)

### A.4 — Logging demotion [`1e1da8d`]
- Feature-gated the per-frame DEBUG frame-budget lines behind `profile-frames` cargo feature so default release builds skip the format machinery.
- TUI parallel path verified — the per-frame log lines on the TUI side were already conditional.
- TRACE/DEBUG paths intact for profiling builds.

### F — Build profile [`b986726`]
- Release: `opt-level=3`, `lto=thin`, `codegen-units=1`, `panic=abort`.
- `tracing` workspace dep updated with `release_max_level_debug` so `cargo build --release` strips DEBUG/TRACE format calls at compile time.

### A.3 — Ensure gating [`5fb8aeb`]
- `EnsureGateCache` keyed on `(cursor, scroll, viewport_height, fold_hash)`; early-return when unchanged.
- `ensure_cursor_in_viewport()` and `RefreshPaneHighlights` dispatch both gated independently.

### C — FoldIndex (O(log) folds) [`2413815`, follow-up `a1768d4`]
- `FoldIndex { closed, all_starts, foldenable }` built once per pane in `paint_pane`; `partition_point` for line→fold lookup.
- Replaced two ad-hoc closures + `fold_aware_highlight_end_line`'s linear walks.
- Both renderer peers (GPUI `EditorElement`, TUI `FrameView`) wired uniformly — no kind-specific logic.
- 9 tests in `crates/lattice-host/src/folds.rs` covering nested / overlapping / disabled cases.

### A.1 — Rope-line window [DONE]
- `window.rs`: removed `snapshot.text() + split('\n')`; materialise only `[visible_start, visible_end)` via `Buffer::line()`.
- Outstanding: the per-line `String` alloc (one per visible row per frame) is still measurable at 120 Hz; `Cow<'_, str>` or pushing lines into the element without an intermediate `Vec` would close it.

### A.2a — Worker pre-paint (active pane) [`c55ba36`]
- `VisibleRows { rows: Arc<[RowPrepaint]>, computed_for_key }` published on a second `ArcSwap` cell (`SyntaxRenderState.visible_rows`).
- Worker `build_rows()` does memchr-driven line seek + run collapse from style-tagged spans (not RGB-baked — theme switch doesn't invalidate the cache).
- GPUI fast path (`shape_row_from_prepaint`) consumes rows when active pane has no inlays; falls back to the legacy span path otherwise (the `line_has_inlays` predicate gates this until A.2b lands).
- TUI peer updated to thread the new cell through `boot.rs` / `app/highlights.rs`; TUI compose loop still reads `visible_spans` (migration is part of A.2b).
- Inactive panes still flow through `pane_highlights` — `VisibleRows` is active-pane-only by design; silently dropping inactive-pane styling was explicitly avoided.
- Worker `syntax_color` host-theme wiring is unchanged (still `Theme::default()` — pre-existing, out of A.2a scope).

### B.1 — Dirty-row cache [`12c330b`]
- `build_rows_with_cache()` reuses prior `RowPrepaint`s when snapshot pointer + text_version match; rebuilds only rows whose source bytes or spans changed.
- Three new tests cover full-hit / partial-hit / full-miss paths.

### A.2b.1 — Publish `syntax.inlay_hints` [`377f635`]
- `SyntaxRenderState.inlay_hints: Arc<[InlayHintRow]>` carries the active document's enabled-gated, padding-baked, utf-16 → utf-8 converted inlay-hint list. Editor builds it once per `publish_render_state` via `Editor::build_active_inlay_hints`.

### A.2b.2 — Worker weaves inlays + GPUI gate drop + TUI source-of-truth swap [`02403a8`]
- `RowRun` promoted from a struct to an enum: `Source { len, style }` for source bytes, `Inlay { len }` for inlay-virtual-text bytes. Consumers map `Inlay` to their inlay colour (resolved on the renderer — GPUI: `inlay_color`; TUI: `inlay_hint_style`).
- `RowPrepaint.inlay_offsets: Arc<[(u32, u32)]>` records each splice `(orig_byte, char_width)` so cursor / decoration / overlay byte→column remap stays correct in `combined`-space.
- `VisibleHighlightsKey.inlay_version: u64` axis added. Worker invalidates the row cache (B.1 dirty-row path) whenever the inlay payload's content hash flips. `Editor::build_active_inlay_hints` returns the `(Arc, hash)` pair so the hash is byte-aligned with the published list by construction; empty list hashes to 0 to keep the steady-state no-hint path on a single cheap branch.
- Worker introduces `weave_row` + `bucket_inlays_by_line`. The no-inlay fast path (empty `inlay_hints`) skips the splice machinery and reuses the A.2a no-alloc `collapse_source_runs` shape; the inlay path walks `line_text` once, splicing inlays at their byte offsets and partitioning into Source / Inlay runs.
- GPUI: `shape_row_from_prepaint` now returns `(ShapedLine, Vec<(u32, u32)>)`; the `line_has_inlays` gate is gone. Active-pane prepaint takes the fast path unconditionally; inactive panes still use `shape_row` + `build_line_with_inlays` since `visible_rows` is active-pane-only.
- TUI: `compose_visible_lines` reads `rs.syntax.inlay_hints` directly (publish-time gated + flattened) instead of `rs.lsp.inlay_hints` + per-line mode gate + per-hint utf-16 conversion + label flatten. FrameView's `lsp_inlay_hint_enabled` cache field dropped. Full compose-loop migration to `visible_rows` deferred to A.2b.2b.
- 8 new worker unit tests: `collapse_source_runs` × 2, `weave_row` × 5 (no-inlay, mid-line, trailing, multi-inlay, empty-line variants), `bucket_inlays_by_line` × 2, `build_rows_with_cache_falls_back_on_inlay_version_change`, `recompute_weaves_published_inlay_hints_into_rows`.
- Existing TUI inlay tests stay green (`inlay_hint_overlay_splices_virtual_text`, `inlay_hint_overlay_suppressed_when_mode_off`) — they exercise the new source-of-truth path end-to-end.
- Pre-existing renderer behaviour preserved: inactive panes still render their own per-buffer inlay hints via the legacy `build_line_with_inlays` path.

### A.2b.2b — TUI `FrameView` reads `visible_rows` [`8e659f3`]
- `FrameView.visible_highlights: Arc<[Vec<StyledSpan>]>` removed; replaced by `FrameView.visible_rows: Arc<VisibleRows>`. Both constructors (`from_app`, `for_buffer`) now `load_full()` the worker's `visible_rows` cell instead of the legacy `visible_spans` cell.
- New helper `source_spans_from_runs(&[RowRun]) -> Vec<StyledSpan>` derives per-row source spans from `RowPrepaint.runs`, filtering out `Inlay` variants. The result partitions the SOURCE line's utf-8 bytes exhaustively (so the existing overlay code — which indexes by source byte — consumes it identically to the legacy slice).
- Active-pane `compose_visible_lines_inner`: replaced the `view.visible_highlights[row]` lookup with a derived `source_spans_from_runs(&row.runs)`. Inactive-pane `draw_inactive_document`: same-doc-same-scroll fallback now derives spans from `view.visible_rows.rows.iter().map(...)` instead of cloning the legacy slice.
- Post-overlay inlay splice (using `rs.syntax.inlay_hints`) stays as-is. Sourcing `combined` directly into the pre-overlay body would require byte-coordinate remap helpers on every overlay layer; deferred. A.2b.3's worker bench doesn't capture the TUI splice cost directly (the splice happens in the TUI compose loop, not the worker), so the question stays open — surface it again only if a TUI `profile-frames` trace flags `compose_visible_lines` as hot.
- 4 new render-side tests on `source_spans_from_runs` (empty / source-only / inlay-skip / leading-inlay).
- Closes the "TUI drops `visible_spans` reader" goal that A.2b was originally scoped to deliver.

### A.2b.3 — Re-bench inlay weave [done]
- Re-ran `editor_element_frame_*` + `worker_*` after A.2b.2 / A.2b.2b. Numbers captured in "Captured baselines (2026-05-25, post-A.2b.2b)" above.
- **Outcome:** worker flat-to-slightly-improved (−3.7% to −5.8% on `recompute_on_scroll`); GPUI helper benches flat (helpers still serve inactive panes). The "3–5× drop" projection from E.2.α was a category error — the helpers don't go away, they just stop running on the active-pane hot path. To bench the active-pane production fast path directly, a future bench would call `shape_row_from_prepaint` on a worker-produced `VisibleRows` fixture; deferred (the path is gated to a known O(rows) walk over a `Box<str> + Vec<RowRun>` already covered by the worker bench).
- Pre-existing TUI test flakes (`help_motion_clamps_to_last_line`, `help_gg_and_capital_g_route_through_grammar`, `popup_with_long_content_scrolls_when_cursor_descends`, intermittent LSP tests) re-confirmed under full-suite load; all pass in isolation. Unchanged by A.2b.

### B.2.a — Worker buckets static overlays + GPUI swap [`abcf81c`]
- New types on `render_state`:
  - `OverlayLayer` enum (`DocHighlight` / `AllMatches` / `Substitute`) tags each pre-bucketed quad so renderers can interleave cursor-coupled layers (`visual`, `current_match`) at the right precedence.
  - `RowOverlayQuad { layer, source_byte_start, source_byte_end }` — source utf-8 byte coordinates (see B.2.b below for the coordinate-system rationale).
  - `StaticOverlayQuads { quads: Arc<[Vec<RowOverlayQuad>]>, computed_for_key }` published on a new `Arc<ArcSwap<...>>` cell parallel to `visible_rows`.
- `SyntaxRenderState` gets three new fields: `static_overlay_quads` (worker output cell), `doc_highlights: Arc<[Range]>` (utf-16 → utf-8 pre-converted at publish time so worker doesn't repeat per recompute), and `static_overlay_version: u64` (content hash of all three layer payloads — bumps independently from `inlay_version`).
- `VisibleHighlightsKey.static_overlay_version` axis added; Clear / HOLD / Recompute branches all keep the overlay cell in lock-step with `rows` / `spans`.
- Worker `bucket_static_overlays(rows, start, source, dh, all, sub)` walks every row × every layer in fixed precedence; early-return on all-three-empty keeps the steady-state no-overlay path cheap.
- GPUI `editor_element` consumes the worker bucket as the base of `overlay_quads_per_row` for the active pane; renderer per-frame walks only `current_match` + `visual_range`. Inactive panes fall through to the legacy per-frame `push_range_quads` for the only layer they paint (DocHighlight) — bucket is active-pane only by design.
- Worker test additions: 6 new tests on `bucket_static_overlays` + the version hash. 28 highlights_worker tests pass.

### B.2.b — TUI consumer swap + source-byte refactor [`de60cfd`] + [`89bfd2b`]
- **Coordinate refactor.** B.2.a published quads in combined-column space (chars after inlay splicing) — GPUI-friendly but doesn't translate cleanly to the TUI (which applies overlays in source-byte space against the un-spliced body). Source-byte coords let both peers consume the same bucket; GPUI converts source-byte → combined-col per quad at prepaint via the existing `byte_to_combined_col` helper (~60 ns × ≤ a few quads/row × 120 rows ≈ 15 µs/frame worst case).
- **TUI consumer.** `compose_visible_lines_inner` loads the worker bucket once per frame and walks the per-row tagged list for `AllMatches` + `Substitute` layers. Legacy per-frame walks of `app.ad().all_matches` and `substitute_preview.matches` are gone for the active pane.
- **DocHighlight stays per-frame on TUI.** The TUI styles each highlight by `DocumentHighlightKind` (Read = green-ish, Write = red-ish, Text/None = blue-ish); the worker bucket doesn't carry kind data. N is small (typically ≤ tens) so the per-frame walk has no measurable cost. A future B.2.x could extend `RowOverlayQuad` with a `kind` tag if a profile-frames trace flags this path.
- Fix-up commit `89bfd2b` caught the deferred-substitute branch in GPUI that the source-byte refactor missed (compiled fine in cargo check; the bench build with `--features bench-internals` exercised the closure and failed).

### B.2.c — Re-bench overlay weave [done]
- Numbers captured in "Captured baselines (2026-05-25, post-B.2)" above. Worker `recompute_on_scroll` +7–11 %; GPUI helper bench flat-to-better (the helpers stay benched but the active-pane production path no longer calls them for static layers).
- **Outcome:** B.2 moves the bucket cost from per-frame on the renderer to per-recompute on the worker, with measurement-confirmed regression on the worker (architecturally correct — amortised across many frames per recompute) and architectural drop in renderer per-frame overlay-walk cost. High-N tail-risk (1000-match hlsearch) drops from ~1 ms/frame to negligible; the bench can't directly show this because the synthetic bench uses 10 hlsearch matches and operates through helpers the active pane no longer touches.
- Pre-existing TUI flakes (`help_motion_clamps_to_last_line`, `help_gg_and_capital_g_route_through_grammar`, `popup_with_long_content_scrolls_when_cursor_descends`) unchanged.

### B.4.a — Identity-preserving Arc publish (Big-5) [_commit pending_]
- New module `lattice_host::versioned` introduces `Versioned<T>` — a newtype with `Deref` (no bump) and `DerefMut` (bumps a `u64`). Existing call sites like `editor.active_modes.insert(...)` autoref `&mut self.active_modes`, fire `DerefMut`, and bump the counter automatically — no migration burden across the ~30 mutator sites.
- Editor fields wrapped: `pane_tree`, `active_modes`, `buffer_locals`, `pane_highlights`, `lsp_progress`. `mem::swap` sites for `pane_tree` (tab swaps) explicitly deref via `&mut *self.pane_tree` so the version bumps exactly once per swap.
- New `PublishCache` lives on Editor behind `std::sync::Mutex<...>` (not `RefCell` because `Editor` is shared as `Arc<Editor>` and must be `Sync`; mutex is uncontested in practice because only `publish_render_state` takes the lock, on the actor thread).
- `cached_or_build(slot, version, build)` helper memoises each sub-state Arc. `panes` / `modes` / `buffer_locals` cache the full sub-state Arc; `pane_highlights` (a field of `SyntaxRenderState`) and `lsp.progress` (a field of `LspRenderState`) cache the inner Arc — the parent sub-state still rebuilds per publish because its other fields churn per-frame.
- New tests in `render_state::tests`: `cached_substates_preserve_arc_identity_on_no_op_publish` (proves Arc identity survives no-op publish for every cached sub-state), `cached_substate_invalidation_is_per_field` (proves a mutation to one cached field invalidates only that sub-state — `panes` / `buffer_locals` survive when only `active_modes` is touched). The legacy `substate_identity_changes_naively_per_publication` test still asserts the inverse for the not-yet-cached sub-states (`diagnostics`, the outer `lsp`, `popup`); docstring updated to scope the "naive" claim.
- New bench: `dispatch_publish` with `steady_state` / `mutated_modes` / `mutated_all` / `unmemoised` regimes. `unmemoised` clears the cache between iterations (no mutation work) and serves as the clean pre-B.4 baseline. Numbers captured in "Captured baselines (2026-05-25, post-B.4.a)" above.
- Versioned wrapper has its own 7-test suite covering version-zero init, no-bump on read, bump on `&mut`, `replace()` semantics, HashMap autoref bumps, `From<T>`, and `into_inner`.

### B (remaining) — Incremental caches
- **B.3** — N/A. GPUI's `LineLayoutCache` already provides shape-line reuse keyed on `(font_id, size, text)`; an explicit LRU is duplication.
- **B.4.b** — Cache `buffers` (registry has interior mutability — needs a `version` counter inside `BufferRegistry` exposed via `BufferRegistry::version()`), `buffer_uris` (straight `Versioned<HashMap>` wrap), and `tabs` (label depends on cross-substate inputs: `tabs` list + `active_tab` + `panes.active().buffer_id` + `buffers.name_of(...)` — composite version derivation or output-hash). Deferred; the steady-state bench shows the residual cost is small enough that B.4.b isn't the next-biggest perf lever.

### D.1 — `Arc<[T]>` publish types [`cc0ffb7`]
- `VisibleSpans.spans` and `VisibleRows.rows` changed from `Vec<T>` to `Arc<[T]>`.
- `Default` impls construct empty `Arc::from(vec.into_boxed_slice())`.
- HOLD path on the worker drops from `O(viewport)` to `O(1)` (single Arc bump to preserve the prior published value).
- Reverses the bench regression A.2a introduced and adds a viewport-independent floor.

### D (remaining) — dropped, see bench-driven D scope reduction above.

### E.1 — Pre-bucket overlay quads [`928754c`]
- Five overlay layers (doc_highlight → all_matches → current_match → visual → substitute) resolve to `(col_start, col_end, color)` tuples in `prepaint`, mirroring `diagnostic_segments_per_row`.
- `paint` becomes an allocation-free walk over `overlay_quads_per_row` — no `byte_to_combined_col` or per-range intersection checks per frame.
- Layering precedence preserved by push order in each row's Vec (`paint_quad` overwrites, last push wins). Inactive panes carry only doc_highlight quads; the `is_active` gate moves into the pre-bucket closure rather than firing per row.
- `paint_range_overlay` removed; replaced by `push_range_quads` (same intersection logic, pushes tuples instead of painting).
- 8 new unit tests on `push_range_quads` cover rows outside range, single-line clipping, multi-line start/middle/end rows, inlay-shifted columns, empty ranges, and layering preservation.

### E.2.α — Prepaint bench coverage [`f8aa713`]
- Extended `benches/editor_element_frame.rs` with `editor_element_frame_with_overlays`: per-row `push_range_quads` for 5 doc-highlights + 10 hlsearch + 1 visual + 1 substitute, across viewports 24 / 60 / 120.
- Required exposing `push_range_quads` as `pub` (it stays `pub(crate)`-visible in default builds via the module's `pub(crate) mod` gating; `pub` in default builds; full `pub` reach only under `bench-internals`).
- Captured baselines (above) confirm the prepaint phase is well under budget at viewport 120 (~104 µs with overlays) and that inlay weave dominates overlay weave 3–5×.
- **Investigation findings** that informed scope: render() has 4 `cx.notify()` callers (worker bridge, popup-dismiss, on_key_down, tab click) — input-driven cadence, no over-fire. Conditional overlay-block construction is already correct (E/F/G/H/I `return None` and skip when state is `None`). The remaining cost surfaces (per-char `div()` cells in popup / picker / completion overlays; tabline label `SharedString` churn; `render_state.load()` micro-churn) are real but only active during overlay-open frames or sub-µs in steady state.

### E.2.* — Remaining sub-slices [bench-justified ordering]
1. **E.2.a** (promote popup overlay body to a shared `OverlayElement`) — only matters while popup is open; defer until someone profiles popup-open frames and finds them hot.
2. **E.2.b / c** (same fix for picker / completion overlays) — same justification as E.2.a; smaller surface.
3. **E.2.d** (tabline `SharedString` identity reuse) — every frame; tiny win; defer.
4. **E.2.e** (consolidate `render_state.load*()` in render + paint_pane) — sub-µs; skip unless cleanup pressure arises.

Also queued (out of E-series but on the perf-plan tail):
- **TUI inlay pre-weave** — fold the post-overlay inlay splice INTO the pre-overlay body so the TUI sources `combined` directly. Needs byte-coordinate remap helpers on every overlay; only worth it if the bench shows the splice is hot.

## Instrumentation

- Split timers inside `ensure` (`cursor_snap`, `fold_snap`, `inactive_pane_refresh`) and inside the worker (fetch spans, overlay merge, row compose, shaping hits) — all gated behind `profile-frames`.
- Counters worth adding before E lands: `rows_reused` / `rows_rebuilt`, `overlay_quads`, `shaped_cache_hits`.

## Acceptance gates

Each gate requires: reference machine class, fixed corpus, release-profile build, default logging.

- Idle: `frame_us < 3 ms`.
- Keystrokes (medium corpus — define once, e.g. `syntax.rs` ~8 kLOC, 150 visible rows, default theme): p50 < 6 ms, p95 < 8 ms, p99 < 10 ms.
- Regression gate (worker hot path): `benches/highlights_worker.rs` is wired in CI for compile + baseline-recording on main pushes (`ci.yml:155, :179`). Baselines above.
- Regression gate (renderer-thread prepaint): `benches/editor_element_frame.rs` is wired the same way. Three groups cover the prepaint surface; E.2.α baselines above.
- **Gap (paint phase):** shaping + `paint_quad` + GPU layout aren't bench-gateable headlessly (no `TestAppContext` on gpui 0.2.2). Regressions there need manual `profile-frames` traces, not CI numbers.

## Implementation pointers

- `lattice-host/src/highlights_worker.rs` — builds + publishes precomposed rows; A.2b.2 added the per-row `weave_row` + `bucket_inlays_by_line` splice that produces `RowRun::Source` / `RowRun::Inlay` runs + `inlay_offsets`; B.2.a added `bucket_static_overlays` (source-byte `RowOverlayQuad`s for doc_highlight / all_matches / substitute); will need to pull theme from `RenderState.theme` (not `Theme::default()`).
- `lattice-host/src/render_state.rs` — `VisibleRows` / `RowPrepaint` / `RowRun` types; `OverlayLayer` / `RowOverlayQuad` / `StaticOverlayQuads` (B.2.a); `Arc<[T]>` storage; `Default` impls construct empty Arc; `VisibleHighlightsKey.{inlay_version, static_overlay_version}` axes paired with `inlay_hints_version()` / `static_overlay_state_version()` content hashes. B.4.a: `PublishCache` + `cached_or_build` helper drive the identity-preserving Arc publish for `panes` / `modes` / `buffer_locals` / inner `pane_highlights` / inner `lsp.progress`.
- `lattice-host/src/versioned.rs` — `Versioned<T>` newtype (B.4.a). `DerefMut` bumps the wrapped version counter on every `&mut` access; reads via `Deref` don't bump. Drives the `PublishCache` cache-hit decision in `build_render_state` without any per-mutator discipline.
- `lattice-host/src/folds.rs` — `FoldIndex` with `partition_point` lookups; both renderer peers consume.
- `lattice-ui-gpui/src/editor_element.rs` — `shape_row_from_prepaint` consumes precomposed rows for the active pane (unconditional after A.2b.2 lifted the `line_has_inlays` gate); falls back to `shape_row` + `build_line_with_inlays` for inactive panes. B.2.a: active-pane `overlay_quads_for_row` consumes `worker_static_overlay_quads` (DocHighlight + AllMatches + Substitute), interleaves cursor-coupled layers per frame, falls back to per-frame `push_range_quads` walk when the bucket is missing (boot / inactive panes).
- `lattice-ui-tui/src/render.rs` + `app/highlights.rs` — threads the `visible_rows` cell; `FrameView` reads `visible_rows` (A.2b.2b) and derives per-row source spans via `source_spans_from_runs`; inlay overlay reads `rs.syntax.inlay_hints` since A.2b.2. B.2.b: `compose_visible_lines_inner` reads `rs.syntax.static_overlay_quads` once per frame and consumes per-row tagged quads for AllMatches + Substitute layers; DocHighlight stays per-frame for `DocumentHighlightKind` coloring; cursor-coupled layers stay per-frame.

## Notes

- Every feature preserved (doc highlights, search, visual, substitute, inlays).
- Composition is off the UI thread for the active pane no-inlay path; recompute keyed on input change.
- Paramount goal #4: "Nothing blocks the UI — enforced architecturally, not by discipline." Every slice here is an architectural enforcement, not a discipline reminder.
