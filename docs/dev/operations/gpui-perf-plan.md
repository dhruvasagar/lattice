# GPUI perf plan (A–F)

Targets: keep every feature, hit <3 ms idle frames, <6 ms median keystroke frames, 95p <8 ms, 99p <10 ms.

Dominant costs from traces:
- ensure_us ≈ 2.5–5.3 ms — per-frame fold geometry + per-pane cursor/scroll/snap re-runs regardless of input deltas.
- highlights_us ≈ 2.5–4.1 ms — UI-thread span consumption + overlay merge (the worker walk itself is already off-thread, X2).
- paint/text/shaping are cheap.

Baseline must be captured (machine class, corpus, build profile, logging on/off) before any slice lands so each acceptance gate has a verifiable delta.

## Stage order

Slices are ranked by largest verified shave per implementation hour. The A–F labels are kept so cross-references stay stable; the order they ship in is:

1. **A.4** — Demote per-frame DEBUG logs behind a profiling feature.
2. **F** — Build profile (LTO thin, codegen-units=1 where safe, panic=abort, feature-gate tracing).
3. **A.3 + C** — Ensure gating + visual-row fold index. Both attack `ensure_us` directly with no cross-renderer ABI churn; can land in parallel.
4. **A.1 [DONE]** — Rope-line window in `window.rs` (already shipped).
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

### A.4 — Logging demotion
- Feature-gate the per-frame DEBUG frame-budget lines (e.g. `profile-frames` cargo feature) so default release builds skip the format machinery.
- Keep TRACE/DEBUG paths intact for profiling builds.

### F — Build profile
- Release: `opt-level=3`, `lto=thin`, `codegen-units=1` (where it doesn't blow the CI build budget), `panic=abort`.
- Feature-gate tracing macros so `cargo build --release` strips them entirely.

### A.3 — Ensure gating
- Cache last `(cursor, scroll, viewport_height, fold_hash)` in GPUI; early-return when unchanged.
- Split per sub-step (`cursor_snap`, `fold_snap`, `inactive_pane_refresh`) and gate each independently — cursor-only deltas must not re-run fold geometry.

### C — Folds / scroll O(1)
- Visual-row index for folds: maintain a `visual_row → buffer_line` mapping that updates on fold open/close, not on every ensure call.
- Cursorline / doc-highlight invalidation keyed on actual input change.

### A.1 — Rope-line window [DONE]
- `window.rs`: removed `snapshot.text() + split('\n')`; materialise only `[visible_start, visible_end)` via `Buffer::line()`.
- Follow-up worth noting: the per-line `String` alloc (one per visible row per frame) is still measurable at 120 Hz; consider `Cow<'_, str>` or pushing lines into the element without an intermediate `Vec`.

### A.2 — Worker pre-paint
- Extend `VisibleSpans` → `VisibleRows` carrying:
  - `combined_text` (storage TBD — bound the alloc; `Box<str>`/`Arc<str>` or arena).
  - `runs` — **style-tagged, not RGB-baked**, so theme switch doesn't invalidate the cache; UI resolves style → RGB at paint time from `RenderState.theme`.
  - `inlay_offsets: Vec<(orig_byte, char_width)>`.
  - `byte_to_combined_col` index per row.
  - Overlay quads for **static-on-change** layers only (doc-highlight, search, substitute). Cursor-following overlays (cursorline, visual) stay UI-side — they churn at every motion and would dominate the worker hot path.
- Publish `Arc<VisibleRows>` via the existing `ArcSwap` cell (`render_state.syntax.visible_spans`).
- Key equality: `(snapshot_ptr, syntax_text_version, scroll, viewport_height, fold_hash, overlay_state_hash, theme_hash)`.
- **Open questions before merging A.2:**
  - Inactive-pane story — does `pane_highlights` migrate to `Vec<RowPrepaint>`, or does `VisibleRows` stay active-pane-only with `pane_highlights` kept as the legacy fallback? Silently dropping inactive-pane styling is not acceptable.
  - Worker must read `RenderState.theme`, not `Theme::default()`.
  - TUI peer (`lattice-ui-tui/src/render.rs`, `app/highlights.rs`) needs an adapter for the type change — the published cell is shared across peers.

### B — Incremental caches (1–2 weeks)
- Dirty-row recomposition keyed by `(text_version, spans_hash, inlay_hash, overlay_hash)`.
- Overlay layers as interval lists; compose once on worker.
- Shaped-line reuse LRU keyed by `(font_id, size, combined_text_hash, runs_hash)` — hash, not byte compare.
- Identity-preserving Arc publish for unchanged sub-states (`RenderState` today rebuilds every sub-state Arc per dispatch — see `render_state.rs::substate_identity_changes_naively_per_publication`).

### D — Parallelisation / preallocs
- Rayon for row prep (small win at 150 rows; benchmark before committing).
- SmallVec for per-row run lists; bump allocator for transient worker buffers.
- Minimise `Arc::new` churn in the publish path.

### E — Render niceties
- Batch overlay quads.
- Avoid rebuilding element trees where GPUI allows.

## Instrumentation

- Split timers inside `ensure` (`cursor_snap`, `fold_snap`, `inactive_pane_refresh`) and inside the worker (fetch spans, overlay merge, row compose, shaping hits).
- Counters: `rows_reused` / `rows_rebuilt`, `overlay_quads`, `shaped_cache_hits`, `cache_hit_short_circuits`.

## Acceptance gates

Each gate requires: reference machine class, fixed corpus, release-profile build, default logging.

- Idle: `frame_us < 3 ms`.
- Keystrokes (medium corpus — define once, e.g. `syntax.rs` ~8 kLOC, 150 visible rows, default theme): p50 < 6 ms, p95 < 8 ms, p99 < 10 ms.
- Regression gate: wire `benches/highlights_worker.rs` into CI for the worker hot path; add a GPUI `frame_us` bench for the UI side or call out the gap explicitly.

## Implementation pointers

- `lattice-host/src/highlights_worker.rs` — build + publish precomposed rows; compute overlay intervals + inlay expansions; pull theme from `RenderState.theme`.
- `lattice-host/src/render_state.rs` — `VisibleRows` / `RowPrepaint` / `RowRun` types; add `overlay_state_hash` + `theme_hash` to `VisibleHighlightsKey`.
- `lattice-ui-gpui/src/editor_element.rs` — consume precomposed rows + quad lists; resolve style → RGB at paint time; retain shaping cache key.
- `lattice-ui-tui/src/render.rs` + `app/highlights.rs` — adapter for the type change so the TUI peer keeps its current paint path.

## Notes

- Preserve every feature (doc highlights, search, visual, substitute, inlays).
- Move composition off the UI thread; recompute only on input change.
- Paramount goal #4: "Nothing blocks the UI — enforced architecturally, not by discipline." Every slice here is an architectural enforcement, not a discipline reminder.
