+++
title = "Input Pipeline — key identity, dispatch, and keystroke→glyph latency"
+++

# Input Pipeline — key identity, dispatch, and keystroke→glyph latency

Design fragment. The *what* and *why* of how a physical key press becomes a
rendered glyph: the canonical input token, the logical-vs-physical key-identity
decision, and the keystroke→glyph latency contract the run loop must honour.

Sequencing and slice status live in
`docs/dev/operations/slice-plans/input-latency.md`; this file owns the contracts
and rationale. Binding→action structure (trie, layers, mode scoping) lives in
`keymap-architecture.md`; this file owns the *event* that the keymap matches.

## Paramount-goal anchor

- **#1 Performance** — keystroke→glyph **imperceptible**: indistinguishable from the
  substrate echoing the key, always within one display frame (8.3ms @120Hz, 16ms
  @60Hz) under any background load, ratcheted by CI; the UI thread does no I/O /
  parsing / shaping (§ "Latency goal: imperceptible, not a number").
- **#3 Extensible vim modal editing** — the grammar IS the public command API,
  and it is **mnemonic** (`w`=word, `d`=delete, `f`=find). This dictates the
  key-identity model (§ "Key identity").
- **UX (higher court)** — the keystroke contract: only the edited line may
  visibly change; the typed char is synchronous; syntax/decorations are
  eventual. A character that appears after a delay is a UX defect even if every
  goal-#1 micro-budget is individually met.

## Key identity: logical (layout-resolved) vs. physical (scan code)

### Canonical token

The single input token is `lattice_protocol::chord::KeyChord { key: KeyKind, mods:
KeyMods }`, where `KeyKind` is `Char(char)` — a **logical, layout-resolved**
character — or a `SpecialKey` (Esc, Enter, Tab, arrows, F-keys, …). Both
renderer peers normalise into it:

- **TUI** — crossterm delivers `KeyCode::Char(c)` where `c` is already the
  character the OS keyboard layout produced; `chord::from_event` maps it to
  `KeyChord`. Layout-resolved.
- **GPUI** — `Keystroke::key` is a logical key-id string; `gpui_chord::from_keystroke`
  maps it to `KeyChord`. Layout-resolved.

So Lattice is **fully logical** today. There is no physical/positional scan code
anywhere in the model, and the grammar/keymap matches on the logical char.

### Decision: logical stays canonical; physical is an optional opt-in seam

**Logical is correct as the default, and this is load-bearing, not incidental:**

- **Goal #3.** The grammar is mnemonic. `d`-for-delete only means something bound
  to the *letter* `d`; a positional binding would attach "delete" to a physical
  key location and destroy the mnemonic. The mnemonic grammar *requires* logical
  identity — it is not merely vim convention.
- **Heuristic #2.** "Editors should use physical scan codes" originates in
  positional-binding contexts (WASD games, layout-independence utilities). It does
  not transfer to a mnemonic command grammar; it is a data point, not a
  justification.
- **Editor references.** Vim/Emacs (authoritative for grammar convention) are
  logical/mnemonic; Zed's keymap (authoritative for substrate) is logical too.
  Both axes agree — strong signal.
- **UX / convention.** Vim muscle memory worldwide is on the character
  (`hjkl`, `dw`). Convention decisively favours logical for user-facing bindings.

**But the genuinely-better long-term shape (heuristic #1) is logical-default with
a physical seam, not logical-only.** Two real cases want positional identity, and
both are per-binding opt-ins rather than a global flip:

- **Non-QWERTY ergonomics** — a Dvorak/AZERTY user who wants `hjkl` back on the
  home-row *position*. A known vim pain point.
- **i18n punctuation** — layouts where vim's `{`, `}`, `[`, `]`, `/` require
  AltGr / dead keys; a physical binding sidesteps the dead-key problem.

So `KeyChord` should be allowed to **carry** an optional physical/positional code
alongside the logical key, and the keymap layer chooses which axis a binding
matches on. The grammar stays mnemonic-logical by default; physical is an overlay.

### Substrate availability (the honest constraint)

Physical scan codes are **not uniformly available**, so the physical axis must be
modelled as optional (`None` where unavailable), never assumed:

| Surface | Physical identity? | Mechanism |
| --- | --- | --- |
| Plain terminal (TUI) | **No** | Layout-resolved characters only; classic terminal input has no scan code. |
| Kitty-protocol terminal (TUI) | Partial | kitty keyboard protocol (crossterm-supported): base-layout key + shifted/unshifted alternates + key-release + disambiguated mods. "Layout-independent-ish," not raw HID. kitty / foot / ghostty / wezterm / newer xterm. |
| GPUI (GUI) | **Yes** | OS-level physical code (macOS hardware keyCode, X11/Wayland keycode, web `code`). GPUI surfaces the logical id today; the physical info exists underneath. |

⇒ A physical-binding feature is inherently **GPUI-first + Kitty-enhanced-terminal,
degrading to logical-only in plain terminals.** The asymmetry is a design input,
not a bug: bindings on the physical axis simply don't resolve where the substrate
can't report it, and the fallback is the logical binding.

### Recommendation

Keep logical `KeyChord` canonical. **Reserve the optional physical axis as a seam
now** (so the protocol type doesn't bake in a logical-only assumption), but
**defer the physical-binding keymap feature to v1+** — it is niche
ergonomics / i18n, not a paramount-goal blocker. Don't over-build it; don't
foreclose it. See slice `I.4` (deferred).

### Rejected: physical-as-default

Make the canonical token positional and resolve mnemonics through a layout table.
Rejected: inverts goal #3 (the grammar would no longer be "the character is the
command"), breaks universal vim muscle memory (UX), and is impossible to honour in
plain terminals (the dominant TUI substrate) — so the default would be undefined
on the most common surface. Logical-default + optional physical overlay dominates
it on every axis.

## Keystroke→glyph path and the latency contract

Dispatch is a **blocking** RPC to the single-writer editor actor
(`editor_actor::mutate_blocking_with`); `dispatch`'s tail publishes `RenderState`
synchronously, and the B2.3 sync `DisplayMatrix` rebuild makes the typed char
text-current *inside* that blocking call. So **when `apply(key)` returns, the
character is already in the published snapshot** — the data is synchronous by
construction. Syntax colour / inlay / diagnostics are produced off-thread (cells
+ highlight workers run on the multi-thread LSP runtime, reparse via
`spawn_blocking`) and catch up a frame or two later — eventual, per the contract.

**Contract:** the run loop MUST present the published char within one frame of the
keystroke, independent of async-subsystem load, and MUST NOT block the draw on
async work proportional to document content.

## Current loop and its three latency sources

`lattice-ui-tui/src/runtime.rs` run loop, today:

```
loop {
	…per-frame setup…
	draw_frame(…)            // builds element tree + ~7 blocking read_editor RPCs
	if poll(100ms) {         // event-blind to actor publishes
		read ONE event       // one input event per iteration
		apply(event)         // blocking actor RPC
	}
}
```

The char is synchronously available (above), yet typing on a large file (e.g.
`design.md` on a maximised terminal) trails visibly. Three structural causes,
none of which is the char data itself:

1. **One input event per iteration, one draw each.** A typing burst of N queued
   keys takes N full draw cycles to drain; the display trails by
   (cycle time × queue depth). This is the dominant cause of "characters appear
   after a delay."
2. **The blocking `apply` round-trip per keystroke.** Dispatch parks the UI
   thread on the single-thread actor; if the actor is mid-`async_landed` publish
   (fired on each reparse completion; O(window) for the sync rebuild + build),
   the keystroke queues behind it. On a maximised viewport this O(window) cost is
   larger, so the wait grows with viewport size.

   > **Correction (2026-06-05):** an earlier draft also listed "~7 `read_editor`
   > reads inside the draw" as a source. That was wrong — it came from a *stale
   > code comment* in `render.rs`, not the current code. The draw path is
   > **provably RPC-free**: `compose_visible_lines`, `modeline_label`, and
   > `pane_status_label` each have a `*_makes_zero_actor_calls` test asserting zero
   > actor-seam calls (they read published `RenderState` — `option_cache`,
   > `modes()` — wait-free). The *only* residual `read_editor` in any draw path is
   > `resolved_option` in `draw_inactive_document` (inactive panes / splits), off
   > the single-pane typing path. So "RPC-free draw" (former slice I.2) is already
   > true for the active pane; see the slice plan.
3. **The 100ms poll is publish-blind.** Async repaints (syntax colour, cursor
   blink, LSP decorations) can wait up to 100ms because the loop only wakes on
   input or timeout, not on an actor publish.

**Why `design.md` is worse:** a large doc on a maximised terminal = a large
viewport = a costlier `draw_frame` per cycle (more cells to build / diff / write).
Bigger per-cycle cost × one-draw-per-key = more visible trailing. The cost scales
with *viewport*, not file length — but a big file is what makes you open a big
viewport.

## Target architecture

Three changes, smallest-leverage-first, each independently shippable:

1. **Input coalescing.** Drain *all* pending input events
   (`while poll(0) { read; apply }`), then draw once. A burst collapses to N cheap
   applies + 1 draw. Intermediate frames during a burst are not wanted — the user
   wants the latest state — so this is strictly a UX win.
2. **RPC-free draw — already true for the active pane** (verified 2026-06-05; see
   the Correction above). The active draw reads only published `RenderState`. The
   sole residual is `resolved_option` in inactive-pane `draw_inactive_document`
   (splits) — a minor, non-latency cleanup, not the typing path.
3. **Event-driven wake.** Replace the 100ms poll with a select on *(input-ready OR
   actor-publish-notify)* so async colour/decoration catch-up paints promptly. The
   actor already exposes `paint_request: Arc<Notify>`; the TUI subscribes and
   multiplexes it with a terminal-input reader.

### Rejected: UI-thread shadow editor

Maintain a UI-thread copy of the active document's text+cursor and render from it
without touching the actor. Rejected (heuristic #1): the actor already publishes
the char synchronously, so coalescing + an RPC-free draw deliver the latency win
without the cost of dual state (two sources of truth, sync bugs, a second edit
path). Big change, no marginal benefit over the target above.

## Workload decomposition — sync-critical vs. async-deferrable

The keystroke path must do the **minimum** before the typed char paints; everything
derived is eventual. Classification of `dispatch(Insert)`:

| Work | Sync for the char? | Status |
| --- | --- | --- |
| Rope edit (`do_insert_text`) | **Yes** | sync, ~µs |
| Cursor update | **Yes** | sync, ~µs |
| `ensure_cursor_visible` (scroll) | only if caret left viewport (rare/char) | sync, usually no-op |
| Sync `DisplayMatrix` rebuild (edited line text-current, B2.3) | **Yes** (the glyph) | sync, ~4µs (O(window)) |
| Publish the active-document delta | **Yes** (one) | see "publish path" below |
| `maybe_reparse_syntax` (request) | No (colour eventual) | async (channel send; parse `spawn_blocking`) |
| cells / vrows worker recompute | No | async (LSP runtime) |
| syntax highlight, inlay, diagnostics, doc-highlight, LSP on-type, completion | No | already async |

**Finding:** everything that *should* be async already is. The insert path was never
blocked by deferrable work — it was blocked by **publish cost** (see below).

## Latency goal: imperceptible, not a number

The goal is **not** a fixed budget — a number is a cap that reads as "done at X" and
can't express *best possible*. The goal is that the typed char appears with latency
the user **cannot perceive**, indistinguishable from the terminal/compositor echoing
the key itself. Three *examinable* parts replace any single target:

- **Aspiration — match-or-beat the best-in-class reference.** vim in the TUI, the
  compositor in GPUI. Verifiable side-by-side at any time (it's how the
  2026-06-05 stale-text bug was caught: "vim is instant, we're not"); "best
  possible", not a line you cross once.
- **Hard ceiling — within one display frame, always.** The glyph lands on the next
  refresh regardless of background load (8.3ms @120Hz, 16ms @60Hz). This is display
  *physics*, not a target: below one frame, faster *output* is imperceptible, so the
  contract is to never *miss* a frame — which is exactly what the non-blocking
  architecture (no I/O / parse / shape on the UI thread) guarantees.
- **Ratchet — CI fails on regression.** The measured keystroke→glyph distribution
  (the `LATTICE_PERF_INPUT` harness, generalised) is recorded; the bar only moves
  down, toward the I/O-hardware floor we can't beat. The current value is
  *descriptive* — ≈0.7ms p50 (TUI, debug build, 2300-line file) — never a finish
  line.

- **Decorations (syntax / inlay / diagnostics): eventual, ≤ 1–2 frames.** Explicitly
  allowed to lag the text; already async. The typed char is synchronous; colour
  catches up. (The keystroke UX contract: only the edited line may change per key,
  pixel-stable elsewhere, the glyph appears immediately — text synchronous, recolour
  eventual.)

## The publish path: amplification → coalescing → per-substate

Measurement (typing `design.md`): one `dispatch(Insert)` triggered **~6 publishes**,
each a whole-world `build_render_state` (~150–400µs) + a wake of *both* workers (12
recomputes/keystroke). Two layers of waste, two fixes:

1. **Amplification → publish coalescing (slice I.4, landed).** Setters publish
   individually ("write field; publish"), which compounds when chained in one
   dispatch. A depth-guarded batch makes a dispatch publish **once**. 6 → 1
   publish/keystroke; 12 → 2 worker wakes. Correct because a dispatch is one logical
   transition. **Heuristic-#3 check:** rejected "strip publish from the setters" —
   they're also called standalone and must publish then; the depth-guard keeps both
   correct.
2. **Whole-world rebuild → per-substate publication (slice I.5, the sub-ms fix).**
   Even coalesced, a keystroke pays one whole-world `build_render_state`. The
   destination (already documented on `build_render_state`: "3b/3c will replace this
   with per-subsystem publication… this whole-world rebuild method retires") is to
   publish only the **active-document substate** on a keystroke — a few Arc swaps,
   sub-ms. **Heuristic #1:** this is the genuinely-better long-term design *and* the
   plan; option "(B) make `build_render_state` cheap" was **rejected** as polishing a
   method we've decided to delete.

## Prior art — vim / neovim / helix / zed

Weighted per `feedback_editor_design_references` (Helix/Zed heaviest for this
substrate decision; Vim as the latency bar; Neovim for the channel-boundary
precedent).

- **Vim** (single-thread C): typeahead buffer drains *all* pending input before
  redraw (= I.1); edit is a direct mutation; `update_screen` redraws only changed
  lines and the terminal layer **cell-diffs**; syntax is viewport-scoped + per-line
  cached. Sub-ms on ancient hardware because the critical section is tiny + output
  is diffed.
- **Neovim** (core↔UI msgpack channel — closest to our actor): UI sends input, core
  processes and emits **incremental grid deltas**, UI doesn't block. Tree-sitter/LSP
  async, fed back as more redraw events.
- **Helix** (Rust, tui-surface): one thread does input+edit+render; tokio handles
  LSP/IO into the same `select!`. Per key: mutate `Rope`, re-render viewport into a
  `Surface`, **cell-diff**. **No actor/RPC on the edit path.**
- **Zed** (Rust, GPUI): edit applied **synchronously on the main thread** (Rope/
  SumTree, ~µs); GPU re-renders visible elements at frame rate (frame-batches input);
  **all** derived work (syntax `SyntaxMap`, LSP, git) on background executors,
  `cx.notify()` when ready (eventual).

**What they agree on:** tiny synchronous critical section; all derived work async;
render O(viewport) + cell-diff (or GPU-retained); input coalesced. **What none do:**
rebuild a whole render-state snapshot per keystroke — our pre-I.4/I.5 behaviour. So
the convention *mandates* I.4 (publish once) + I.5 (publish only what changed).

**On the actor boundary:** Helix/Zed edit on the render thread (no RPC); we block the
UI on the actor per keystroke, closest to Neovim's channel (though Neovim's UI
doesn't block). Two legitimate shapes:
- **(i) Keep the actor, make per-keystroke work minimal (I.5).** Blocking RPC then
  costs ~round-trip + a few Arc swaps ≈ sub-ms. Preserves goal #4 (single-writer
  core) with least churn. **Chosen.**
- **(ii) Edit on the UI thread (Helix/Zed model).** Lower overhead but a core
  rearchitecture backing out the actor's single-writer ownership. **Held as a
  measured fallback only** — reach for it only if (i) provably can't hit sub-ms after
  I.5. Heuristic #1: no demonstrated need over (i); adopting it now would be rewrite
  for novelty.

## Cross-references

- Slice plan + status: `docs/dev/operations/slice-plans/input-latency.md`.
- Binding→action keymap structure: `keymap-architecture.md`.
- Synchronous char availability: display-line slice plan, B2.3
  (`docs/dev/operations/slice-plans/archive/display-line.md`).
- Actor model: `crates/lattice-host/src/editor_actor.rs`.
