# GPUI perf plan (A–F)

Targets: keep every feature, hit <3 ms idle frames, <6 ms median keystroke frames, 95p <8 ms, 99p <10 ms.

Dominant costs from traces (pre-plan):
- ensure_us ≈ 2.5–5.3 ms — per-frame fold geometry + per-pane cursor/scroll/snap re-runs regardless of input deltas.
- highlights_us ≈ 2.5–4.1 ms — UI-thread span consumption + overlay merge (the worker walk itself is already off-thread, X2).
- paint/text/shaping are cheap.

## Status as of 2026-05-24

Twelve slices shipped (A.4, F, A.3, C + follow-up, A.1, A.2a, B.1, D.1, E.1, E.2.α, A.2b.1, A.2b.2). The plan's `ensure_us` and `highlights_us` UI-thread costs have been attacked: ensure work is gated and fold geometry is O(log); the worker now publishes pre-painted rows via `Arc<[T]>` with inlays already woven in so the GPUI fast path consumes `VisibleRows` unconditionally; overlay quads are pre-bucketed in prepaint so paint is an allocation-free walk; and the renderer-thread prepaint phase has bench coverage. Remaining: A.2b.2b (TUI compose-loop migration to `visible_rows`) and A.2b.3 (re-bench inlay weave on worker).

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
| A.2b.2 — Worker weaves inlays + GPUI gate drop + TUI source-of-truth swap | done | — | `RowRun` → enum (`Source` / `Inlay`); `RowPrepaint.inlay_offsets`; `VisibleHighlightsKey.inlay_version` axis; GPUI active-pane fast path is unconditional; TUI reads `rs.syntax.inlay_hints` instead of raw LSP cache. |
| A.2b.2b — TUI compose loop migrates to `visible_rows` | pending | — | Compose loop reads `RowPrepaint.combined` + `runs` + `inlay_offsets`; drops `visible_spans` reader for active pane. |
| A.2b.3 — Re-bench inlay weave     | pending | —         | Confirm GPUI prepaint surface drops 3–5× per the A.2.α projection; update baselines. |
| B.2 — Overlay precompute on worker | deferred | —      | Blocked behind A.2b.2b (interval lists want the same row-shaped structure). |
| B.4 — Identity-preserving sub-state Arc publish | deferred | — | Lower priority; bench impact unmeasured.                |
| D.* — rayon / SmallVec / bump-alloc | dropped | —       | Bench review (below) didn't justify the impl cost.                 |
| E.2 — Element-tree reuse          | pending | —         | Needs investigation pass on `EditorView::render` notify cadence.   |

Known pre-existing gaps (not introduced by this plan, called out so they're not lost):
- TUI compose loop still reads `visible_spans` for the active pane (A.2b.2b will close this).
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

### A.2b.2 — Worker weaves inlays + GPUI gate drop + TUI source-of-truth swap [pending commit]
- `RowRun` promoted from a struct to an enum: `Source { len, style }` for source bytes, `Inlay { len }` for inlay-virtual-text bytes. Consumers map `Inlay` to their inlay colour (resolved on the renderer — GPUI: `inlay_color`; TUI: `inlay_hint_style`).
- `RowPrepaint.inlay_offsets: Arc<[(u32, u32)]>` records each splice `(orig_byte, char_width)` so cursor / decoration / overlay byte→column remap stays correct in `combined`-space.
- `VisibleHighlightsKey.inlay_version: u64` axis added. Worker invalidates the row cache (B.1 dirty-row path) whenever the inlay payload's content hash flips. `Editor::build_active_inlay_hints` returns the `(Arc, hash)` pair so the hash is byte-aligned with the published list by construction; empty list hashes to 0 to keep the steady-state no-hint path on a single cheap branch.
- Worker introduces `weave_row` + `bucket_inlays_by_line`. The no-inlay fast path (empty `inlay_hints`) skips the splice machinery and reuses the A.2a no-alloc `collapse_source_runs` shape; the inlay path walks `line_text` once, splicing inlays at their byte offsets and partitioning into Source / Inlay runs.
- GPUI: `shape_row_from_prepaint` now returns `(ShapedLine, Vec<(u32, u32)>)`; the `line_has_inlays` gate is gone. Active-pane prepaint takes the fast path unconditionally; inactive panes still use `shape_row` + `build_line_with_inlays` since `visible_rows` is active-pane-only.
- TUI: `compose_visible_lines` reads `rs.syntax.inlay_hints` directly (publish-time gated + flattened) instead of `rs.lsp.inlay_hints` + per-line mode gate + per-hint utf-16 conversion + label flatten. FrameView's `lsp_inlay_hint_enabled` cache field dropped. Full compose-loop migration to `visible_rows` deferred to A.2b.2b.
- 8 new worker unit tests: `collapse_source_runs` × 2, `weave_row` × 5 (no-inlay, mid-line, trailing, multi-inlay, empty-line variants), `bucket_inlays_by_line` × 2, `build_rows_with_cache_falls_back_on_inlay_version_change`, `recompute_weaves_published_inlay_hints_into_rows`.
- Existing TUI inlay tests stay green (`inlay_hint_overlay_splices_virtual_text`, `inlay_hint_overlay_suppressed_when_mode_off`) — they exercise the new source-of-truth path end-to-end.
- Pre-existing renderer behaviour preserved: inactive panes still render their own per-buffer inlay hints via the legacy `build_line_with_inlays` path.

### A.2b.2b — TUI compose loop migrates to `visible_rows` [pending]
- Active-pane `compose_visible_lines` reads `RowPrepaint.combined` + `runs` + `inlay_offsets` instead of `view.visible_highlights` + raw line text + post-hoc inlay splice. Drops `visible_spans` reader for the active pane.
- Inactive pane (`draw_inactive_document`) keeps `pane_highlights` — the worker only publishes rows for the active document.
- Closes the "TUI drops `visible_spans` reader" goal that A.2b was originally scoped to deliver.

### A.2b.3 — Re-bench inlay weave [pending]
- Re-run `editor_element_frame_with_inlays` + `worker_recompute_*` after A.2b.2 + A.2b.2b land.
- Confirm GPUI prepaint inlay surface drops 3–5× (the E.2.α projection) now that the active-pane fast path doesn't fall back on `build_line_with_inlays`. Update baselines.

### B (remaining) — Incremental caches
- **B.2** — Overlay layers as interval lists; compose static overlays (doc-highlight, search, substitute) once on worker. Blocked behind A.2b.2b (interval lists want the same row-shaped structure).
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

### E.2.α — Prepaint bench coverage [`f8aa713`]
- Extended `benches/editor_element_frame.rs` with `editor_element_frame_with_overlays`: per-row `push_range_quads` for 5 doc-highlights + 10 hlsearch + 1 visual + 1 substitute, across viewports 24 / 60 / 120.
- Required exposing `push_range_quads` as `pub` (it stays `pub(crate)`-visible in default builds via the module's `pub(crate) mod` gating; `pub` in default builds; full `pub` reach only under `bench-internals`).
- Captured baselines (above) confirm the prepaint phase is well under budget at viewport 120 (~104 µs with overlays) and that inlay weave dominates overlay weave 3–5×.
- **Investigation findings** that informed scope: render() has 4 `cx.notify()` callers (worker bridge, popup-dismiss, on_key_down, tab click) — input-driven cadence, no over-fire. Conditional overlay-block construction is already correct (E/F/G/H/I `return None` and skip when state is `None`). The remaining cost surfaces (per-char `div()` cells in popup / picker / completion overlays; tabline label `SharedString` churn; `render_state.load()` micro-churn) are real but only active during overlay-open frames or sub-µs in steady state.

### E.2.* — Remaining sub-slices [bench-justified ordering]
1. **A.2b.2b** (TUI compose loop → `visible_rows`) — closes the "TUI drops `visible_spans` reader" goal; lets the worker be the single source of truth for inlay weaving across both peers.
2. **A.2b.3** (re-bench) — capture inlay-weave savings now that GPUI's `line_has_inlays` gate is gone; verify the 3–5× projection.
3. **E.2.a** (promote popup overlay body to a shared `OverlayElement`) — only matters while popup is open; defer until someone profiles popup-open frames and finds them hot.
4. **E.2.b / c** (same fix for picker / completion overlays) — same justification as E.2.a; smaller surface.
5. **E.2.d** (tabline `SharedString` identity reuse) — every frame; tiny win; defer.
6. **E.2.e** (consolidate `render_state.load*()` in render + paint_pane) — sub-µs; skip unless cleanup pressure arises.

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

- `lattice-host/src/highlights_worker.rs` — builds + publishes precomposed rows; A.2b.2 added the per-row `weave_row` + `bucket_inlays_by_line` splice that produces `RowRun::Source` / `RowRun::Inlay` runs + `inlay_offsets`; B.2 will add overlay intervals; will need to pull theme from `RenderState.theme` (not `Theme::default()`).
- `lattice-host/src/render_state.rs` — `VisibleRows` / `RowPrepaint` / `RowRun` (now enum) types; `Arc<[T]>` storage; `Default` impls construct empty Arc; `VisibleHighlightsKey.inlay_version` axis paired with `inlay_hints_version()` content hash.
- `lattice-host/src/folds.rs` — `FoldIndex` with `partition_point` lookups; both renderer peers consume.
- `lattice-ui-gpui/src/editor_element.rs` — `shape_row_from_prepaint` consumes precomposed rows for the active pane (unconditional after A.2b.2 lifted the `line_has_inlays` gate); falls back to `shape_row` + `build_line_with_inlays` for inactive panes.
- `lattice-ui-tui/src/render.rs` + `app/highlights.rs` — threads the new `visible_rows` cell; compose loop still reads `visible_spans` (active pane) pending A.2b.2b; inlay overlay reads `rs.syntax.inlay_hints` since A.2b.2.

## Notes

- Every feature preserved (doc highlights, search, visual, substitute, inlays).
- Composition is off the UI thread for the active pane no-inlay path; recompute keyed on input change.
- Paramount goal #4: "Nothing blocks the UI — enforced architecturally, not by discipline." Every slice here is an architectural enforcement, not a discipline reminder.
