# GPUI perf plan (A–F)

Targets: keep every feature, hit <3 ms idle frames, <6 ms median keystroke frames, 95p <8 ms, 99p <10 ms.

Dominant costs from traces (pre-plan):
- ensure_us ≈ 2.5–5.3 ms — per-frame fold geometry + per-pane cursor/scroll/snap re-runs regardless of input deltas.
- highlights_us ≈ 2.5–4.1 ms — UI-thread span consumption + overlay merge (the worker walk itself is already off-thread, X2).
- paint/text/shaping are cheap.

## Status as of 2026-05-24

Nine slices shipped (A.4, F, A.3, C + follow-up, A.1, A.2a, B.1, D.1, E.1). The plan's `ensure_us` and `highlights_us` UI-thread costs have been attacked: ensure work is gated and fold geometry is O(log); the worker now publishes pre-painted rows via `Arc<[T]>` so the renderer's HOLD path is O(1) regardless of viewport size; and overlay quads are pre-bucketed in prepaint so paint is an allocation-free walk.

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
| A.2b — Inlay weave on worker      | deferred | —       | Would lift the `line_has_inlays` fast-path restriction.            |
| B.2 — Overlay precompute on worker | deferred | —      | Blocked behind A.2b.                                               |
| B.4 — Identity-preserving sub-state Arc publish | deferred | — | Lower priority; bench impact unmeasured.                |
| D.* — rayon / SmallVec / bump-alloc | dropped | —       | Bench review (below) didn't justify the impl cost.                 |
| E.2 — Element-tree reuse          | pending | —         | Needs investigation pass on `EditorView::render` notify cadence.   |

Known pre-existing gaps (not introduced by this plan, called out so they're not lost):
- TUI compose loop still reads `visible_spans` — should migrate to `visible_rows` once A.2b lands.
- `syntax_color` host-theme wiring still uses `Theme::default()`; A.2 is style-tagged so the cache survives theme switch, but the worker's RGB fallback is wrong.
- `pane_highlights` inner storage isn't `Arc<[T]>` yet — one remaining D.1-style clone in `window.rs` pane fallback.

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

**Caveat — what we don't yet measure.** The plan's headline `ensure_us` / `highlights_us` are UI-thread costs, not worker costs. The worker bench above is what we own; a GPUI `frame_us` bench is the gap called out in the acceptance gates and not closed yet. Until that lands, ship-side claims about `ensure_us` improvement from A.3 + C rest on the trace evidence in the corresponding commits, not on a regression-gated benchmark.

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

### A.2b — Inlay weave on worker [deferred]
- Move inlay weaving (currently UI-side per-row) into the worker's row composition; would let the GPUI fast path consume `VisibleRows` unconditionally and let the TUI peer drop its `visible_spans` reader.
- Blocks B.2.

### B (remaining) — Incremental caches
- **B.2** — Overlay layers as interval lists; compose static overlays (doc-highlight, search, substitute) once on worker. Blocked behind A.2b (interval lists want the same row-shaped structure).
- **B.3** — N/A. GPUI's `LineLayoutCache` already provides shape-line reuse keyed on `(font_id, size, text)`; an explicit LRU is duplication.
- **B.4** — Identity-preserving Arc publish for unchanged `RenderState` sub-states. Today every dispatch rebuilds every sub-state Arc even when nothing changed (`render_state.rs` comment: `substate_identity_changes_naively_per_publication`). Deferred — bench impact unmeasured; lower priority.

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

### E.2 — Element-tree reuse [pending]
- `EditorView::render` (800+ lines, runs every `cx.notify`) is the candidate for sub-element reuse.
- Needs an investigation pass before scoping: which sub-elements rebuild unnecessarily; whether the notify cadence is over-firing.

## Instrumentation

- Split timers inside `ensure` (`cursor_snap`, `fold_snap`, `inactive_pane_refresh`) and inside the worker (fetch spans, overlay merge, row compose, shaping hits) — all gated behind `profile-frames`.
- Counters worth adding before E lands: `rows_reused` / `rows_rebuilt`, `overlay_quads`, `shaped_cache_hits`.

## Acceptance gates

Each gate requires: reference machine class, fixed corpus, release-profile build, default logging.

- Idle: `frame_us < 3 ms`.
- Keystrokes (medium corpus — define once, e.g. `syntax.rs` ~8 kLOC, 150 visible rows, default theme): p50 < 6 ms, p95 < 8 ms, p99 < 10 ms.
- Regression gate: `benches/highlights_worker.rs` is wired in CI for the worker hot path (baselines above). A GPUI `frame_us` bench for the UI side is **not yet wired** — outstanding.

## Implementation pointers

- `lattice-host/src/highlights_worker.rs` — builds + publishes precomposed rows; A.2b will add overlay intervals + inlay expansions; will need to pull theme from `RenderState.theme` (not `Theme::default()`).
- `lattice-host/src/render_state.rs` — `VisibleRows` / `RowPrepaint` / `RowRun` types; `Arc<[T]>` storage; `Default` impls construct empty Arc.
- `lattice-host/src/folds.rs` — `FoldIndex` with `partition_point` lookups; both renderer peers consume.
- `lattice-ui-gpui/src/editor_element.rs` — `shape_row_from_prepaint` consumes precomposed rows for the active pane no-inlay fast path; falls back to span path otherwise.
- `lattice-ui-tui/src/render.rs` + `app/highlights.rs` — threads the new `visible_rows` cell; compose loop still reads `visible_spans` pending A.2b.

## Notes

- Every feature preserved (doc highlights, search, visual, substitute, inlays).
- Composition is off the UI thread for the active pane no-inlay path; recompute keyed on input change.
- Paramount goal #4: "Nothing blocks the UI — enforced architecturally, not by discipline." Every slice here is an architectural enforcement, not a discipline reminder.
