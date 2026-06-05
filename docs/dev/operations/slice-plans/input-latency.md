# Slice plan — Input latency (event-driven TUI loop)

Sequencing + status for the keystroke→glyph latency work. Design fragment
(contracts, rationale, rejected alternatives, the logical-vs-physical key-identity
decision): `docs/dev/architecture/input-pipeline.md`.

**Problem (see design § "Current loop"):** the typed char is published
synchronously (B2.3), but the TUI run loop draws once per single input event,
pays ~8 blocking actor RPCs per cycle, and polls on a 100ms timeout. On a large
viewport (e.g. `design.md` maximised) a typing burst trails visibly — a keystroke
UX-contract violation.

**Goal:** keystroke→glyph within one frame independent of async-subsystem load;
the draw does zero blocking round-trips and zero work proportional to document
content.

## Slices

### I.1 — input coalescing  ✅ (2026-06-05)

Drain *all* pending terminal events before drawing, instead of one-per-iteration.

- `runtime.rs`: the single `if poll(100ms) { read ONE; apply }` is now
  `if poll(100ms) { loop { read; apply; if should_quit break; if !poll(0) break } }`
  — wait up to 100ms for the FIRST event (idle responsiveness, until I.3), then
  drain every already-buffered event before looping back to draw. A burst of N
  queued keys becomes N cheap applies + **one** draw.
- The translate context is rebuilt per event (applying one event can change the
  modal state / mode stack that governs the next event's translation).
- `should_quit` short-circuit: the drain breaks the instant a quit lands, so later
  buffered keys are not applied against a tearing-down app and no final draw fires.
- Resize/paste events handled in the same drain (paste is already one
  `Event::Paste`; resize defers to the next iteration's viewport setup).
- Validation: `cargo build -p lattice-cli` green; full TUI lib suite (1467) green —
  per-event apply/translate semantics are unchanged, only the draw cadence. Manual:
  re-test typing on `design.md`.
- **Test seam caveat:** a dedicated "feed a burst, assert one compose pass reflects
  the final state" test needs a mockable event source — crossterm's
  `poll`/`read` are not. That seam arrives with I.3 (the input-reader channel), so
  the burst test lands there; until then coalescing is covered by the regression
  suite (semantics unchanged) + manual.
- Risk: low — end state identical, only intermediate frames dropped (desired).
- Deps: none.

### I.2 — RPC-free draw  ✅ already satisfied for the active pane (verified 2026-06-05)

**The premise was wrong.** The original scope ("remove ~7 per-frame `read_editor`
RPCs from the draw") came from a *stale code comment* in `render.rs`. Verifying
against the actual code: the active-pane draw is **already RPC-free** and has been
since slice 3c.final.X —

- The active option reads hit published `RenderState`, not the actor:
  `foldenable`/`show_line_numbers`/`relative_line_numbers` → `ad().option_cache`;
  all five LSP gates (`lsp_mode_enabled_for` … `lsp_progress_mode_enabled_for`)
  → `self.modes()` = `render_state.load().modes`. All wait-free Arc bumps.
- This is **enforced by existing tests**: `compose_visible_lines_makes_zero_actor_calls`,
  `modeline_label_makes_zero_actor_calls`, `pane_status_label_makes_zero_actor_calls`
  (assert the `actor_call_counter` delta is 0 across a paint).
- ⇒ I.2 will **not** reduce typing latency; nothing to do for the active pane.

**Residual (optional, non-latency):** `FrameView::for_buffer` →
`show_line_numbers_for`/`relative_line_numbers_for` → `resolved_option` is a
`read_editor` RPC, hit by `draw_inactive_document` (inactive panes / splits only).
Off the single-pane typing path. If taken later: publish per-buffer
number/relativenumber resolution into a `RenderState` substate so inactive panes
read wait-free too. Low priority — split-layout polish, not the latency goal.

### I.3 — event-driven wake (drop the 100ms poll)  🗒

Wake the loop on *(input-ready OR actor-publish)* instead of a fixed timeout.

- Spawn a terminal-input reader feeding a channel; multiplex it with the actor's
  existing `paint_request: Arc<Notify>` (already wired for GPUI) so an async
  publish (syntax recolour, LSP decoration, cursor blink) repaints promptly.
- Removes the up-to-100ms async-repaint lag; keeps CPU idle when nothing changes
  (no spin).
- Rejected sub-option: shorten the poll to ~16ms — burns CPU spinning and still
  caps latency at the interval (strictly worse than event-driven).
- Test: assert an actor publish with no input pending triggers exactly one draw;
  assert idle = zero draws.
- Risk: medium-high — changes the input-ingestion architecture (reader thread +
  channel + crossterm raw-mode handoff); needs careful teardown on quit.
- Deps: cleanest *after* I.1 (coalescing) so the input channel drains in the same
  shape.

### I.4 — publish coalescing  ✅ (2026-06-05)

PERF-DIAG measurement (typing `design.md`) showed **one `dispatch(Insert)` publishes
~6×** — each a whole-world `build_render_state` (~150–400µs) + a wake of *both* the
cells and virtual-rows workers (so 12 worker recomputes/keystroke). Cause: the
"write field; publish" convention compounds when setters are chained inside one
dispatch (`do_insert_text` → edit → `ensure_cursor_visible` → `maybe_reparse_syntax`
→ setters → the dispatch tail each publish).

Fix: a depth-guarded publish batch (`PublishCache.publish_batch_depth` /
`publish_pending`). `dispatch` / `handle_effect` open the batch at entry; while
depth > 0 `publish_render_state()` suppresses (marks pending); the single real
publish fires when the outermost batch unwinds. **6 → 1 publish/keystroke; 12 → 2
worker wakes.** State lives in `PublishCache` (Default-derived, already the actor's
publish-side lock) → zero `Editor` construction churn; lock scoped + released
before `build_render_state` re-locks (std `Mutex` non-reentrant). Depth-counted so
nested (cascaded) dispatch coalesces too.

- Validation: `cargo build -p lattice-cli` green; 691 host-lib tests green (final
  published state unchanged — only its frequency). Manual: re-run with PERF-DIAG to
  confirm publishes/keystroke 6 → 1.
- **Caveat:** this removes the *amplification* but each keystroke still pays one
  whole-world `build_render_state` (~200µs). Sub-ms requires I.5.
- Deps: none. Correct independently of I.5 (the batch guard stays useful there).

### I.5 — per-substate publication (the sub-ms / test-of-time fix)  🗒

Retire the whole-world `build_render_state` on the keystroke path. Each subsystem
`store()`s its own substate Arc when *it* changes; a keystroke publishes only the
**active-document** substate (cursor, text version, `DisplayMatrix` pointer — the
per-pane `DisplayMatrix` ArcSwap is *already* independently published by the cells
worker + B2.3). Keystroke publish becomes a few Arc swaps → **sub-ms even on
low-end hardware.**

- This is the documented 3b/3c destination (`build_render_state` is an explicit
  placeholder) and the cross-editor convention: Neovim emits incremental grid
  deltas; Helix/Zed read models directly — none rebuild a whole snapshot per key.
  See `input-pipeline.md` § "Prior art".
- Slice **active-document-first** (the typing hot path): make the renderer read the
  active-doc fields from their own substate cell instead of monolithic
  `render_state.load()`, then migrate the rest. Gate every step with the existing
  `compose_visible_lines_makes_zero_actor_calls` / `modeline_label_*` /
  `pane_status_label_*` paint tests.
- Goal: drive keystroke→edit-published **sub-ms**, toward the imperceptibility bar —
  no fixed target; the ratchet only moves it down (see `input-pipeline.md` §
  "Latency goal: imperceptible, not a number").
- Risk: medium-high — touches the renderer's read contract; slice tightly.
- Deps: after I.4.

### I.6 — physical key-identity seam  🗒  (deferred, v1+)

Reserve the optional physical/positional axis on `KeyChord` (design § "Key
identity"). **Not scheduled** — niche ergonomics / i18n, not a paramount-goal
blocker. Captured so I.1–I.3 don't bake in a logical-only assumption.

- When taken: add an optional physical-code field to the input event (populated by
  GPUI + kitty-protocol terminals, `None` in plain terminals); teach the keymap
  layer to match a binding on the physical axis when the binding opts in.
- GPUI-first + kitty-enhanced-terminal; degrades to the logical binding where the
  substrate can't report a physical code.
- Deps: independent of I.1–I.3; touches `lattice-protocol::chord` + keymap match,
  not the run loop.

### I.7 — minimize per-keystroke actor round-trips  ✅ (2026-06-05)  (the felt-latency fix)

Measurement (`actor_call_counter` delta, typing `design.md`): **each keystroke made
6 blocking actor round-trips** (49×6, 22×7, a few ×11), each ~0.5ms on WSL2
(futex/scheduler) → the ~3.5ms drain. The **round-trip count — not publish cost — is
the felt typing latency**: I.4 collapsed publishes 6→1 and worker wakes 12→2 but the
drain was unchanged, which is what isolated this. On native hardware each round-trip
is ~tens of µs (so the 3ms is partly a WSL2 artifact), but 6 round-trips/keystroke
still misses the sub-ms / low-end-hardware bar.

The 6, pinned by static trace of the `mutate_editor*` / `read_editor` seam:

| # | call | site | kind |
|---|------|------|------|
| 1 | `completion_popup_active()` → `minor_mode_enabled_for` | translate ctx (runtime.rs) | read |
| 2 | `dispatch(action)` | `App::apply` | mutate |
| 3 | `ensure_cursor_visible()` | apply tail | mutate |
| 4 | `maybe_reparse_syntax()` | apply tail | mutate |
| 5 | `sync_keymap_overlays()` | apply tail | mutate |
| 6 | `run_tick_pending()` | apply tail | mutate |

(`chord_capture_active()` early-returns on published `ad().modal` in Insert → 0 RPC.)

**Landed fix — collapse to 1 round-trip:**

1. **Fused in-actor tail — [`Editor::dispatch_fused`]** (`lattice-host/src/dispatch.rs`).
   Items 3–6 are all pure-host `Editor` methods (the actor's own `async_landed` arm
   already calls `run_tick_pending` in-actor), so they fold into the dispatch
   round-trip. `dispatch_fused` runs `dispatch`, and **iff** the outcome has no
   renderer-coupled work (`effects`/`renderer_signals`/`next_actions` empty, not
   `consumed`) **and** no popup is up, runs the four-op tail in-actor and returns its
   signals in `FusedDispatch.tail_signals`. Otherwise `tail_signals` is `None` and
   `App::apply` runs the **legacy multi-RPC tail** with the original ordering — the
   rare effect-bearing keystrokes (file open / LSP / picker / hover) where effect
   handlers must run BEFORE the tail (e.g. `OpenBufferAt` switches the active doc,
   then `ensure_cursor_visible` clamps against it) and where typing-contract latency
   is not the concern. Publishes coalesce: an outer publish batch wraps the sequence
   so the dispatch flush + the four tail flushes collapse into the single flush the
   actor's `mutate_*` wrapper fires after the closure returns.
2. **RPC-free `completion_popup_active()`** (`lattice-ui-tui/src/app/lsp.rs`). Now
   reads the published `modes()` map (mirroring `lsp_diagnostics_mode_enabled_for`)
   instead of `minor_mode_enabled_for` → `read_editor`. The popup mode's activation is
   established by the prior keystroke's `sync_keymap_overlays` (which publishes), so
   the published map is the correct pre-dispatch state. cfg(test) keeps the direct-
   editor read.

**Result: 1 round-trip per plain keystroke** (the single `dispatch_fused` crossing);
items 1 and 3–6 are gone. Cap test `apply_noop_action_makes_bounded_actor_calls`
(`render.rs`) tightened 6 → 2 (baseline 1, one slot headroom); it would read 5
pre-I.7, so passing at ≤ 2 proves the fuse fired. host-lib 691 + tui-lib 1467 green.

**GPUI parity — deferred (documented).** GPUI's `dispatch_action` is pre-parity
(5.7.B.4+): it does **not** run the `ensure_cursor_visible`/`maybe_reparse_syntax`/
`sync_keymap_overlays` tail at all (none appear in the GPUI crate) — its plain-key
path is 2 mutate RPCs (`dispatch` + `run_tick_pending`), not TUI's 6. `dispatch_fused`
*bundles* that tail, so adopting it on GPUI would conflate the round-trip collapse
with bringing GPUI's tail to parity. GPUI adopts `dispatch_fused` when its apply tail
reaches TUI parity; until then it pays 2 mutate RPCs/keystroke, which the primitive
will collapse to 1 at that point. (The shared host primitive is the parity vehicle —
the lockstep rule's trigger categories (effect classifier / renderer match arms /
theme / virtual rows / diff-sign) are not touched by this host-internal change.)

- Target met for the TUI peer: the keystroke entry path is 1 actor crossing.
  Driving the single publish **sub-ms** (toward the imperceptibility bar) still needs
  I.5 to make it cheap (it currently carries one whole-world `build_render_state`
  ~200µs).
- Deps: independent; **execution order I.7 → I.5 → I.6** (chosen 2026-06-05 — the
  felt-latency fix before the publish-architecture + the physical-key feature).

## Sequencing

Landed: **I.1** (input coalescing) ✅, **I.4** (publish coalescing) ✅, **I.7**
(6 → 1 keystroke round-trip) ✅, plus the per-iteration geometry diff-guard. **I.2**
(RPC-free draw) was already satisfied for the active pane.

**Execution order (chosen 2026-06-05): I.7 → I.5 → I.6**, then I.3.
- **I.7** (collapse 6 → 1 keystroke round-trip) is the felt-latency fix and went
  first ✅.
- **I.5** (per-substate publication) makes the single remaining publish cheap (the
  sub-ms/test-of-time architecture).
- **I.6** (physical key seam) is the deferred v1+ feature.
- **I.3** (retire the 100ms poll) last — fixes async-decoration lag, not the typed
  char; GPUI already has the I.3 shape natively (`cx.notify()`-driven).

Measurement note: I.1 + the geometry diff-guard + I.4 collapsed the idle/per-keystroke
publish *storm* (idle ~6/100ms → 0; keystroke publishes 6 → 1, worker wakes 12 → 2),
and I.7 collapsed the keystroke entry path to 1 actor crossing — all real wins, all in
the µs–sub-ms range, both needed to drive the publish sub-ms (with I.5).

**But none of these was the *felt* typing lag.** That was diagnosed separately
(2026-06-05): the TUI hot path was already ~0.7ms p50, yet typing visibly trailed. Root
cause was **stale text**, not timing — the incremental `DisplayMatrix` reused the EDITED
line for intra-line edits (`EditDelta {removed:0,added:0}` → `pre_edit_end_line() ==
start_line`), painting pre-edit text for a frame (`|word`→`w|ord`→` |word`). Fixed in
`cells_worker::try_incremental_display_build` (gate the suffix boundary one past the
edited line for pure intra-line edits). See `project_typing_latency_root_cause` and
`input-pipeline.md`. Lesson: a "feels laggy" report is not necessarily a timing problem —
check the renderer for version-lag / stale-row reuse first.

## Status legend
✅ landed · 🚧 in progress · 🗒 planned
