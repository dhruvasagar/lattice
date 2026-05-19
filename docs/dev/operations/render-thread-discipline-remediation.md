# Render-thread discipline remediation (Phase 5.8.AF.5 / X-series)

**Status:** in progress — X1, X3 (partial), X4, X2 (renderer-hot-path cutover) landed; X2.6 (legacy field deletion), X1b (idle wake bridge), X5 (frame-budget bench) and the full-X3 (per-line styled-text element) still pending.
**Started:** 2026-05-19.
**Owners:** session-driven (Dhruva + Claude).
**Successor work:** Phase 5.8.AF.5 / Slice 3c.final (Editor on its own thread). Resumes AFTER this remediation lands.

## 1. Why this exists

The GPUI peer (`crates/lattice-ui-gpui`) breached paramount goal #1 from CLAUDE.md:

> **Performance.** Sub-frame input latency: keystroke -> glyph <= 8ms at 120Hz, <= 16ms at 60Hz. **UI thread does no I/O, no parsing, no shaping.**

Measured 2026-05-19 with the slice 3c.atomic.L instrumentation: keystroke→glyph latency on the GPUI peer is ~200 ms — **25× over the 8ms-at-120Hz budget**. On hardware that should clear the bar trivially (RTX 5070 + Ryzen 7 9700x, with llvmpipe software rendering under WSL2 as a worst-case substrate but Electron-based Slack noticeably more responsive on the same box). The user's exact words: *"performance is paramount, rendering should never block — non-negotiable from day 1"*, *"users of a text editor comparable to vim won't be running this on a heavy GPU; we need something responsive even if running on the CPU"*.

This document records the concrete breach, why it was not caught, and the sequenced remediation plan.

## 2. The breach (concrete, file+line)

The GPUI peer's `EditorView::Render::render` (`crates/lattice-ui-gpui/src/window.rs`) violates the spec four ways:

### B1. Parsing on the render thread
`crates/lattice-ui-gpui/src/window.rs:954` — `self.app.refresh_highlights()` invokes `Editor::refresh_highlights_window` (`crates/lattice-host/src/highlights.rs:48`) which on a cache miss runs `snap.highlight_lines(start, end_line)` — a tree-sitter query walk. **That is parsing on the render thread.**

Cost: 200µs steady-state, 16 ms post-file-open, 200–600µs spikes on scroll-change cache misses.

### B2. I/O / event drain on the render thread
`crates/lattice-ui-gpui/src/window.rs:967` — `self.app.editor.run_tick_pending()` is the per-tick aggregator that drains:
- pending hover, signature-help, definitions, references, symbols, moniker, completion, completion-resolve, format, rename, selection-range, code-action, live-picker, picker-init responses;
- mode-lifecycle events;
- option-change cascades;
- LSP log / progress / detach events;
- inbound configuration / show-message / show-documents / apply-edit requests;
- message events;
- diagnostic / inlay-hint / semantic-tokens / code-lens / file-watcher refreshes;
- `maybe_request_*` pre-drain pumps.

Each item is I/O-driven; arrivals come via tokio channels. **That is I/O drain on the render thread.** TUI peer commits the same violation at `crates/lattice-ui-tui/src/runtime.rs:142`.

Cost: 100–300µs idle, **49 ms on file open** (LSP attach backlog), 200µs–3 ms during motion.

### B3. Shaping on the render thread (element-tree fan-out)
`crates/lattice-ui-gpui/src/window.rs:773–800` — per-character `div()` construction:

```rust
for (byte_idx, c) in line.char_indices() {
    let cell = div().text_color(rgb(syntax_color(span_style))).child(c.to_string());
    cells.push(cell);
}
```

For an 80×80 viewport: **~6,400 widget divs per frame**, each carrying color / border / background style metadata that GPUI's layout pipeline must resolve.

**That is shaping** — the spec's term for text layout. Element-tree fan-out proportional to character count violates goal #1 the same way per-keystroke parsing does. The CPU-rendered budget assumes the renderer hands the GPU a tree the size of the viewport in **lines**, not in characters.

Cost (downstream of our `render()` return): the difference between our measured `frame_us ≈ 5–9 ms` and the observed frame-to-frame cadence of **~230 ms**. GPUI's layout/paint/composite pipeline takes 5–10× our own paint time to process the 6,400-element tree.

### B4. Unconditional per-frame republish
`crates/lattice-ui-gpui/src/lib.rs::GpuiApp::ensure_cursor_in_viewport` (introduced by slice 3c.atomic.I) calls `editor.publish_render_state()` unconditionally every frame. Allocates ~11 sub-state Arcs per frame regardless of whether scroll changed. Cost ~200µs. **Regression from slice 3c.atomic.I** — should be conditional on actual mutation.

## 3. Why the design was not enforced

Five structural reasons. None excuse the breach — they explain how an explicit non-negotiable rule produced a 25× breach without alarm.

### R1. The `--features window` build IS in CI, but commits stayed local
**Corrected 2026-05-19 (after slice X1 surfaced the actual gap).** The `.github/workflows/ci.yml` `gpui-window-build` job (Phase 5.7.A.1, lines 119-141) already runs `cargo build -p lattice-ui-gpui --features window --locked` on Linux / macOS / Windows for every push and PR. The structural gap is *not* missing CI coverage — it's that commits H, I, J, K, L, M, the X-series doc, and X1 all sat on the local branch unpushed, so CI never ran on any of them. The ~30-minute window where `self.app.ad()` and `self.app.set_viewport_height()` referenced non-existent `GpuiApp` methods was a *local-only* window between H landing and K incidentally fixing the compile; CI would have rejected H at push time, but the breach compounded across the H→K stretch because nothing pushed.

The fix shape changed: instead of *adding* CI coverage (X4 in the original plan), the durable enforcement is to push more frequently so the existing CI gate runs, OR add a local `pre-push` git hook that runs `cargo build -p lattice-ui-gpui --features window --locked` so the gate fires before the push is even attempted. The latter is a developer-environment change and isn't imposed by this remediation doc.

### R2. No frame-budget benchmark on the GPUI peer
Task #120 in the project backlog — *"Design + add GPUI-layer UI responsiveness benchmarks"* — has been pending through 13 architectural slices. No automated assertion that `frame_us < 8000`. Performance budgets in CLAUDE.md without CI enforcement are decoration.

### R3. "Off the UI thread" was interpreted as "host-side"
The 3b slices moved LSP request *implementations* host-side (good). But the GPUI peer kept invoking those same paths from inside `Render::render` (bad). Slice messages read *"every per-tick drain is now folded into `run_tick_pending`"* — true at the host level, false at the render-call-site level. Nobody asked "is `run_tick_pending` itself called from a per-frame body?" because each slice individually framed itself as a relocation win.

### R4. Element-tree fan-out was never classified as "shaping"
CLAUDE.md explicitly forbids shaping on the UI thread. Per-character `div()` construction was treated as paint code, not shaping. By the spec's own term it IS shaping — laying out glyph positions and styles into the GPU pipeline. A correct text renderer batches a line into one styled-text element; ours emits N sibling widgets per line.

### R5. "Scaffold" framing insulated the GPUI peer from audit
Many slices' comments read *"the GPUI peer doesn't have X yet — wires in when Y lands"*. That framing protected the peer from goal-#1 audit because "scaffold" implies "not ready for review." But once `cargo run --features gui` produces a real running binary, it is a real renderer with real spec obligations. Calling it a scaffold doesn't exempt it.

## 4. Remediation plan (the X-series)

Five slices, sequenced. Each addresses one structural failure and one spec rule.

### X1. Move `run_tick_pending` out of per-frame bodies
**Spec enforced:** "no I/O on the UI thread."
**Files:** `crates/lattice-ui-gpui/src/window.rs`, `crates/lattice-ui-tui/src/runtime.rs`, `crates/lattice-ui-gpui/src/lib.rs`, `crates/lattice-ui-tui/src/app/dispatch.rs` (or `app.rs::apply`).
**Plan:** Each peer's dispatch tail (`GpuiApp::dispatch_action`, `App::apply`) calls `editor.run_tick_pending()` AFTER its existing signal fanout. Renderer bodies stop calling `run_tick_pending`. The 49 ms file-open spike no longer hits the renderer's first frame; the steady-state per-frame tick cost vanishes.
**Idle-wakeup gap:** LSP responses arriving with no keystroke in flight won't be drained until the next keystroke. **Acceptable as a one-keystroke degradation pending X1b.**
**Expected impact:** Removes the file-open freeze on the renderer. Cuts ~200µs–3 ms per frame steady-state. Goal #1 violation B2 closed.

### X1b. Idle wakeups
**Spec enforced:** "no I/O on the UI thread" (idle path).
**Plan:** Spawn-side workers post to a `tokio::sync::Notify` (or equivalent) on completion. A renderer-thread bridge listens and posts a wake action so the next dispatch tick fires `run_tick_pending`. Renderer body still never calls `run_tick_pending`. Closes the one-keystroke degradation from X1.

### X2. Move `refresh_highlights` to a worker — **LANDED (renderer-hot-path cutover) 2026-05-19**
**Spec enforced:** "no parsing on the UI thread."
**Files:** `crates/lattice-host/src/render_state.rs`, `crates/lattice-host/src/editor.rs`, `crates/lattice-host/src/dispatch.rs`, `crates/lattice-host/src/editor_boot.rs`, `crates/lattice-host/src/highlights_worker.rs` (new), `crates/lattice-host/src/folds.rs`, `crates/lattice-ui-gpui/src/window.rs`, `crates/lattice-ui-tui/src/render.rs`, `crates/lattice-ui-tui/src/runtime.rs`.

**Landed shape** (driven by `tokio::sync::Notify` instead of a typed `Event::*` channel — simpler and integrates with the existing `publish_render_state` tail):

- `SyntaxRenderState` carries inputs (`syntax_handle`, `scroll`, `viewport_height`, `fold_hash`, `text_version`) populated by `build_render_state` and a nested `visible_spans: Arc<ArcSwap<VisibleSpans>>` output cell. `VisibleHighlightsKey` migrated host-side from `highlights.rs` to `render_state.rs`.
- `Editor::highlight_wake: HighlightWake` (newtype around `Arc<Notify>`) + `Editor::syntax_visible_spans_cell` (durable Arc the worker and `SyntaxRenderState` both clone from).
- `publish_render_state` fires `highlight_wake.0.notify_one()` at its tail.
- `crates/lattice-host/src/highlights_worker.rs` owns the async loop. `recompute(...)` is the pure synchronous decision function (returns `Clear` / `CacheHit` / `StaleSnapshotHold` / `Recomputed`); tests cover it.
- `editor_boot` spawns the worker on the process-wide LSP runtime, sharing the three Arcs.
- Both peers' per-frame bodies drop `refresh_highlights()` and read `render_state.syntax.visible_spans.load()` directly. Goal #1 violation B1 closed for both peers.

**Idle-wake gap (X1b) still open:** after a publish-burst with the user idle, the worker may publish fresh spans without anything scheduling a renderer repaint. Worst case: one frame of slightly-stale colors until the next event triggers paint. Documented; X1b closes it.

**Remaining cleanup (X2.6):** `Editor::refresh_highlights_window` + `visible_highlights` + `visible_highlights_key` fields are intentionally retained so the TUI peer's `refresh_highlights_cache_invalidates_*` tests stay green. X2.6 ports those tests to drive `highlights_worker::recompute` and deletes the legacy method + fields.

**Open: bench (X5 scope).** Per-keystroke UI-thread cost under held-key bursts needs to be machine-asserted post-X2 (the original motivation for the user-visible "cursor disappears" symptom). Lives in `crates/lattice-host/benches/highlights_worker.rs` once X5 lands.

### X3. Rewrite `paint_pane` to per-line text runs
**Spec enforced:** "no shaping on the UI thread"; element-tree fan-out is `O(viewport-lines)`, not `O(chars)`.
**Files:** `crates/lattice-ui-gpui/src/window.rs::paint_pane`.
**Plan:** Investigate GPUI 0.2.2's styled-text API. Each visible line becomes a single styled-text element with span attributes for color / underline / cursor / visual / hlsearch overlays. Element tree shrinks from ~6,400 nodes to ~80 nodes for the same viewport — **80× reduction**.
**Expected impact:** Drops the downstream ~230 ms-per-frame GPUI composition cost. Target: 30–60 FPS on llvmpipe, 120 FPS on bare-metal Vulkan. Goal #1 violation B3 closed.

### X4. CI build for `--features window` (already done; gap was push cadence, not coverage)
**Spec enforced:** spec breaches caught at PR time, not by user reports.
**Status:** **Already implemented** at `.github/workflows/ci.yml` lines 119-141 as the `gpui-window-build` job (Phase 5.7.A.1). Runs `cargo build -p lattice-ui-gpui --features window --locked` on Linux / macOS / Windows on every push + PR. Verified 2026-05-19 by checking out slice H's working-tree contents via `git show` — the H state would have failed CI immediately had it been pushed; the local-branch lag is what let H..K compound unchecked.
**Optional follow-up (not part of this remediation's required scope):** add a `pre-push` git hook in `scripts/hooks/` that runs the same `cargo build -p lattice-ui-gpui --features window --locked` so the gate fires regardless of when the developer chooses to push.

### X5. GPUI frame-budget benchmark
**Spec enforced:** "8 ms at 120Hz" becomes machine-checked.
**Plan:** Programmatic `EditorView::render` driver over a 1000-line synthetic file. Asserts `frame_us < 8000`. Lives in `crates/lattice-ui-gpui/benches/` (criterion). Wired to fail CI on regression. Concrete realisation of backlog task #120. Closes structural reason R2.

## 5. Order of attack

X1 → X4 → X3 → X1b → X2 → X5.

Rationale:
- **X1 first** because it's the smallest surgical relocation and unsticks the file-open freeze immediately.
- **X4 second** because the next slice should not land without CI catching feature-gated breaches.
- **X3 third** because it's the perf-defining change; B3 dominates the budget. Substantial scope (own session).
- **X1b fourth** to close the idle-wakeup gap X1 introduces.
- **X2 fifth** because its absolute cost is smaller than X3 and the cache it produces benefits both peers.
- **X5 last** so the benchmark asserts the final shape, not intermediate stages.

## 6. Acceptance criteria

The X-series is complete when ALL of:

- [ ] No call inside any renderer peer's per-frame body does I/O, parsing, shaping, or unbounded fan-out.
- [ ] `cargo build --workspace --all-features` passes in CI on every PR.
- [ ] GPUI frame-budget benchmark asserts `frame_us < 8000` over a 1000-line file and fails CI on regression.
- [ ] Measured keystroke→glyph latency on the GPUI peer is ≤ 8 ms steady-state on bare-metal GPU, ≤ 33 ms (30 FPS) on llvmpipe, on a 5000-line file.
- [ ] `feedback_no_ui_thread_work.md` memory and `CLAUDE.md` updated with concrete forbidden-pattern catalog (already done 2026-05-19).

## 7. Forward enforcement: how future slices are audited

Any slice touching a renderer peer's per-frame body — `crates/lattice-ui-tui/src/runtime.rs` body, `crates/lattice-ui-gpui/src/window.rs::Render::render`, future Web peer's analogue — MUST be audited against this checklist:

1. Does this call do I/O? (Read from disk, send over a socket, query an LSP server, run a subprocess.)
2. Does this call do parsing? (Tree-sitter, regex over the buffer, language-server protocol parsing.)
3. Does this call do shaping? (Text layout, glyph positioning, per-character or per-byte element-tree construction.)
4. Is the work bounded by O(viewport-lines), or does it scale with document size, LSP state, or syscalls?

If ANY of (1)–(3) is yes, OR (4) is "scales with non-viewport size", the call relocates off-thread. No exceptions, no "we'll fix it later", no "the host method moved so it's fine".

After this remediation lands, the 3c migration (Editor on its own thread) resumes. 3c's correctness depends on this remediation being done first: an Editor that lives on a dedicated thread but is driven by a renderer doing parsing / I/O on its own thread defeats the whole architectural arc.

## 8. References

- CLAUDE.md, paramount goals §1 + §4.
- `docs/dev/architecture/design.md` §8 (Performance commitments).
- `docs/dev/operations/implementation.md` (slice ledger).
- Memory: `feedback_no_ui_thread_work.md`.
- Instrumentation: slice 3c.atomic.L (commit `4fd1fc7`).
- First diagnostic data: this conversation's 2026-05-19 perf traces.
