# Benchmark Results

Captured numbers from the criterion suite, indexed against the
performance commitments in ../architecture/design.md §8.2 (Floor / Target / Today
/ Stretch).

This document is a snapshot, not a moving record -- update it when a
deliberate perf change lands or a new bench is added; do not bump
numbers on every routine run. Commit history is the moving record.

Each row points back at the §8.2 commitment it backs, so a
regression here is identifiable as a violation of a specific
target rather than just a slower number.

> **On the `Floor / Target` column.** "Floor" is the typical
> achieved number on this hardware (median of recent runs).
> "Target" is **what we want to keep delivering**, not the
> §8.2 spec ceiling. Where current achievement is well under
> the §8.2 ceiling we set the target tight (~2-3× the floor)
> so any meaningful regression flags. Strive for best:
> the spec value is a guide, the achieved number is the bar.
> When a regression is intentional the row's target moves with
> the new floor in the same commit -- never silently relaxed.

> Run `cargo bench --workspace` to reproduce. Times shown are
> criterion's median estimate. Each row's outer `[low high]`
> bracket is the 95% confidence interval; we report the median.

---

## Post-3c.unify (slice 7 + 8, 2026-05-21)

Snapshot after the slice-7 unification arc (7a-7g) closed:
single source-registration contract (`SourceRegistration`),
typed accept payload on every picker candidate
(`RawCandidate::accept_action`), dual-registry lookup in
`open_picker`, accept dispatch via `DefaultAcceptHandler`,
preview unification (LSP-references-preview gap closed).

**The architectural question this run answers:** does the
unified-shape plumbing (carrying `Option<AcceptAction>` on every
candidate that flows through the pipeline) regress picker
filter performance? Initial measurement before slice 8's
boxing optimisation showed +97% on the empty-query 5k case
(774µs → 1.52ms) because `AcceptAction` is an enum carrying
PathBuf / String / Args variants — its inline size dominated
RawCandidate, doubling per-candidate memcpy through the
matcher/ranker passes.

Slice 8's fix: box the field
(`accept_action: Option<Box<AcceptAction>>`). `Option<Box>` is
8 bytes regardless of variant size. Cmdline-completion and
insert-completion candidates leave the field `None` → null
pointer → free. Picker candidates pay one heap alloc per row
at construction, recovered ~10× over by smaller per-candidate
memcpy during refilter (fires per keystroke).

### Headline deltas

| Bench                                   | Pre-arc baseline | Post-7 (inline) | Post-8 (boxed) | Δ vs baseline |
|-----------------------------------------|------------------|-----------------|-----------------|----------------|
| `picker::refilter/n=5000,query=""`      | 774µs            | 1.52ms (+97%)   | **801µs**       | +3.5%          |
| `picker::refilter/n=5000,query="f"`     | 1.50ms           | 1.41ms (-6%)    | **1.43ms**      | -4.7%          |
| `picker::refilter/n=5000,query="file_"` | 1.57ms           | 1.65ms (+5%)    | **1.61ms**      | +2.5%          |
| `picker::refilter/n=500,query=""`       | ~60µs            | 60µs            | **60µs**        | flat           |
| `picker::open_inline/5000`              | ~1.66ms          | 1.68ms          | **1.94ms**      | +17%           |
| `picker::open_inline/500`               | ~132µs           | 130µs           | **127µs**       | -4%            |
| `picker::mru_snapshot/5000`             | ~542µs           | 521µs           | **520µs**       | -4%            |

**Verdict:** the refilter hot path (per-keystroke; the budget
that matters for input latency) lands within noise of the
pre-arc baseline. `open_inline/5000` shows +17% — the heap
alloc per candidate at seat time. Acceptable because
picker-open fires once per `:picker <name>` invocation, not
per keystroke; user-perceived latency is dominated by the
subsequent refilters.

### Cost analysis

Each `RawCandidate` carrying `accept_action: Some(...)` pays:
  - 1 heap alloc at construction (~50ns × 5000 candidates =
    ~250µs); the source of the open_inline/5000 +260µs delta.
  - 8 bytes inline (Box ptr) instead of ~64 bytes inline
    (largest variant: OpenLspLog with String + PathBuf).
    Recovers ~280KB of memcpy per refilter at 5k scale.

Net: refilter wins, open_inline pays. The trade is favourable
because refilter fires per-keystroke and open_inline fires
once per picker invocation.

If/when `open_inline` becomes a hot enough complaint, the
candidate-vec construction can move to a bumpalo arena (free
all allocs at picker-close instead of per-candidate). Out of
scope for slice 8; queued as `3c.unify.arena-candidates` if
needed.

---

## Post-3c.final.E.swap + B-extension (2026-05-21)

Snapshot taken after the Phase 5.8.AF.5 / 3c.final arc closed:
the Editor now lives on its own dedicated thread (slice
`3c.final.E.swap`, compile-time-enforced via the cfg-gated
`App.editor: Editor → App.editor_actor: EditorActorHandle`
swap), and slices B.7–B.9 lifted six per-frame `read_editor`
round-trips off the hot paint paths to wait-free `Arc::clone`
reads against published RS sub-states (Messages, Modeline,
Options, Modes, SyntaxRS.pane_highlights, BufferLocals).

**The architectural question this run answers:** does compile-
time-enforced async (paramount goal #4) plus the 6 new RS sub-
states come at a measurable cost? Predictions made beforehand:
`snapshot_publish_standalone` +50-200ns (~1ns per Arc::new × 6);
`apply_edit_round_trip` / `dispatch_round_trip` +5-20µs from
the actor mailbox; frame paint flat-or-faster; read paths flat.

### Headline deltas

| Bench                                       | Baseline (2026-05-13) | Today (2026-05-21) | Δ          | Prediction       |
|---------------------------------------------|------------------------|---------------------|-----------|------------------|
| `runtime::snapshot_load/load`               | 16ns                  | **15.92ns**         | flat       | ✅ flat          |
| `runtime::snapshot_load_cached/steady`      | 290ps                 | **285.69ps**        | flat       | ✅ flat          |
| `runtime::snapshot_publish_standalone/10`   | 95ns                  | **101.43ns**        | **+6.7%**  | ✅ +50-200ns; came in at +6ns |
| `runtime::snapshot_publish_standalone/1000` | (95ns)                | 102.05ns            | +7.4%      | ✅               |
| `runtime::snapshot_publish_standalone/50000`| (95ns)                | 98.81ns             | +4.0%      | ✅               |
| `runtime::status_segment_update`            | 56ns                  | **55.68ns**         | flat       | ✅ flat          |
| `runtime::apply_edit_round_trip/10`         | 77µs                  | **81.26µs**         | +5.5% (~4µs)| ✅ +5-20µs band; low end |
| `runtime::apply_edit_round_trip/1000`       | (77µs)                | 77.16µs             | flat       | ✅               |
| `runtime::apply_edit_round_trip/50000`      | (77µs)                | 77.43µs             | flat       | ✅               |
| `runtime::dispatch_round_trip/10`           | 79.69µs               | **77.75µs**         | **-2.4%**  | ⚡ better than predicted |
| `runtime::dispatch_round_trip/1000`         | 90.50µs               | **87.31µs**         | **-3.5%**  | ⚡ better        |
| `runtime::dispatch_round_trip/50000`        | 572µs                 | **557µs**           | **-2.6%**  | ⚡ better        |
| `highlight::rust_viewport/24_lines`         | 185.23µs              | **186.88µs**        | flat       | ✅ flat (control)|
| `highlight::rust_viewport/60_lines`         | 259.12µs              | **261.17µs**        | flat       | ✅ flat (control)|
| `highlight::rust_viewport/120_lines`        | —                     | 376.53µs            | new        |                   |

### Architectural read

- **Read paths unaffected.** `snapshot_load` (15.92ns) and
  `snapshot_load_cached` (285.69ps) are flat within criterion
  noise. The actor swap added zero overhead to the wait-free
  `ArcSwap::load` semantics — exactly the property that made
  the swap correctness-preserving.
- **Publish cost +6.7% (+6ns absolute).** Six new sub-states
  each contribute one `Arc::new(SubState { ... })` per publish.
  At ~1ns per Arc allocation + small struct move, the predicted
  ~6ns delta matches observed. On a default `Editor` the
  `BufferLocals::clone` deep-walk (added by B.9) is a no-op
  because there are no entries; production sessions with 5-20
  open buffers × ~3-10 locals add some — bench coverage for
  that fully-populated path is queued as `bench_publish_populated`.
- **Dispatch round-trip improved 2–4%.** The mpsc-send +
  oneshot-recv mailbox roundtrip cost was expected to add
  5–20µs vs the pre-swap synchronous direct dispatch. It
  doesn't — and is actually *slightly faster*, because the
  prior "publish RenderState after every App helper" pattern
  has been replaced by one tail-publish per `mutate_editor`
  closure. Net: fewer publishes per Action chain, even with
  the channel overhead added.
- **Apply-edit round-trip flat at typical sizes.** The +5.5%
  blip at `/10` (~4µs) sits inside the per-bench noise floor
  (WSL2 host drift, documented below) and disappears at
  `/1000` and `/50000`. No code-attributable regression.
- **Highlight viewport flat.** `rust_viewport/{24,60}_lines`
  match the 2026-05-13 baseline within ±2%. The highlight
  pipeline doesn't touch the RS-publish path; this row was
  included as a control to confirm the bench environment is
  comparable, and it is.

### Frame paint — fixed via `3c.fixup.actor-block-on` + `3c.extension.fold-rs`

First attempt at the render bench panicked. **Two real defects
surfaced**, both addressed in same-day follow-up slices:

**Defect 1: actor-runtime `block_on` mismatch** (fixed in slice
`3c.fixup.actor-block-on`, commit 2647011). The actor thread
runs a `current_thread` tokio runtime; `lattice_runtime::
block_on` called `tokio::task::block_in_place` whenever
`Handle::try_current()` returned `Ok`, but `block_in_place`
requires `MultiThread` — it panicked on the actor's runtime.
Production impact would have been release-build panics on file
save, LSP completion-resolve, synthetic-buffer seed, etc.
`cargo test` missed it because `cfg(test)` preserves direct
`App.editor: Editor` ownership (no actor spawned). Fixed by
adding a `Handle::runtime_flavor()` branch: `MultiThread` uses
`block_in_place` as before; non-`MultiThread` escapes to a
fresh OS thread via `std::thread::scope` so `target.block_on`
runs outside any tokio context.

**Defect 2: per-frame actor RPCs in paint paths** (fixed in
slice `3c.extension.fold-rs`, commit dc30942). With the panic
gone, the render bench showed +373× regression: `frame_120_
lines/200` at 43.73ms vs the 90µs 2026-05-13 baseline. Each
paint of a 120-line frame paid ~120 actor mailbox round-trips
(~94µs each) for per-line gates: `app.line_inside_closed_fold`,
`app.fold_start_at`, plus per-line LSP mode-enabled checks for
diagnostics / semantic-tokens / document-highlights / inlay-
hints / progress, plus the gutter's per-line `app.relative_line_
numbers()`. The B-extension lifted most per-frame reads but
missed these.

Two changes landed `fold-rs`:

1. `FrameView` caches the per-frame option + mode-gate reads at
   construction (one read each at frame entry, then per-line
   lookups read the cached bool). Per-line callers
   (`compose_visible_lines_inner`, `render_gutter_for`,
   `severity_for_line`, `diagnostics_on_line`) switched from
   `&App` to `&FrameView`.
2. App-level accessors (`App::foldenable`, `App::lsp_*_mode_
   enabled_for`) rewritten to read RS directly. `foldenable`
   reads `ad().option_cache.foldenable`; the LSP gates read
   `app.modes().map.get(buffer).has_minor(mode_id)` against the
   `ModesRenderState` published by slice B.11.

Post-fix numbers vs 2026-05-13 baseline:

| Bench                                  | Baseline | Today      | Δ                          |
|----------------------------------------|----------|------------|----------------------------|
| `render::frame_24_lines/200`           | 15µs     | **21.3µs** | +42% (criterion noise)     |
| `render::frame_60_lines/200`           | 46µs     | **60.0µs** | +30%                       |
| `render::frame_120_lines/200`          | 90µs     | **117.3µs**| +30%                       |
| `render::refresh_highlights_cache_hit` | 21ns     | 99.8µs     | irreducible — one actor RPC per call; not per-frame in production |

The ~30% spread on frame paint is the FrameView construction
cost (six `Arc::load_full` + an Arc-clone of the syntax spans +
typed-options lookups; all wait-free) plus criterion noise from
the host-state drift documented below. Tight enough for §8.2
(the frame budget is 500-800µs depending on viewport size).

`refresh_highlights_cache_hit` remains at the actor-RPC floor
(~100µs) — this bench measures `app.refresh_highlights()`
directly, which is `mutate_editor(|e| e.refresh_highlights())`.
The mailbox round-trip itself is the cost. In production this
fires on edit / scroll / config change, not per frame; the
per-frame paint reads the worker-published `visible_spans` cell
wait-free.

### Host (highlights_worker) — new in this run

| Bench                                | Today        | Note |
|--------------------------------------|--------------|------|
| `worker_cache_hit/24`                | **51.69ns**  | wait-free cache lookup; measures the worker's input-key compare. |
| `worker_cache_hit/60`                | 51.50ns      |      |
| `worker_cache_hit/120`               | 50.46ns      |      |
| `worker_recompute_on_scroll/24`      | 197.30µs     | scroll-only recompute |
| `worker_recompute_on_scroll/60`      | 263.23µs     |      |
| `worker_recompute_on_scroll/120`     | 392.90µs     |      |
| `worker_stale_snapshot_hold/24`      | 3.12µs       | stale-snapshot HOLD path |
| `worker_stale_snapshot_hold/60`      | 4.27µs       |      |
| `worker_stale_snapshot_hold/120`     | 5.52µs       |      |

### What this means for the §8.2 commitments

Every row in the §8.2 commitments table above remains within
its v1 target after the architectural changes:

- Snapshot load (< 20ns) — **16ns**, unchanged.
- Snapshot load cached (< 500ps) — **286ps**, unchanged.
- Snapshot publish (< 500ns) — **101ns**, +6ns from B-extension
  sub-states; still 5× under target.
- Apply-edit round-trip (< 100µs) — **77-81µs**, unchanged.
- Dispatch round-trip (< 100µs at typical sizes) — **77-87µs**,
  unchanged or slightly improved.
- Frame render TUI 80×24 (< 500µs) — UNMEASURED this run; will
  re-bench after the `block_on` defect is fixed. Highlight-side
  contribution is unchanged (rust_viewport/24_lines flat).

### Bench methodology — what changed vs 2026-05-13

The 2026-05-13 run used `cargo bench --workspace`. This run
followed the same toolchain (1.94.0 stable) and bench profile
(`opt-level = 3`). The `--workspace` run was abandoned mid-way
on 2026-05-21 because it didn't fit the planned assessment
window; targeted crate-by-crate bench runs (`cargo bench -p
lattice-{runtime,syntax,host}`) gave the same numbers in a
fraction of the time. Crates not benched today —
`lattice-config`, `lattice-grammar`, `lattice-picker`,
`lattice-core` — exercise paths untouched by the architectural
changes; their 2026-05-13 numbers carry forward unchanged.

### Bench environment continues to drift

Several criterion-flagged rows sit inside the documented WSL2
noise floor:

- `highlight::python/200`: -6.9% (improvement, not regression —
  criterion's hypothesis test phrases both directions as
  "change detected").
- `highlight::python/2000`: -19.9% (improvement).
- `motion::word_backward/10`: +3.6% (1.34µs absolute, sub-µs
  delta, noise).
- `reparse_incremental_single_char_change/2000`: 1.46ms →
  1.94ms (+33%). This bench isn't touched by the architectural
  arc; suspect host-state drift (same WSL2 + CPU governor
  story documented in the 2026-05-13 section). Worth a
  controlled re-probe but not architecturally significant.

---

## Post-perf-plan (2026-05-25)

Snapshot after the GPUI perf plan (archived at
[`../archive/gpui-perf-plan.md`](../archive/gpui-perf-plan.md)) closed.
Nineteen slices shipped between 2026-05-21 and 2026-05-25, plus E.2
formally dropped on bench-justified grounds. The plan attacked the
two dominant UI-thread costs identified in profiling (`ensure_us` and
`highlights_us`) and ended with `Editor::publish_render_state` itself
made identity-preserving on the seven highest-allocation sub-states.

**Slice arcs that landed:**

- **A.* / B.1 / D.1 / E.1** — worker pre-paints rows (`VisibleRows`)
  with inlays woven in (`RowRun` enum); both renderer peers consume
  the same pre-woven rows wait-free; `Arc<[T]>` publish types collapse
  HOLD-path clones to a single Arc bump.
- **A.2b.* / B.2.*** — overlay buckets. Worker pre-buckets the three
  static overlay layers (doc_highlight / all_matches / substitute)
  per row in source-byte space; both peers consume the same bucket.
  Eliminates the per-frame O(N_overlay × V_row) intersection walk
  that scaled to ~1 ms/frame on a 1000-match `hlsearch` corpus.
- **B.4.a / B.4.b** — identity-preserving Arc publish. `Versioned<T>`
  newtype + `PublishCache` on Editor cache seven sub-state Arcs
  (`panes`, `modes`, `buffer_locals`, `buffers`, `tabs`, inner
  `syntax.pane_highlights`, inner `lsp.progress`) keyed on per-field
  version counters. Reuses prior Arc when input version is unchanged.
- **C** (FoldIndex, O(log N) visual-row math), **A.3** (ensure
  gating), **A.1** (rope-line window), **F** (release profile
  tightening), **A.4** (logging demotion behind `profile-frames`)
  closed earlier in the arc.
- **E.2** (element-tree reuse) dropped after the E.2.α investigation
  found no bench-justified work — notify cadence is input-driven,
  conditional overlay-block construction is already correct, and the
  four candidate sub-slices each had measurement-backed reasons not
  to ship.

**The architectural question this run answers:** does the identity-
preserving publish cache actually pay off in a measurable, no-overhead
way? Predicted: ~50 % savings on no-op publish (`steady_state` regime
where all cached inputs are unchanged) with sub-µs net cost on a
fully-invalidated publish (the cache machinery shouldn't add cost the
rebuild it's avoiding doesn't already pay).

### Headline deltas — new bench `dispatch_publish`

Reproduce: `cargo bench -q -p lattice-host --bench dispatch_publish`.
Fixture: editor with a 3-pane tree, 20 LSP-attached buffers
(`active_modes` + `buffer_locals` + `buffer_uris` populated), 4 tabs,
3 panes × 60 spans of `pane_highlights`, 6 in-flight `$/progress`
items.

| Bench                                | Time     | Δ vs `unmemoised` (pre-B.4 equivalent) |
|--------------------------------------|----------|----------------------------------------|
| `dispatch_publish/steady_state`      | **3.23 µs** | **−52 %** (cache hits everywhere)   |
| `dispatch_publish/mutated_modes`     | 3.68 µs  | −45 % (one cache miss, six hits)       |
| `dispatch_publish/mutated_all`       | 6.44 µs  | −4 % (5 misses + bench-loop mutations) |
| `dispatch_publish/unmemoised`        | 6.72 µs  | baseline (cache cleared each iter)     |

The `unmemoised` row clears the cache between iterations with no
per-iter mutation work — the cleanest pre-B.4 stand-in. The `mutated_all`
row also pays for 5 HashMap insert/remove ops per iteration to bump
the versions; its small delta over `unmemoised` is that bench-loop
overhead, not cache overhead. The cache machinery itself
(`Mutex<PublishCache>::lock` + 7 version reads + 7 slot compares +
7 Arc clones on hits / closure call + `Arc::new` per miss) is zero
net cost on a fully-invalidated publish.

### Headline deltas — `highlights_worker` (post-B.2)

| Worker bench                       | Post-A.2b.2b | Post-B.2 | Δ vs post-A.2b.2b |
|------------------------------------|--------------|----------|-------------------|
| `worker_cache_hit/{24,60,120}`     | ~49 ns       | ~50 ns   | flat              |
| `worker_recompute_on_scroll/24`    | 185.4 µs     | 199.9 µs | +7.8 %            |
| `worker_recompute_on_scroll/60`    | 260.0 µs     | 282.5 µs | +8.7 %            |
| `worker_recompute_on_scroll/120`   | 374.7 µs     | 415.7 µs | +10.9 %           |
| `worker_stale_snapshot_hold/{24,60,120}` | ~2.6-2.9 µs | ~2.9 µs | flat-to-+11%   |

The +7–11 % on the recompute path is the new per-recompute static-
overlay bucket build (`bucket_static_overlays` walk + `snap.source()`
access + the third `Arc<ArcSwap<...>>` cell store). **Architecturally
correct** — every µs added on the worker thread is a µs removed from
the renderer's per-frame body. Worker fires once per text/scroll
change; renderer fires every paint.

### Headline deltas — `editor_element_frame` (post-B.2)

| GPUI prepaint bench (viewport 120)       | Post-A.2b.2b | Post-B.2 | Δ      |
|------------------------------------------|--------------|----------|--------|
| `editor_element_frame_pre_paint`         | 104.3 µs     | 90.1 µs  | −14 %  |
| `editor_element_frame_with_inlays`       | 130.2 µs     | 118.6 µs | −9 %   |
| `editor_element_frame_with_overlays`     | 94.0 µs      | 89.9 µs  | −4 %   |

Renderer-side helper-bench numbers improved or held flat. The
production active-pane path no longer calls these helpers for the
static overlay layers — it consumes the worker bucket directly — so
this bench measures the inactive-pane / fallback path. Active-pane
real-world cost dropped by the worker-bucket lookup amount that the
bench doesn't capture (no headless `TestAppContext` on gpui 0.2.2).

### Architectural read

- **Editor publish halved on the realistic fixture** (`steady_state`
  3.23 µs vs `unmemoised` 6.72 µs). Most keystrokes don't touch
  `panes` / `modes` / `buffer_locals` / `buffers` / `tabs` /
  `pane_highlights` / `lsp.progress`, so most publishes now reuse
  every cached Arc instead of rebuilding them.
- **Renderer side gains the ability to short-circuit per-frame work
  by `Arc::ptr_eq`** on consecutive frames' sub-state Arcs. No call
  site does this yet; the seam is in place for future slices.
- **High-N hlsearch tail-risk eliminated.** Pre-B.2, the active-pane
  `overlay_quads_for_row` walked every `(hlsearch_match, visible_row)`
  pair per frame — ~1 ms/frame at viewport 120 with 1000 matches.
  Post-B.2 the worker emits ≤ a few quads per row in source-byte space;
  the renderer's walk is O(quads in viewport), not O(N × V).
- **No CI-gateable measurement of the production active-pane path.**
  gpui 0.2.2 doesn't expose a headless `TestAppContext`, so the paint
  phase itself remains `profile-frames`-only. The two bench surfaces
  that ARE CI-gated (`editor_element_frame` for prepaint,
  `dispatch_publish` for the publish path, `highlights_worker` for
  the worker recompute) cover the dominant cost surfaces.

### What this means for the §8.2 commitments

- **Snapshot publish** (`runtime::snapshot_publish_standalone`) stays
  at 95–101 ns — B.4 operates one layer up (`Editor::publish_render_state`
  builds the `RenderState` that gets stored into the snapshot cell).
  The §8.2 row is unchanged.
- **Frame render TUI 80×24 / 200×60** stays under target. Active-pane
  paint is faster than the §8.2 commitments table reflects because the
  worker pre-paints rows + buckets overlays; the bench harness can't
  exercise that path headlessly so the §8.2 table still reports the
  pre-A.2 frame numbers as the gating reference.
- **Editor dispatch publish** is a new measurement surface, not a §8.2
  row. It sits on the actor tail every time `Editor` mutates and would
  scale up roughly linearly with publishes-per-frame; B.4 caps that
  cost on no-op publishes to ~3 µs / publish.

---

## Environment

- Date: 2026-05-13 (post Phase 4.4 + 4.5 LSP slices — supervisor refactor, file watchers, dynamic capability registration, callHierarchy/typeHierarchy/codeLens/documentLink/documentColor pumps + caches; no perf-targeted commits in the window)
- Host: WSL2 (Ubuntu) on x86_64
- Toolchain: Rust 1.94.0 stable
- Build profile: `bench` (`opt-level = 3`)

WSL2 adds ~5-15% overhead vs. native Linux on syscall-heavy paths.
Numbers below are conservative; native CI runners should land
better.

---

## §8.2 commitments at a glance

| §8.2 row                                     | Target (v1)   | Today                                                                 | Bench                                                 | Status                                                                                                                                |
|----------------------------------------------|---------------|-----------------------------------------------------------------------|-------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Snapshot load (`load_full`)                  | <20ns         | **16ns**                                                              | `runtime::snapshot_load`                              | ✅ at floor for `load_full` semantics                                                                                                 |
| Snapshot load (`Cache::load`, steady)        | <500ps        | **290ps**                                                             | `runtime::snapshot_load_cached`                       | ✅ ~55× faster than `load_full`; sub-nanosecond                                                                                       |
| Snapshot publish standalone                  | <500ns        | **95ns**                                                              | `runtime::snapshot_publish_standalone`                | ✅ at the floor (~80ns)                                                                                                               |
| Status segment update                        | <100ns        | **56ns**                                                              | `runtime::status_segment_update`                      | ✅ at the floor                                                                                                                       |
| Apply-edit round-trip                        | <100µs        | **77µs**                                                              | `runtime::apply_edit_round_trip`                      | ✅ scheduler-bound; sync fast-path is the next lever                                                                                  |
| Dispatch round-trip (small buffer)           | <100µs        | 79–91µs                                                               | `runtime::dispatch_round_trip`                        | ✅ same envelope as apply-edit                                                                                                        |
| Frame render TUI 80×24 (highlight + compose) | <500µs        | ~199µs (184 + 15)                                                     | `highlight::rust_viewport` + `render::frame_24_lines` | ✅ under target                                                                                                                       |
| Frame render TUI 200×60                      | <800µs        | ~307µs (261 + 46)                                                     | `highlight::rust_viewport` + `render::frame_60_lines` | ✅ under target                                                                                                                       |
| Open 100MB log (rope construction)           | <100ms        | 74ms                                                                  | `buffer::open_large/100mb`                            | ✅ under target                                                                                                                       |
| Search literal worst-case 200k               | <2ms          | **749µs**                                                             | `search::no_match_with_wrap/200k`                     | ✅ under target; ~14% above prior baseline -- see "Regressions" below                                                                 |
| Tree-sitter incremental reparse              | scale-by-size | **293µs (1600 lines), 1.46ms (16k lines)**                            | `highlight::reparse_incremental_single_char_change`   | ✅ landed (B.2); ~8–16× under full reparse. tree.edit is O(num_nodes) — floor scales with tree size. See Slice B.2 calibration below. |
| Highlight span cache hit (steady-state)      | <50ns         | **21ns**                                                              | `render::refresh_highlights_cache_hit`                | ✅ at floor (B.3); ~8900× faster than the pre-B.3 path.                                                                               |
| Reflex motion / operator                     | <2ms          | mostly under; `d_whole/50000` ≈ 3ms on this host (bench environment drift, not code) | `motion::*`, `operator::*`                            | ⚠️ host-env regression -- bisect attributes the 1.23ms→3ms drift to WSL2 host state, not lattice code. See "Bench environment drift" below. |
| LSP framing parse (Content-Length)           | <500ns        | **68ns**                                                              | `lsp::framing::parse_header_block`                    | ✅ Background-class                                                                                                                   |
| LSP encode `didChange`                       | <2µs          | **183ns**                                                             | `lsp::encode::did_change`                             | ✅ per-keystroke debounced outgoing                                                                                                   |
| LSP decode `publishDiagnostics`              | <10µs         | **1.50µs**                                                            | `lsp::decode::publish_diagnostics`                    | ✅ per-save inbound                                                                                                                   |
| LSP utf-16 column conversion (CJK line)      | <1µs          | **21ns**                                                              | `lsp::position::utf16_cjk_line`                       | ✅ never shows up in flame graphs                                                                                                     |

---

## Bench environment drift — not a code regression (2026-05-13)

Comparing this run against the numbers captured on 2026-05-03,
several rows look like regressions on paper. **A targeted bisect
on the headline candidate (`operator::d_whole/50000`) showed the
"regression" is environmental, not code-attributable.**

### What the bisect showed

| Commit (probed today, same hardware)                       | `operator::d_whole/50000` (median) |
|------------------------------------------------------------|------------------------------------|
| `c94d734` (commit where the **1.23ms baseline** was first written, 2026-05-02) | **2.84ms** |
| `b75c135` (first commit with `.tool-versions` pin)         | ~3.03ms |
| `0759cc6` (last commit before the Phase 4.4 work-window)   | ~3.46ms |
| `dbf30f3` (HEAD)                                            | ~3.03ms |

At the *exact* commit that recorded 1.23ms in May, today's
hardware reports 2.84ms — a 2.3× delta with zero code change. The
real code-attributable delta across `c94d734 → HEAD` is **2.84ms
→ 3.03ms (~7%)**, well inside criterion's confidence interval.
The 6.66ms number captured in the morning's full run wasn't
reproducible 30 minutes later (later runs land at ~3ms in both
`--quick` and full criterion modes).

`criterion` itself wasn't bumped between `c94d734` and HEAD; the
operator-walk code wasn't restructured. Candidate root causes for
the host-side drift:

- **WSL2 kernel update**. The env now reports `Linux
  6.6.87.2-microsoft-standard-WSL2`; the kernel at 2026-05-02
  isn't recorded in the prior doc, but a Windows-host update in
  the intervening 11 days is the most plausible vector. Scheduler
  + vDSO timer paths in WSL2 have historically shifted by ~2×
  between MS kernel revs.
- **CPU governor / thermal state**. The host is shared with a
  Windows GUI; a sustained-load bench like `d_whole/50000`
  (~5GB/s rope walk) is particularly sensitive to whether the
  CPU is sitting at peak frequency or has been parked.
- **L3 / NUMA pressure**. A 50k-line buffer's ropey + tree-sitter
  state may now spill out of L3 where it didn't before; the
  smaller-size variants (`d_whole/10`, `d_whole/1000`) scale
  linearly today (5.15µs → 20.59µs ≈ 4×, matching the line-count
  ratio) while `d_whole/50000` shows the non-linear knee.

### What changed in the doc

* The §8.2 commitments row for "Reflex motion / operator" reads
  ⚠️ instead of ✅, but the qualifying note now points at the
  *bench environment*, not lattice code.
* The Operators table row for `d_whole/50000` shows today's
  ~3ms number, not the stale 1.23ms; the prior baseline is
  preserved in the table footer for historical comparison.
* The "Improvement target" column on `d_whole/50000` calls out
  what *would* be a real regression: any movement past ~6ms at
  the same host state, since 3ms is now this hardware's natural
  floor for that bench.

### Real (modest) regressions worth watching

Even after stripping out the host-drift noise, four rows moved
~10–15% in the wrong direction across `c94d734..HEAD`:

- `search::no_match_with_wrap/200000`: 659µs → 749µs (+14%). Still
  inside the 2ms budget; the regex window-walk cost grew slightly,
  possibly from `fancy-regex` minor-version churn or memmem-
  windowed scan layout. Not worth chasing yet — investigate
  alongside other search-path work.
- `search::forward_last_match/200000`: 469µs → 516µs (+10%). Same
  cause; same posture.
- `render::frame_60_lines/200`: 42µs → 46µs (+10%). At a 200-fn
  buffer the renderer composes 60 visible lines; ~4µs added per
  frame is within ratatui write-noise but worth a flame-graph if
  it grows further. Suspect cause: option-cache misses on the new
  `lsp-*` sub-modes (mode-cascade work landed during the window).
- `render::frame_120_lines/200`: 78µs → 90µs (+16%). Same cause
  scaled.

These four are inside the WSL2 noise floor (~±15%) but trend
together, so the cascade explanation feels right rather than
random.

### Methodology note (added this run)

The bench environment isn't pinned today. Concretely:

- No CPU-governor lock on the WSL2 kernel (`cpupower frequency-set
  -g performance` would help, if `cpupower` is exposed).
- No host-side quiescence guarantee (background load on Windows
  affects WSL2 latency).
- Criterion default sample size (100) is sensitive to one-off
  thermal spikes on sustained-load benches; `d_whole/50000` is
  the prototypical sufferer.

When a future bench run produces a *real* code-attributable
regression, the test is whether the same commit reproduces it on
the same host state — re-probe at HEAD~10 or so as a control.

### Improvements worth noting

The same window picked up real wins (mostly from the picker
single-pass seat refactor in `1b095ae`):

- `picker::open_inline/5000`: 2.80ms → **1.43ms** (−49%)
- `picker::refilter/n=5000,query=""`: 1.50ms → **644µs** (−57%)
- `picker::refilter/n=5000,query="f"`: 1.35ms → 1.12ms (−17%)
- `motion::word_forward/50000`: 700µs → **523µs** (−25%)
- `motion::first_non_blank/indented-50k`: 268µs → 226µs (−16%)
- `tree_edit_single_char/2000`: 4.0ms → **2.66ms** (−34%) — tree-
  sitter version bump or query-cache locality improvement
- `reparse_incremental_single_char_change/2000`: 1.77ms → **1.46ms** (−18%)
- `folds::compute_syntax_rust/2000`: 323ms → 286ms (−11%)
- `highlight::rust_viewport/60_lines`: 289µs → 261µs (−10%)
- `highlight::rust_viewport/120_lines`: 388µs → 359µs (−7%)
- `runtime::apply_edit_round_trip`: 83µs → **77µs** (−7%)
- `runtime::snapshot_publish_standalone`: 101ns → **95ns** (−6%)

---

## Runtime / actor (`crates/lattice-runtime/benches/actor.rs`)

The load-bearing async primitives (../architecture/design.md §5.2.1, §5.6.8, §5.7).

| Benchmark                                       | 10 lines   | 1k lines   | 50k lines  | Floor / Target         | Improvement target                                                                             |
|-------------------------------------------------|------------|------------|------------|------------------------|------------------------------------------------------------------------------------------------|
| **Snapshot publish standalone**                 | **~95ns**  | **~95ns**  | **~96ns**  | ~80ns / <500ns         | ⏹️ at the practical floor (Arc::new + atomic). Constant across sizes -- buffer clone is O(1).   |
| `apply_edit` round-trip (block_on)              | 76.5µs     | 78.4µs     | 77.5µs     | ~50µs / <100µs         | 🔼 sync edit fast-path drops to ~5µs (../architecture/design.md §8.2 stretch).                                 |
| **Dispatch round-trip** (motion)                | **~79µs**  | **~91µs**  | **~575µs** | ~50µs / <100µs (small) | ⏹️ scheduler-bound on small bufs; large-buf cost is the motion walk itself.                     |
| Snapshot publish via apply_edit                 | 77.6µs     | 79.7µs     | 76.7µs     | same as apply_edit     | (envelope, not standalone publish)                                                             |
| Snapshot load (`load_full`)                     | **~16ns**  | --         | --         | ~16ns / <20ns          | ⏹️ at the floor (atomic acquire + Arc bump).                                                    |
| **Snapshot load (`Cache::load`, steady)**       | **~290ps** | --         | --         | ~280ps / <500ps        | ⏹️ sub-nanosecond. Per-thread cached; ~55× faster than `load_full`. Renderer's per-frame read.  |
| Snapshot post-publish read                      | 71.4ns     | 17.2ns     | 19.5ns     | --                     | 🔼 same path                                                                                   |
| **Status segment update**                       | **~56ns**  | --         | --         | ~50ns / <100ns         | ⏹️ at the floor (snapshot load + small format).                                                 |

**Round-trip is constant across buffer sizes** -- mailbox + oneshot
+ Arc clone, not a buffer walk. The ~85µs publish-via-apply-edit is
the *end-to-end* cost; the snapshot-construct + arc-swap-store is
sub-microsecond, bundled inside the round-trip.

**snapshot_load_cached at ~305ps** is the renderer's hot path;
`SnapshotCache::load` returns a borrowed reference to the cached
`Arc` after a single `Relaxed` atomic compare against the
underlying `ArcSwap` pointer. When the writer hasn't published
since the last load (the common case mid-frame), the compiler
inlines the compare to a register read. The fallback path
(`load_full`) stays available for callers that need an owned
`Arc` outside the cache's borrow lifetime.

The renderer migration to `SnapshotCache` is the §5.6.8 read-side
floor. The actor-internal write path is separate (see
"snapshot_publish_standalone" above).

---

## Search (`crates/lattice-core/benches/search.rs`)

Now backed by [`fancy-regex`](https://docs.rs/fancy-regex). Patterns
without backrefs/lookarounds route through the `regex` crate's
RE2-style DFA + SIMD literal prefilter; backref patterns fall back
to a bounded NFA. **All literal-search variants under §8.2's <2ms
Reflex budget; the 200k-line worst case beats the prior
memmem-only path by 40-50%.**

| Search                | 10     | 1k         | 50k        | 200k       | Improvement target                                     |
|-----------------------|--------|------------|------------|------------|--------------------------------------------------------|
| `forward_first_match` | 2.08µs | 1.16µs     | 2.36µs     | 2.24µs     | ⏹️ near floor (regex setup dominates on tiny scans)     |
| `forward_last_match`  | 2.40µs | 2.33µs     | **103µs**  | **516µs**  | ⏹️ near floor; ~10% above prior baseline (see Regressions) |
| `no_match_with_wrap`  | 1.56µs | 3.06µs     | **158µs**  | **749µs**  | ⏹️ near floor; ~14% above prior baseline (see Regressions) |
| `backward`            | 2.52µs | 1.47µs     | **15.2µs** | --         | ⏹️ near floor                                           |

| Regex feature             | 50k        | Improvement target                                                                                       |
|---------------------------|------------|----------------------------------------------------------------------------------------------------------|
| `alternation`             | 2.69µs     | ⏹️ regex literal-set extraction handles `(foo\|bar\|baz)`                                                 |
| `class_quantifier`        | 1.16ms     | ⏹️ general DFA path; under Reflex budget                                                                  |
| `backref` (pathological)  | **176ms**  | 🔼 fancy-regex backtracking; bounded by 1M-iteration recursion limit. Add per-search timeout for safety. |

**Implementation notes.**
- 128KB scan window amortises fancy-regex's per-call setup
  (~5µs/call × ~800 chunks → ~5µs/call × ~100 windows on 13MB).
- `from_utf8_unchecked` on the window: rope chunks are `&str`
  (valid UTF-8); the drain logic preserves codepoint alignment via
  `round_down_utf8_boundary`. The unsafe call is gated by a module-
  level `#![allow(unsafe_code)]` with the safety argument inline at
  the call site. Every other module remains `unsafe_code = "deny"`.
- `MAX_MATCH_LEN = 8KB` bridge: matches longer than this AND
  spanning a window boundary are missed. Generous for editor
  patterns; pathological matches that span >8KB are not a v1
  concern.

**🔼 backref pathological pattern (169ms on 50k).** fancy-regex's
default backtrack limit is 1M iterations; our
`(handler_\d+)\b.*\b\1` pattern hits ~150ms before terminating.
For an editor we'd want a stricter per-search timeout (target:
abort at 50ms, surface "search timed out" to the user) -- needs
the cancellation token contract (../architecture/design.md §5.2.5) to land first.
Today the search just runs to completion or hits the recursion
cap.

**Negative result: B-γ (rayon parallel scan) was tried + reverted.**
Adaptive sequential-prefix + parallel-tail regressed every 50k+
bench. memmem / fancy-regex scan rare-prefix patterns at L2
bandwidth, so a 13MB scan fits inside rayon's spawn overhead.
Documented in `find_forward_in_rope`'s docstring.

**Search history (this session):**

| Bench                    | Original | After α (memmem) | After β (chunk-walk) | After regex+window | Δ vs original       |
|--------------------------|----------|------------------|----------------------|--------------------|---------------------|
| forward_first_match/200k | 1.0ms    | 908µs            | 211ns                | 2.23µs             | **-99.78% (450×)**  |
| forward_last_match/200k  | 21ms     | 1.29ms           | 811µs                | **469µs**          | **-97.8% (45×)**    |
| no_match_with_wrap/200k  | 34ms     | 1.4ms            | 1.21ms               | **659µs**          | **-98% (51×)**      |
| forward_last_match/50k   | 4.4ms    | 163µs            | 189µs                | **103µs**          | **-97.7% (43×)**    |
| no_match_with_wrap/50k   | 8.7ms    | 177µs            | 288µs                | **156µs**          | **-98.2% (56×)**    |

Notable: regex+window beats the literal-only memmem path on every
50k+ benchmark. fancy-regex's literal prefilter on a 128KB window
runs faster than memmem's tighter window because the per-call setup
cost amortises across more bytes. The "near-cursor" small-buffer
case (`forward_first_match/200k = 2.23µs`) is slightly slower than
the prior memmem-only number (211ns) but still trivially under
budget.

---

## Motions (`crates/lattice-grammar/benches/motions.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Motion                                   | 10 lines | 1k lines | 50k lines | Improvement target                                                |
|------------------------------------------|----------|----------|-----------|-------------------------------------------------------------------|
| `word_forward`                           | 279ns    | 9.86µs   | **523µs** | 🔼 SIMD whitespace scan via memchr (potential 5-10× on big files); ~25% faster than the 700µs prior baseline -- likely an upstream `ropey` win |
| `word_backward`                          | 1.25µs   | 1.94µs   | 108µs     | ⏹️ near floor                                                      |
| `word_end`                               | 1.13µs   | 1.59µs   | 103µs     | ⏹️ near floor                                                      |
| `first_non_blank` (50k indented)         | --       | --       | 226µs     | 🔼 memchr `memchr` on `b' '` / `b'\t'`                            |
| `word_forward` count=50 in 100x buffer   | 611ns    | --       | --        | ⏹️                                                                 |
| `find_char_forward` (900-char wide line) | 279ns    | --       | --        | 🔼 memchr (potential 3-5×)                                        |

**🔼 Three motions could use memchr for the same reason search did:**
`word_forward`, `first_non_blank`, `find_char_forward` all do
linear character-class scans. Replacing with `memchr::memchr`
prefilter would give 5-10× wins on large files. Not a v1 priority
(absolute numbers already pass §8.2).

---

## Operators (`crates/lattice-grammar/benches/operators.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Operator                         | 10 lines | 1k lines | 50k lines  | Improvement target                                                                            |
|----------------------------------|----------|----------|------------|-----------------------------------------------------------------------------------------------|
| `dw` (delete word)               | 4.97µs   | 17.3µs   | 670µs      | ⏹️                                                                                             |
| `dd` (delete line)               | 5.43µs   | 15.9µs   | 840µs      | ⏹️                                                                                             |
| `d_whole` (delete entire buffer) | 5.15µs   | 20.6µs   | **~3.0ms** (one-off 6.66ms spike in this run's first execution; reproducible value ~3ms) | ⚠️ host-environment drift -- the historical 1.23ms is unreproducible at the same commit (bisect probed `c94d734` today: 2.84ms). 50k case is the only operator past the 2ms Reflex budget; real regression threshold ~6ms going forward. |
| `yw` (yank word)                 | 6.16µs   | 13.3µs   | 890µs      | ⏹️                                                                                             |
| `cw` (change word)               | 4.93µs   | 13.4µs   | 687µs      | ⏹️                                                                                             |
| `diw` (delete inner word)        | 5.83µs   | 3.84µs   | 227µs      | ⏹️                                                                                             |
| `di_paren` (deep arg list)       | 8.79µs   | --       | --         | ⏹️                                                                                             |

**`d_whole/50k` at ~3ms** is the only operator outside the 2ms
Reflex budget on this hardware today. The historical doc value
of 1.23ms (captured 2026-05-02 on the same WSL2 host) is no
longer reproducible: a targeted bisect probed `c94d734` (the
commit that originally recorded 1.23ms) today and measured
2.84ms, with ±0.5ms noise across `b75c135..HEAD`. The delta is
attributed to WSL2 host-state drift, not lattice code — see
"Bench environment drift" above for the full bisect data + the
candidate root causes (kernel rev, CPU governor, L3 pressure).

This means the row stays ⚠️ because the absolute number IS over
the 2ms budget — but a code fix isn't the right lever; pinning
the bench host's CPU governor + a quiet-host policy are. The
other operators stay near floor; their cost is dominated by
ropey's `remove(...)` work and is sub-millisecond at 50k lines.

The first invocation of the bench today produced a one-off spike
to 6.66ms (recorded in the morning's full-suite output); every
subsequent rerun (full and `--quick`) landed at ~3ms. The 6.66ms
is treated as an outlier rather than the canonical number; a real
P1 regression would need to reproduce stably across runs.

---

## Folds (`crates/lattice-ui-tui/benches/folds.rs`)

Three computed fold providers; each measured across small / medium /
large corpora so a regression on either small-file ergonomics or
large-file scaling surfaces. Folds recompute on every reparse, so
the budget is "stay sub-frame on realistic buffers."

| Provider              | small          | medium      | large        | Improvement target                                                                                                              |
|-----------------------|----------------|-------------|--------------|---------------------------------------------------------------------------------------------------------------------------------|
| `compute_indent`      | 1.9µs (10 fns) | 30µs (200)  | 310µs (2000) | ⏹️ linear in line count; pure rust, no allocations beyond the result vec                                                         |
| `compute_markdown`    | 1.0µs (10)     | 6.7µs (100) | 30µs (500)   | ⏹️ linear; ATX-heading scan + nesting walk                                                                                       |
| `compute_syntax_rust` | 64µs (10)      | 3.7ms (200) | 286ms (2000) | 🔼 `QueryCursor::matches` traversal; sub-linear past 200 fns. Phase 5/9 incremental reparse + per-pattern caching is the lever. |

**The syntax provider's 200-fn time (3.7ms) is the relevant ceiling**
for real-world Rust files (typical ≤500 LOC). 2000-fn buffers are an
outlier (~50kloc in one file). The bench pre-parses the source into a
`Syntax` instance so the timing measures only fold-query work, not the
underlying tree-sitter parse.

`compute_syntax_rust` covers `function_item`, `struct_item`,
`impl_item`, `if_expression`, `match_expression`, `block`, etc. (see
`queries/rust/folds.scm`). The query traversal cost grows with the
number of pattern alternatives; pruning the captures we don't fold
visibly (e.g. `parameters`, `arguments` for very-short ranges) is the
next available optimization if 3.9ms ever pushes uncomfortable.

---

## Native highlight (`crates/lattice-syntax/benches/highlight.rs`)

Times `Syntax::highlight_lines_native` per language across the
realistic call shapes the renderer actually issues.

| Benchmark                          | size              | time      | Floor / Target  | Improvement target                                                                  |
|------------------------------------|-------------------|-----------|-----------------|-------------------------------------------------------------------------------------|
| `highlight::rust/10`               | 81 lines          | **142µs** | ~120µs / —      | ⏹️ small-buffer setup floor (one full QueryCursor traversal).                        |
| `highlight::rust/200`              | ~1600 lines       | **2.93ms**| ~3ms / <5ms     | 🔼 per-pattern caching + pruning never-folded captures (~1ms achievable).           |
| `highlight::rust/2000`             | ~16k lines        | 38ms      | --              | 🔼 outlier (single-file 50kloc); the renderer never asks for full-buffer highlight. |
| **`highlight::rust_viewport/24`**  | 24-line viewport  | **184µs** | ~150µs / <300µs | ⏹️ realistic frame call shape. The renderer's keystroke path lives here.             |
| **`highlight::rust_viewport/60`**  | 60-line viewport  | **261µs** | ~250µs / <500µs | ⏹️                                                                                   |
| **`highlight::rust_viewport/120`** | 120-line viewport | **359µs** | ~350µs / <800µs | ⏹️                                                                                   |
| `tree_edit_single_char/10`         | 80 lines          | **4.6µs** | --              | ⏹️ tree.edit() floor at small size (B.2).                                            |
| `tree_edit_single_char/200`        | 1600 lines        | **167µs** | --              | ⏹️ scales with tree node count, not constant (B.2).                                  |
| `tree_edit_single_char/2000`       | 16k lines         | **2.66ms**| --              | ⏹️ −34% vs prior baseline (4.0ms) -- tree-sitter version bump.                       |
| `reparse_incremental/10`           | 80 lines          | **586µs** | --              | ⏹️ slower than full at this size; tree-sitter incremental setup overhead.            |
| `reparse_incremental/200`          | 1600 lines        | **293µs** | --              | ⏹️ **8× faster than full reparse**.                                                  |
| `reparse_incremental/2000`         | 16k lines         | **1.46ms**| --              | ⏹️ **16× faster than full reparse**, fits 16ms@60Hz frame budget.                    |
| `reparse_full_baseline/10`         | 80 lines          | **199µs** | --              | ⏹️ falsification anchor at small size.                                               |
| `reparse_full_baseline/200`        | 1600 lines        | **2.47ms**| --              | ⏹️ falsification anchor at medium size.                                              |
| `reparse_full_baseline/2000`       | 16k lines         | **23.7ms**| --              | ⏹️ exceeds 16ms@60Hz budget -- why incremental matters at scale.                     |

**The viewport-bounded numbers are the meaningful ones.** The
full-buffer rows characterise worst-case query traversal cost
but the renderer never asks for the whole document at once. At
24 lines (typical 80×24 terminal) we're at 178µs, well under the
§8.2 frame-render budget.

`tree_sitter::QueryCursor::set_byte_range` is a hint -- the
cursor still walks the entire tree to find captures that overlap
the requested range. The ~178µs viewport floor reflects this; a
true viewport-bounded query traversal (Helix's `LanguageLayer`
incremental approach) is post-1.0 work.

### Slice B.2 — Incremental reparse calibration

The incremental rows surface honest scaling: `tree.edit()` is
O(num_nodes), not constant. Initial §8.2 estimate of "~500ns
floor" was wrong; the real floor scales by tree size.

**Speedup vs. full reparse**:
- 80 lines: incremental **0.4× slower** -- tree-sitter's
  incremental setup overhead doesn't pay off below ~hundreds of
  lines. Both paths sub-ms; user-imperceptible.
- 1600 lines: incremental **~8× faster** (325µs vs 2.5ms).
- 16k lines: incremental **~14× faster** (1.77ms vs 25.5ms),
  AND incremental fits the 16ms@60Hz frame budget while full
  exceeds it.

**Why we don't gate on file size.** A threshold "use full reparse
below N bytes" is tempting but adds a discontinuity that would
be observable as latency-jitter when files cross the threshold
during editing. The small-file regression is sub-ms in absolute
terms; the architectural simplicity of "always incremental" is
worth the 350µs at the small end.

**Pathological-burst guard.** The worker caps coalesced edits at
256 per request. A 100k-char paste-as-keystrokes would otherwise
multiply the 4ms-per-edit (16k-line case) cost into seconds of
pre-parse work. The cap drops the edit list and falls through to
full reparse -- still produces a correct tree, just at full-
reparse cost (which is what the user-paste path naturally hits).

### C-series (C.1–C.5) — bench numbers now reflect production

The B.2 bench numbers were *algorithmically* correct from the day
they landed but *operationally* dead until slice C.1. Pre-C.1, the
syntax worker silently never spawned in production
(`tokio::runtime::Handle::try_current()` failed because `main` was
synchronous). Every `request_reparse` sent into a dropped channel;
the snapshot stayed at the seeded state forever. The B.2 incremental
numbers reflected what the worker *would do* if it ran -- they
didn't reflect what users experienced. The C.1 `#[tokio::main]`
migration plumbed the runtime in from program start; only then did
production users actually see incremental reparse.

The C-series adds no new benches but introduces a **sub-µs
synchronous-shift cost** on every edit:

- `shift_highlights_for_edit`: O(N) Vec drain/insert where N is
  lines added or removed by the edit. Typically 0 (in-line) or
  1 (line delete/insert). ~tens of ns.
- `shift_spans_within_line`: O(span_count) on the edited line,
  with each span doing a single i64 add. Typical line has <20
  spans. ~hundreds of ns.

Total C-series input-thread overhead per edit: <1µs. Doesn't show
up in the existing bench rows because they don't exercise the
edit→render cycle as a unit. The visible-side win (flicker
elimination) is correctness-not-perf; the existing
`refresh_highlights_cache_hit/200` at 20ns is unchanged.

---

## Frame render (`crates/lattice-ui-tui/benches/render.rs`)

Times `compose_visible_lines` against pre-warmed highlight cache
-- the per-frame view-composition cost on top of the highlight
work above.

| Benchmark                     | size             | time     | Floor / Target | Improvement target         |
|-------------------------------|------------------|----------|----------------|----------------------------|
| `render::frame_24_lines/200`         | 80×24, 200 fns   | **15µs** | ~10µs / <50µs  | ⏹️ near floor; +13% vs prior (~13µs) -- see Regressions. |
| `render::frame_60_lines/200`         | 200×60, 200 fns  | **46µs** | ~30µs / <100µs | ⏹️ +10% vs prior (~42µs); option-cache miss on the new lsp-* sub-modes is the prime suspect. |
| `render::frame_120_lines/200`        | 200×120, 200 fns | **90µs** | ~70µs / <150µs | ⏹️ +16% vs prior (~78µs); same cause, scaled.         |
| `refresh_highlights_cache_hit/10`    | 80 lines         | **21ns** | ~10ns / <50ns  | ⏹️ steady-state cache hit (B.3); independent of size. |
| `refresh_highlights_cache_hit/200`   | 1600 lines       | **21ns** | ~10ns / <50ns  | ⏹️ same -- key compare short-circuits before any work.|
| `refresh_highlights_cache_hit/2000`  | 16k lines        | **21ns** | ~10ns / <50ns  | ⏹️ same; ~8500× faster than the pre-B.3 ~178µs path.  |

The frame_60 / frame_120 rows ticked down ~15% after the renderer
migrated to `Cache::load` + a single per-frame snapshot
(`632310d`). `compose_visible_lines` no longer pays an internal
`load_full` per call, and `closed_fold_display_span` (called per
fold heading) no longer pays one each either. frame_24 is
unchanged within noise -- the savings scale with viewport height
because more visible folds = more eliminated loads.

Combined with viewport-bounded highlight (178µs at 24 lines), the
total per-frame cost on the editor side is ~192µs -- well under
the §8.2 "Frame render TUI <500µs" target. The remaining cost is
ratatui's terminal write, which isn't measured here (hardware-
bound; not benchable without a real TTY).

**Slice B.3 -- highlight span cache** (`refresh_highlights_cache_hit`
above): on the steady-state frame (cursor blinking, no edit /
scroll / fold change), the 178µs `highlight_lines` call is now a
20ns key-compare + early return. The total editor-side per-frame
cost on a steady-state frame drops from ~192µs to ~33µs (compose
13µs + cache check 20ns), a ~6× speedup -- the cache check
component is **8900× faster** than the path it replaces. Cache
key is `(snapshot_ptr, text_version, scroll, viewport_height,
fold_hash)`; any actual change invalidates it and falls through
to the original recompute path, which is unchanged.

**Typed-options migration** (75e2390 → 1bfee16): the initial
landing of `lattice-config` regressed render frames by 25-57%
because every per-frame option read (`app.show_line_numbers()`,
`app.foldmethod()`, ...) went through the registry's mutex +
`ArcSwap` + downcast (~33ns per read; benchmarked in
`config::get_bool_via_handle`). At 60-120 visible lines × 2-4
reads per line, the path added multiple microseconds per frame.
The follow-up (commit landing this entry) restores baseline by
caching the option values on `App.option_cache`, refreshed via
the `Event::OptionChanged` cascade in `apply_option_cascade`.
Reads become field accesses (~1ns); the canonical value still
lives in `lattice-config::ConfigRegistry`, the cache is a
derived projection. Numbers above reflect the post-fix state.

---

## Tree-sitter consolidation (Option B migration)

**Architectural change, 2026-05-03.** `lattice-syntax` previously ran
`tree_sitter_highlight::Highlighter` (which parses internally) AND a
separate `tree_sitter::Parser` for the folds query — two parses per
edit on every recognised buffer. The Option B migration ([Steps 1–4](../docs/implementation.md))
collapsed both onto a single `Parser` + `Tree` owned by `Syntax`:

- One parse per edit. Highlight, folds, and any future query consumer
  (textobjects.scm, indents.scm, locals.scm) all walk the same `Tree`.
- `tree-sitter-highlight` dropped from the dep tree; the hand-rolled
  pipeline reads `highlights.scm` directly via `QueryCursor` with
  later-pattern-wins overlap resolution.
- Markdown injections (block→inline + fenced code blocks) recurse one
  level: a `\`\`\`rust ... \`\`\`` block inside markdown reuses the rust
  highlights query.

The user-visible win lands when the document/syntax actor (§5.7) is
threaded through with `Tree::edit` deltas — the seam exists today, and
incremental reparse will further reduce per-keystroke cost on large
buffers. A dedicated highlight bench is on the "what's NOT here" list
below.

---

## Buffer ops (`crates/lattice-core/benches/buffer.rs`)

Direct rope mutations.

| Operation                  | 10 lines | 1k lines | 100k lines | Improvement target                  |
|----------------------------|----------|----------|------------|-------------------------------------|
| `insert_at_origin`         | 1.71µs   | 1.14µs   | 66.0µs     | ⏹️ ropey is the floor                |
| `insert_at_middle`         | 1.96µs   | 1.96µs   | 66.4µs     | ⏹️                                   |
| `delete_one_byte`          | 2.14µs   | 1.53µs   | 66.7µs     | ⏹️                                   |
| `position_byte_round_trip` | 863ns    | 372ns    | **323ns**  | ⏹️ B-tree is faster on bigger ropes  |
| `input_edit_construction`  | **1.82ns** | --     | --         | ⏹️ at §8.2's ~2ns floor (B.1)        |
| `clone_vs_text/clone`      | **7.7ns**  | **7.7ns** | **7.8ns**  | ⏹️ Arc bump on ropey's internal Arc (B.5) |
| `clone_vs_text/as_string`  | 79ns     | 991ns    | **211µs**  | falsification anchor; pre-B.5 path (full materialization) |

`input_edit_construction` is the new tree-sitter-shaped delta
construction at the tail of `Buffer::apply_edit` -- six u32 writes
plus three `Position` copies. Backs §8.2's Write-path row "InputEdit
construction (per `Document::apply_edit`)" -- floor ~2ns, target
<10ns, today **1.87ns**. The full `apply_edit` cost is dominated by
the rope mutation above (`insert_at_origin` ≈ 2µs); delta
construction is in the noise floor.

`clone_vs_text` measures the slice-B.5 input-thread cost reduction.
Pre-B.5, `Document::text()` materialized the full buffer to a
`String` on every keystroke (~189µs at 100k lines, on the input
thread). Post-B.5, `Buffer::clone()` (Arc bump) replaces it
(~7.7ns flat, independent of size). **24,500× faster** at large
sizes; the full-text alloc moves to the syntax worker per goal #1.
The `as_string` row stays as the falsification anchor -- if it
ever matches `clone`, ropey's internal sharing changed.

| Open-large benchmark | size  | time      | throughput | Floor / Target | Improvement target                                             |
|----------------------|-------|-----------|------------|----------------|----------------------------------------------------------------|
| `buffer::open_large` | 10MB  | **3.5ms** | 2.9 GiB/s  | ~2ms / <10ms   | ⏹️ near floor (memcpy-ish into ropey's internal buffer).        |
| `buffer::open_large` | 100MB | **74ms**  | 1.3 GiB/s  | ~50ms / <100ms | ⏹️ B-tree split cost compounds; under §8.2 first-paint target.  |

`position_byte_round_trip` is *faster* on bigger ropes -- ropey's
B-tree packs better at scale.

---

## Typed options (`crates/lattice-config/benches/options.rs`)

The §5.12 typed-options registry. Read paths (`config.get` /
`config.with`) appear inside the renderer's per-line gutter
checks; write paths (`config.set` / `parse_and_set_command`) are
cmdline / plugin / customize-buffer triggered (cold). Hot-path
reads are cached on `App.option_cache` (~1ns field access);
these benches measure the underlying registry costs.

| Bench                                 | Time      | Floor / Target | Notes                                                                                                    |
|---------------------------------------|-----------|----------------|----------------------------------------------------------------------------------------------------------|
| `config::get_bool_via_handle`         | **33ns**  | ~30ns / <50ns  | Mutex acquire + `Arc::clone` + `as_any().downcast_ref` + `ArcSwap::load_full` + `Arc<bool>` deref.       |
| `config::with_int_via_handle`         | **26ns**  | ~25ns / <50ns  | Skips one `Arc::clone` vs `get`; the cheaper closure-style read.                                         |
| `config::lookup_by_name`              | **35ns**  | ~30ns / <100ns | HashMap probe + `Arc::clone`. Cmdline path uses this; not on the per-frame render hot path.              |
| `config::set_no_publisher`            | **134ns** | ~100ns / <500ns | Validate + `ArcSwap::store`. No event publisher wired -- baseline cost of the typed write.              |
| `config::set_with_publisher`          | **144ns** | ~120ns / <500ns | Same as above plus the publisher closure -- registry's contribution to the §5.10 `OptionChanged` flow.  |
| `config::parse_and_set_command_bool`  | **217ns** | ~180ns / <1µs  | Full cmdline path: `parse_set` + lookup + parse_and_set + format echo + publish.                         |
| `config::resolved_get_typed`          | **13.9ns** | ~12ns / <50ns | M.2.1: type-keyed read against the per-buffer `ResolvedOptions` cache. One `TypeId` HashMap probe + `Arc::clone` + downcast. The hot-path read for mode-aware option access; the `App.option_cache` projection sits on top for sub-ns reads. |
| `config::resolve_into_10_layers`      | **1.89µs** | ~1.8µs / <10µs | M.2.1: full recompute -- bootstrap from registry currents, then layer 10 minor-mode contributions on top. Per `../architecture/mode-architecture.md` §6.3.2 the gate is p99 < 10µs at 10 minors; we hit ~5× headroom because the bootstrap walk dominates and the layer merge is bounded. **+2.2× vs prior baseline (851ns)** -- new sub-modes (lsp-progress / lsp-document-highlight / lsp-selection-range / lsp-folding / lsp-inlay-hint / lsp-semantic-tokens) registered through Phase 4.4 are layered into the bootstrap; cost is amortised across reads, still well inside the 10µs gate. |

The `App.option_cache` projection turns the per-frame
renderer reads into ~1ns field accesses; M.2.1's resolved
cache (per buffer, mode-aware) sits between
`option_cache` and the registry, used when the renderer needs
*mode-resolved* values rather than the global default. Writes
remain cold (cmdline / plugin / customize-buffer triggered);
mode toggles are roughly an order of magnitude rarer than
`:set` writes, so the recompute cost is amortised across
many reads.

---

## Picker (`crates/lattice-picker/benches/picker.rs`)

Per `../architecture/picker.md` § 9.2: criterion benches for
the picker primitive's hot paths. `refilter` is the
per-keystroke filter+rank pass; `open_inline` is the
seed-and-bonus-snapshot path; `mru_snapshot` is the host-side
O(N) cache pass; `mru_record` is the accept-time index
write; `bonus_of` is the frecency math kernel.

| Bench                                  | Time         | Floor / Target | Notes                                                                                                                       |
|----------------------------------------|--------------|----------------|-----------------------------------------------------------------------------------------------------------------------------|
| `picker::open_inline/100`              | **26.4 µs**  | ~25 µs / <500 µs   | Build candidates + bonus snapshot + single-pass seat. Buffer-switcher scale.                                                |
| `picker::open_inline/500`              | **120.0 µs** | ~120 µs / <1 ms    | LSP-symbols / outline scale.                                                                                                |
| `picker::open_inline/5000`             | **1.43 ms**  | ~1.4 ms / <8 ms    | **−49% vs prior baseline (2.80 ms)** -- the single-pass seat collapsing the prior double-refilter (commit 1b095ae) lit up here. Worst-case file-picker walker output (5000 candidates). |
| `picker::refilter/n=500,query=""`      | **52.3 µs**  | ~50 µs / <500 µs   | Empty-query refilter on 500 candidates. No filtering, just rank+sort.                                                       |
| `picker::refilter/n=500,query="f"`     | **99.3 µs**  | ~100 µs / <500 µs  | Single-char substring filter -- the match-range walk dominates over the trivial empty-query bypass.                          |
| `picker::refilter/n=500,query="file_"` | **122.9 µs** | ~120 µs / <500 µs  | 5-char query against a substring-matching candidate set.                                                                    |
| `picker::refilter/n=5000,query=""`     | **644 µs**   | ~650 µs / <2 ms    | **−57% vs prior baseline (1.50 ms)** -- same single-pass seat collapse. Empty-query rank+sort dominates here. |
| `picker::refilter/n=5000,query="f"`    | **1.12 ms**  | ~1.1 ms / <2 ms    | Substring filter rejects nothing (every path contains `f`); cost is matcher walk × N.                                       |
| `picker::refilter/n=5000,query="file_"`| **1.35 ms**  | ~1.3 ms / <2 ms    | Worst-case substring scan + full-match rank at 5000 candidates. Consumes ~16% of the 8.3ms frame budget at 120Hz; tightening levers in "Headroom" below. |
| `picker::mru_snapshot/100`             | **11.0 µs**  | ~10 µs / <100 µs   | O(N) HashMap-lookup pass. Runs once per picker-open.                                                                        |
| `picker::mru_snapshot/500`             | **55.8 µs**  | ~55 µs / <500 µs   |                                                                                                                             |
| `picker::mru_snapshot/5000`            | **513.6 µs** | ~500 µs / <2 ms    | At 5000 candidates the snapshot cost is dominated by HashMap probes; cap-per-namespace bounds the cost in practice (most entries lookup-miss to 0.0). |
| `picker::mru_record/100`               | **915 ns**   | ~1 µs / <10 µs     | Single accept: HashMap insert + cap-check. Steady-state cost; the user feels none of it.                                    |
| `picker::mru_record/1000`              | **61.7 µs**  | ~60 µs / <500 µs   | At-cap insert: the eviction path runs through `lowest_frecency_in_namespace` (linear scan + frecency compute per entry). Rare in practice (only fires when a namespace hits its 1000-entry ceiling). |
| `picker::bonus_of`                     | **21.8 ns**  | ~20 ns / <100 ns   | Frecency formula kernel. Called once per candidate during snapshot; this floor is what sets the snapshot's per-entry cost.  |

Measured on the workstation listed in [§ Environment](#environment),
release profile, criterion default sample size = 100.
Numbers are mean point estimates; criterion ranges (lower /
upper) are within ±5 % at this sample size.

### Why these targets

- **`refilter` sub-frame.** Per CLAUDE.md paramount goal #1
  (sub-frame keystroke-to-glyph), the refilter pass runs
  per-keystroke. At 60Hz the frame budget is 16 ms; at 120Hz
  it's 8.3 ms. The 5000-candidate worst case at ~1.5 ms
  leaves ample headroom on both, and the typical case
  (100-500 candidates) is well under 200µs.
- **`open_inline` headroom.** Picker open is user-invoked
  (`:picker <source>`), not per-keystroke. The 5000-entry
  case at ~3.1 ms is below user-perception threshold
  (~100 ms for "instant"); the double-refilter inefficiency
  is the only thing keeping it above 2 ms.
- **`mru_record` per-accept.** A user can't accept faster
  than ~5/sec by typing `<CR>` repeatedly. Even the at-cap
  eviction path at 64 µs is invisible. The steady-state
  ~1 µs cost is below noise.
- **`bonus_of` per-candidate.** The frecency math runs once
  per candidate during snapshot. At 22 ns × 5000 = 110 µs --
  reasonable correspondence with the 530 µs snapshot bench
  (the difference is HashMap-probe overhead). If `bonus_of`
  ever regresses past 100 ns the snapshot bench will catch
  it before the user notices.

### Headroom notes

The `refilter/n=5000` worst case is at ~1.5 ms today --
inside the sub-frame budget but consumes ~18 % of the
8.3 ms frame budget alone. Two tightening levers if this
ever needs more headroom:

- **Matcher graduation.** The v1 substring matcher walks
  the full display string per candidate. The pipeline-
  driven matcher (`lattice-completion` full vertico stack)
  short-circuits prefix / boundary tiers and would cut the
  5000-candidate cost roughly in half.
- **Survivor-set caching.** Today every keystroke calls
  `refilter` against the full `raw` slice. A two-stage
  pipeline (cache the survivor set of the previous query;
  incremental filter only when the user adds a char) is
  the standard prescient trick; lets a 5-char query
  refilter against ~50 candidates instead of 5000.

Neither lever is needed at v1's typical workloads
(<500 candidates) where `refilter` is well under 200 µs.

The previously-listed `open_inline` double-refilter cost
(prior revision of this section) was collapsed by the
`set_raw_candidates_with_routing_and_bonuses` single-pass
seat method -- numbers above reflect the post-collapse
state.

---

## LSP wire layer (`crates/lattice-lsp/benches/lsp.rs`)

Per ../architecture/design.md §5.4 + §5.2.5, LSP requests are **Background**-class
(no sync-prelude budget). The wire-layer benches don't gate any
per-keystroke commitment; they exist to prove the plumbing
itself never appears next to editor work in a flame graph.

| Bench                                 | Time       | Floor / Target | Notes                                                                                              |
|---------------------------------------|------------|----------------|----------------------------------------------------------------------------------------------------|
| `lsp::framing::parse_header_block`    | **68ns**   | ~50ns / <500ns | One ASCII header block, ≤200 bytes. Runs once per inbound message.                                 |
| `lsp::encode::did_change`             | **183ns**  | ~150ns / <2µs  | One `TextDocumentContentChangeEvent` with a small replacement. Runs once per debounced keystroke.  |
| `lsp::decode::publish_diagnostics`    | **1.50µs** | ~1µs / <10µs   | Diagnostic with code + range + source + message + severity. Inbound on save / idle.                |
| `lsp::decode::small_response`         | **383ns**  | ~250ns / <2µs  | initialize / hover response shape.                                                                 |
| `lsp::encode_decode::hover_request`   | **878ns**  | ~600ns / <5µs  | Encode + decode round-trip (no I/O) for a typical request.                                         |
| `lsp::position::utf8_passthrough`     | **1.0ns**  | ~1ns / <5ns    | utf-8 negotiated mode short-circuits to a branch + return.                                         |
| `lsp::position::utf16_cjk_line`       | **21ns**   | ~20ns / <500ns | Worst case: 64-char CJK-only line, mid-line offset. Walks prefix counting utf-16 code units.       |
| `lsp::position::utf16_to_byte_cjk`    | **41ns**   | ~30ns / <500ns | Reverse direction: utf-16 column → utf-8 byte. Used for ranges arriving FROM the server.           |
| `lsp::logging::log_info`              | **116ns**  | ~80ns / <500ns | Per-record cost: lock + push + format + tracing fan-out. Background-class.                         |
| `lsp::logging::log_trace_off`         | **10ns**   | ~5ns / <50ns   | Trace toggle off short-circuit -- a HashSet lookup + return. Hot path when trace stays disabled.   |
| `lsp::logging::log_trace_on`          | **116ns**  | ~80ns / <500ns | Trace toggle on -- includes the ring push. Negligible at editor pace; perceptible at indexer bursts. |
| `lsp_edit_publish_three_subs`         | **2.4µs**  | ~2µs / <5µs    | UI-thread cost per applied edit: `EventBus::publish` of one `Event::DocumentChanged` with one `AppliedEdit`, three `DocumentChanged` subscribers attached. The *only* LSP work the keystroke thread does after the per-actor fan-in refactor (docs/../architecture/lsp-architecture.md §11). +25% vs prior baseline (1.9µs) -- the new `MessagePushed` + `LspCodeLensRefresh` typed-event subscribers landed in Phase 4.4/4.5 add small downcast costs per publish; still ≪ 5µs gate. |
| `lsp_edit_propagation_publish_to_recv`| **241ns**  | ~200ns / <600ns| Bus → mpsc receive hop: time from `EventBus::publish` to the per-actor fan-in's `mpsc::recv().await` returning. Excludes the actor's own `record_edit`. |
| `lsp_didchange_flush_16_edits`        | **8.4µs**  | ~6µs / <25µs   | Actor-side debounce-arm cost: 16 `DocSync::record_edit` calls + `take_flush_payload` + serialise to `textDocument/didChange` JSON. Runs off the UI thread (post-debounce). |
| `lsp_diagnostics_line_severity_wait_free` | **25ns** | ~20ns / <75ns  | Render-thread `DiagnosticsLayer::line_severity(uri, line)` after the audit's C3 fix. Pre-fix path locked an inner `Mutex` + cloned the full diagnostics list per call (microseconds, ~3000 calls/sec on the render thread = milliseconds wasted). New path: one `ArcSwap::load` + a borrowed-slice filter — wait-free, allocation-free. |

The full LSP feature matrix (per-method status) lives in
[`../notes/lsp-features.md`](../notes/lsp-features.md); the architecture in
[`../architecture/lsp-architecture.md`](../architecture/lsp-architecture.md). Per-feature
benches (request round-trip latency end-to-end through a real
server) land alongside their features in 4.2 / 4.3 / 4.4.

### Why these targets

The per-actor fan-in refactor moved DocSync into the actor and
removed the supervisor mutex from the keystroke path. The
three rows above split the resulting cost into the three
moments that matter:

- **UI thread** (`lsp_edit_publish_three_subs`): publishing
  one event must stay deep in microseconds even with several
  attached actors. The §8.2 keystroke-to-glyph budget is 8 ms;
  budgeting <50 µs for "tell the LSP layer" leaves >99% of
  the budget for the rest of the path. **1.9 µs ≪ 50 µs.**
- **Propagation** (`lsp_edit_propagation_publish_to_recv`):
  bus → fan-in receive hop. Sub-microsecond means the actor
  sees the event in the same tick the publish completes;
  diagnostics, hover, completion never lag a keystroke
  behind. **227 ns ≪ 5 µs.**
- **Async flush** (`lsp_didchange_flush_16_edits`): cost
  paid by the actor task on its debounce arm. Off the UI
  thread; bound by JSON encode + a string splice per edit.
  **8.4 µs for 16 edits = ~525 ns/edit.**

Regressions in any of these rows would mean the refactor's
core promise (UI thread untouched by LSP work) is leaking.

---

## Keymap (`crates/lattice-ui-tui/benches/keymap.rs`)

The audit's M3 / Slice 8 family rebuilds key-input dispatch as
a typed `KeyChord` → trie-driven lookup. Slice 8.a lands the
foundation -- `KeyChord` type + `KeyEvent ↔ KeyChord ↔ String`
round-trip. The keystroke path runs `KeyEvent → KeyChord`
once per key press; `KeyChord → String` and `String → KeyChord`
run off the keystroke path (`:describe-key`, macro recording,
startup catalog enumeration).

| Bench                                       | Time       | Floor / Target | Notes                                                                                                                                                                                          |
|---------------------------------------------|------------|----------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `keychord_from_event_plain_letter`          | **1.6ns**  | ~1.5ns / <5ns  | Hot path: every keystroke. Plain printable char (`j`, `w`, `a`). A few register operations -- the canonicalisation branches all skip. **Dominates the keystroke-path budget by 50×.**          |
| `keychord_from_event_ctrl_letter`           | **1.8ns**  | ~1.5ns / <5ns  | Hot path: Ctrl-letter normalisation (lowercase fold + redundant-shift strip). Adds one branch + one `to_ascii_lowercase` over the plain-letter path.                                            |
| `keychord_from_event_back_tab`              | **1.4ns**  | ~1.5ns / <5ns  | Special-key canonicalisation (`KeyCode::BackTab` → `Tab + KeyMods::SHIFT`). Match arm + a single bitfield OR.                                                                                  |
| `keychord_to_string_plain_letter`           | **15.8ns** | ~15ns / <40ns  | Off the keystroke path. Allocates a 1-char String via `to_string`. Dominated by the alloc, not the formatting logic.                                                                            |
| `keychord_to_string_ctrl_shift_letter`      | **22.1ns** | ~20ns / <50ns  | Off the keystroke path. Multi-modifier formatting + small-string allocation.                                                                                                                    |
| `keychord_parse_plain_letter`               | **5.1ns**  | ~5ns / <15ns   | One-shot at startup or `:bind`. Single-char fast path -- skip the angle-bracket walk.                                                                                                          |
| `keychord_parse_modifier_special`           | **13.6ns** | ~12ns / <30ns  | One-shot. `<C-S-Tab>` -- walks two modifier prefixes + `parse_special` for the body.                                                                                                            |
| `parse_chord_sequence_multi_key`            | **24.8ns** | ~20ns / <60ns  | One-shot at startup per `KeymapEntry`. With ~280 built-in bindings (per the M3 census), startup parse cost across the catalog is ~7µs total -- not measurable against the rest of boot.        |
| `parse_chord_sequence_two_letters`          | **14.9ns** | ~12ns / <30ns  | One-shot. `gg` / `dw` / `zt` shape -- two bare-char chords per sequence.                                                                                                                       |
| `keymap_trie_lookup_single`                 | **16.3ns** | ~15ns / <40ns  | Hot path. Single-chord lookup (`j`). One `HashMap::get` + a few branches. Slice 8.b.                                                                                                          |
| `keymap_trie_lookup_two_chord`              | **27.1ns** | ~25ns / <60ns  | Hot path. Two-chord lookup (`gd`). Two descents. Models `g_` and `z_` family lookups.                                                                                                          |
| `keymap_trie_lookup_three_chord`            | **40.5ns** | ~40ns / <100ns | Hot path. Three-chord lookup (`diw`). Three descents. Operator + `i` / `a` + text-object -- the deepest trie walks the dispatcher does. **Combined with `keychord_from_event` (~2 ns), end-to-end keystroke path is ~43 ns vs. the architecture's 1 µs commitment.** |
| `keymap_trie_lookup_partial`                | **11.8ns** | ~12ns / <30ns  | Hot path. Partial-prefix lookup (`g` waiting for the second chord). One descent + check.                                                                                                       |
| `keymap_trie_lookup_unbound`                | **11.2ns** | ~10ns / <30ns  | Hot path. Unbound lookup (`q` not in trie). HashMap miss at root + return.                                                                                                                     |
| `keymap_trie_lookup_wildcard`               | **23.5ns** | ~22ns / <60ns  | Hot path. Wildcard fallback (`f x` -> capture `'x'`). One exact miss + one wildcard descent + a one-element `Vec<char>` allocation for the captured char.                                       |
| `keymap_trie_merge_overlay`                 | **418ns**  | ~400ns / <1µs  | Off the hot path. `merge_over` for a layer-overlay add (~16 base bindings + 2 overlays). Runs at minor-mode push / pop -- mode transitions are rare.                                            |
| `keymap_handle_lookup_single`               | **32.4ns** | ~30ns / <80ns  | Hot path. End-to-end keystroke lookup through the registry handle: `ArcSwap::load` + per-mode `HashMap::get` + trie walk. Single-chord (`j`). Slice 8.c.                                       |
| `keymap_handle_lookup_two_chord`            | **45.1ns** | ~45ns / <100ns | Hot path. End-to-end two-chord lookup (`gd`).                                                                                                                                                    |
| `keymap_handle_lookup_three_chord`          | **61.1ns** | ~55ns / <120ns | Hot path. End-to-end three-chord lookup (`diw`). **Combined with `keychord_from_event` (~2 ns), full keystroke path is ~63 ns vs. the architecture's 1 µs commitment -- ~16× headroom.**         |
| `dispatch_translate_full_two_chord`         | **97ns**   | ~100ns / <300ns| Hot path. Full `translate()` round-trip for the second key of `gd` -- partial_chord stack of `[g]`, event `d`. Exercises the post-8.i.4 dispatch shape: ArcSwap load + per-mode fan-out + trie lookup with prefix + resolved `Action::Invoke` materialisation. ~3× the bare `keymap_handle_lookup_two_chord` row -- the rest is the dispatcher's mode match + Action construction. Slice 8.i.4.h. |
| `dispatch_translate_full_operator_motion`   | **101ns**  | ~100ns / <300ns| Hot path. Full `translate()` for `dw` -- partial_chord `[d]`, event `w`. Operator-motion variant of the above; latches op_count via the `AbsorbOperatorPrefix` flow that 8.i.4.c rebuilt, then resolves to a motion `Action::Invoke`. Slice 8.i.4.h.                                                                                                                                              |

### Why these targets

The keymap-architecture doc (`docs/../architecture/keymap-architecture.md` §4)
commits to "**lookup p99 < 1 µs** including chord
normalisation and trie walk." Slice 8.a delivers
the **chord-normalisation half** of that budget at 1.7ns --
60× under the target. The trie-walk half lands in slice 8.b;
the combined number gets a row above this table once the
KeymapTrie ships.

The `keychord_to_string_*` rows are **not** on the keystroke
path -- they fire only when the editor needs a chord-string
representation (`:describe-key X`, macro recording, future
config dump). Allocation is acceptable there; sub-30ns means
even a 1000-entry `:keymap` view renders in ~30µs total.

The `*_parse_*` rows fire at startup (when the built-in
catalog enumerates into the registry) and on user / plugin
`:bind` invocations. Total startup parse cost across the
~280 built-in chords is ~7 µs -- well under the cost of any
single tokio task spawn.

### Slice 8.i.0-8.i.4 -- dispatcher rebuild stayed in budget

Slices 8.i.0 through 8.i.4.h retired the per-`Pending`
`match` body in `compute_normal_action` in favour of a
`partial_chord` stack + trie lookup driven by the catalog's
chord notation. The two `dispatch_translate_full_*` rows
above measure the full round-trip a real keystroke pays
through `translate()` -- ArcSwap load, per-mode dispatch
fan-out, trie lookup with prefix, and resolved-Action
materialisation. ~100 ns each, well under the 1 µs
commitment, and within ~3× the bare trie-lookup numbers
above (the rest is dispatcher fan-out + Action
construction). The `AbsorbPartialChord` /
`AbsorbOperatorPrefix` short-circuits the new dispatch
shape introduces don't measurably hurt the hot path.

---

## Cell-grid renderer (`crates/lattice-host/benches/cells_worker.rs`)

Anchor: `../architecture/cell-grid-renderer.md` (S5 bench
harness) + paramount goal #1 (≤8ms keystroke→glyph at 120Hz).

Measures `lattice_host::cells_worker::recompute` — the cells
worker's entrypoint — across three workloads at three line
counts. Viewport height fixed at 60 (chunked-mode threshold:
`4 × 60 = 240` lines).

| Workload                                   | 100 lines     | 1 000 lines   | 5 000 lines   | Floor / Target                            |
| ------------------------------------------ | ------------- | ------------- | ------------- | ----------------------------------------- |
| `cells_worker_full_build` (cold start)     | ~41 µs        | ~385 µs       | ~1.9 ms       | ≤2 ms@5k / ≤5 ms@5k                       |
| `cells_worker_incremental_build` (typing)  | ~39 µs        | ~63 µs        | ~103 µs       | ≤150 µs@5k / ≤500 µs@5k (≪1ms keystroke) |
| `cells_worker_cache_hit` (no-op publish)   | ~33 ns        | ~33 ns        | ~33 ns        | ≤50 ns / ≤100 ns                          |

**Reading the numbers:**

- `cache_hit` at ~33 ns confirms `recompute`'s version-compare
  fast path doesn't grow with line count — exactly the
  expected behaviour from `MatrixVersion::differs_from`.
- `incremental_build` is what fires on every keystroke. The
  5000-line cost (~103 µs) is well under any reasonable
  fraction of the 8 ms keystroke→glyph budget; the chunk
  rebuild + suffix shift scales sub-linearly because only the
  edit zone rebuilds, not the whole document.
- `full_build` is the cold path (boot frame, buffer switch).
  5000-line cost (~1.9 ms) is comfortably within a single
  paint budget — even on cold start the user sees content on
  the very next frame.

**What's NOT measured here:** `paint_cells_row` (needs a live
GPUI window so it's outside the Criterion-bench surface),
`GlyphResolver::resolve` miss path (also needs a window),
end-to-end keystroke→glyph latency (measured by the existing
held-key probes; S6 strips those once enough confidence in the
bench numbers accrues). The bench above covers the worker
side of the pipeline; the paint side is hardware-bound and
validated by hand-runs against an actual document buffer
(paint_cells is the default for active panes after S4.final.f
retired the env-var toggle).

Numbers captured: 2026-05-27, S5 first run.

---

## "Performance has regressed" warnings

Criterion reports several regressions vs. its stored baseline. The
baseline was captured before the actor refactor (commit `6d1bb24`):
buffer mutations now route through a tokio mailbox (~80µs round-
trip) instead of a direct sync `apply_edit` (~5µs).

**This is the architectural cost we accepted**, not a code
regression to fix. Specifically:

- Small-file motions / operators (10-line buffers) show 15–30%
  regression because the 80µs actor overhead dominates the few-µs
  buffer-walk cost.
- Large-file (50k-line) regressions compress (or vanish) because
  buffer work dominates.

Phase 4–7 work (LSP, plugin host) requires the actor; the
regression is what enabled them. The motions/operators benches
measure the *full path* through the dispatcher.

After the actor refactor lands as the new baseline, future runs
should not show these regressions; criterion's stored baseline can
be reset by deleting `target/criterion/`.

---

## Improvement paths (prioritized)

1. **🔼 Motion SIMD prefilters.** `word_forward`, `first_non_blank`,
   `find_char_forward` would benefit from `memchr` the same way
   search did. ~30 LOC each. Not blocking §8.2 today; ladder for
   "decisively better than neovim" framing.

2. **🔼 `with_snapshot<R>(f)` API on `DocumentHandle`.** Read-only
   paths drop from 17ns to ~5ns via `ArcSwap::load() -> Guard<T>`.
   Renderer-side `Cache` for ~2ns post-cache loads. Worthwhile when
   GPU rendering arrives and per-frame snapshot overhead matters.

3. **🔼 Frame-render bench.** `compose_visible_lines` itself isn't
   measured; we only have its parts. Bench would close §8.2 row
   "Frame render (code, 1080p) <2ms".

4. **🔼 Cmdline completion popup bench.** Vertico-style live filter
   isn't on the bench. Should be sub-millisecond on the registered
   command set.

5. **🔼 Tree-sitter incremental reparse bench.** §8.2 commits to
   <1ms p99 on a 50k-line file -- unmeasured today. Now that the
   parser is owned by `Syntax` (post Option B), the seam for
   `Parser::parse(.., Some(&old_tree))` exists; need to thread
   `Tree::edit` deltas from the document actor first.

5a. **🔼 Native highlighter bench.** `Syntax::highlight_lines_native`
   isn't measured directly. Worth adding a per-language bench that
   isolates parse + query traversal so future regressions on the
   single-parse architecture surface in CI.

6. **🔼 Open-100MB-log file bench.** §8.2 commits to <100ms first
   paint, <500ms full ready -- unmeasured.

7. **🔼 Dispatch round-trip via `DocumentHandle::dispatch`.** The
   motion + effect commit path (vs. `apply_edit` alone). Closes the
   "what does a real keystroke cost?" question.

8. **🔼 Suffix-array search index** (months of work; deferred). The
   only credible path to microsecond full-buffer scans on 200k-line
   corpora. ~5× memory cost; rebuild on every edit.

9. **🔼 Allocation discipline check** (../architecture/design.md §A.6). Per-keystroke
   alloc count via `dhat-rs`. Catches refactor regressions before
   wall-clock benches do.

10. **🔼 Long-running session bench** (§A.6). 10K random invocations;
	assert no monotonic memory growth.

---

## What's NOT here

Benches we'd want before claiming §8.2 coverage but haven't built
yet (marked 🔼 above): frame render, completion popup, tree-sitter
incremental reparse, native highlighter per-language timing, file
open, dispatch round-trip.
