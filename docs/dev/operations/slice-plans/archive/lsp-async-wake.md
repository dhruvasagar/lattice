# Slice plan — LSP async-result render-wake unification (AW)

**Design home:** [`../../../architecture/lsp-architecture.md`](../../../architecture/lsp-architecture.md) §12
("Async-result render-wake"). This plan sequences the work that makes
`async_landed` the **single, renderer-agnostic** wake for every async LSP
result. Cross-reference: §12 owns the *what/why*; this file owns the
*when/order*.

## Why

Three user-visible bugs, one root cause:

1. **`gr` (and every one-shot LSP action) often waits for an extra
   keystroke before its picker/jump/popup appears.** "Often, not always"
   is a race: the result sits in its `pending_*_rx` channel until
   *something* runs `run_tick_pending` — the next keystroke, or an
   unrelated `async_landed` (a syntax reparse / diagnostics push) that
   happens to land in the window.
2. **A late result opens anchored to the symbol the cursor has since
   moved off.** In-flight requests are cancelled only when a *new
   same-kind request* supersedes them — never on cursor motion.
3. Audit intent: **every** user-facing LSP surface (`gr`, `gd`/`gD`/`gy`/
   `gI`, `K`, `gl`/code-actions, rename, format, symbols, signature,
   completion, call/type hierarchy, …) must behave correctly off-keystroke.

### Root cause (verified in `crates/lattice-host/src/dispatch.rs`)

The editor actor already has the correct off-keystroke drain:
`async_landed.notified()` → `run_tick_pending()` → `publish_render_state()`
→ (revision-gated) `paint_request.notify_one()` (`editor_actor.rs` §12 arm).
It runs on the single-writer actor thread (paramount #1) and drives **both**
renderers uniformly (each repaints on the `paint_request` wake and drains
`poll_signal`).

But only the **4 passive-decoration** requests (`foldingRange`,
`semanticTokens`, `inlayHint`, `documentHighlight`) fire `async_landed`.
The **18 channel-delivered action requests** deliver via an `mpsc` channel
drained by `drain_pending_*` and fire **nothing** — except `hover`, which
fires the renderer-specific `paint_request` clone. That clone works in the
GPUI peer (its paint bridge calls `run_tick_pending`, `window.rs`) but is a
**no-op in the TUI peer** (TUI's `Wake::Repaint` only redraws; X1 removed
`run_tick_pending` from the per-frame loop). So even `K` has a latent TUI
bug, and §12's "popup overlay owns a `paint_request` clone (already wired)"
row is wrong for the TUI peer.

**The renderer-agnostic contract:** the wake is a *core* concern, not a
renderer concern. Every async LSP result fires `async_landed`; the actor
arm is the *single* drain + publish + paint chokepoint; renderers are pure
consumers of published `RenderState` + forwarded signals.

## The channel-delivered request inventory (AW.1 scope)

All in `dispatch.rs`, all `spawn_on_lsp_runtime` + `tx.send` + **no**
`async_landed` today:

`lsp_references_request` (`gr`), `lsp_nav_request` (`gd`/`gD`/`gy`/`gI`),
`lsp_hover_request` (`K`, currently `paint_request`), `lsp_signature_help_request`,
`lsp_completion_request`, `lsp_document_symbol_request`,
`lsp_workspace_symbol_request`, `do_lsp_code_action_request`,
`do_lsp_format_request`, `do_lsp_rename_request`,
`do_lsp_call_hierarchy_request`, `do_lsp_type_hierarchy_request`,
`do_lsp_moniker_request`, `do_lsp_on_type_formatting_request`,
`do_lsp_insert_completion_request`, `do_completion_resolve_focused`,
`issue_selection_range_request`, `spawn_code_action_resolve_apply`,
`execute_lsp_command`.

## Slices

### AW.1 — `async_landed` on every channel-delivered LSP result  ✅
Clone `self.async_landed` into each spawned task (mirroring the decoration
requests) and fire `async_landed.notify_one()` after **each** terminal
`tx.send(...)` (every arm: `NoServers`, `Found`, empty, error). For `hover`,
**replace** the `paint_request`/`paint_notify` clone with `async_landed`
(the actor arm re-fires `paint_request` when the popup revision moves — the
popup is a `paint_revision` surface, so both peers repaint). Match the
existing `notify_one()`-after-landing shape; **no new guard abstraction**
(per CLAUDE.md "no abstractions beyond the task").
- *paramount:* #1 (result on the frame it arrives), #4 (async correctness
  architecturally, not per-drain-site).
- *test:* actor-loop test — a `references` request whose task hits the
  `NoServers` arm drains + publishes the echo **with no keystroke** (fails
  today: `async_landed` never fires). Failure modes: empty `Found`,
  `NoServers` echo text.
- *doc:* AW.4.
- *error handling:* `notify_one` is permit-style + coalescing; a
  cancelled task's wake is a harmless no-op drain (nothing new in the
  channel → stable revision → no paint).

### AW.2 — cancel position-anchored lookups on cursor move  ✅
Extend the existing `dispatch()` cursor-motion block (the Issue #20
stale-popup dismiss: capture `pre_cursor`, compare after `handle_action`)
to also cancel + clear the position-anchored one-shots — **references, nav
(definition), hover** — when `self.cursor != pre_cursor`. Excludes
whole-buffer decorations (`documentHighlight`/`inlayHint`/`foldingRange`/
`semanticTokens`): they're position-independent and re-fire single-flight
via the `maybe_request_*` pumps. Reuses the established chokepoint — no new
cursor-mutation hooks (cursor is set in 15+ sites; there is no single
setter).
- *paramount:* #1 (no stale picker/jump), UX (never lands off-symbol).
- *test:* fire `references`, move cursor, land a stale `Found` → assert no
  picker opens and the token is cancelled.
- *doc:* AW.4 + §7 (cancellation) cross-ref.

### AW.3 — GPUI paint-bridge unification (drop the renderer-specific drain)  ✅
**Landed in two steps (2026-07-01).**

**Prereq — convert the remaining non-LSP `paint_request` drain paths to
`async_landed`.** The investigation found the GPUI paint bridge's own
`run_tick_pending` was still load-bearing for three **picker** paths that
fired the renderer-specific `paint_request` clone rather than `async_landed`:
`open_picker` (init future), `fire_live_picker_query_changed` (live re-query
future), and `bump_live_picker_debounce` (the debounce *timer* wake — its own
comment noted "the event-driven loop has no timer wake of its own"). Same
latent class as the AW.1 hover bug: they drained only in GPUI (its bridge ran
`run_tick_pending`); the TUI `Wake::Repaint` redrew without draining, so an
idle picker's results/re-query stalled until the next keystroke. All three now
fire `async_landed`, so the actor arm drains `drain_pending_picker_init` /
`drain_pending_live_picker_query` uniformly. No loop: `fire_live_picker_query_changed`
clears `debounce_until` at entry, so `should_fire` goes false and an unrelated
later wake can't re-fire.

**Step 2 — drop the GPUI bridge's `run_tick_pending`.** With every async
drain-path now firing `async_landed`, the actor's `async_landed` arm is the
**single** drain chokepoint (runs `run_tick_pending` + `publish_render_state`
+ fires `paint_request` on the actor thread). Neither peer reads the actor's
`signal_rx` in production — both render from published `RenderState` — and the
now-correct TUI proves `RenderState` is sufficient for every async result.
So the GPUI bridge is simplified to a **pure repaint forwarder**
(`paint_request` → `cx.notify()`), matching the TUI's `Wake::Repaint` arm.
The drain is no longer duplicated in a renderer-specific path.

**Verification.** `lattice-host` lib 609/609, `lattice-ui-tui` lib 1523/1523,
picker suite 9/9, integration 6/6; `lattice-ui-gpui --features window` and the
default workspace build compile clean. The picker→`async_landed` conversion
reuses the mechanism AW.1's tests already cover (async result → actor arm →
off-keystroke drain), and the debounce-clear invariant is verified in code.
**Recommended interactive check** (the editing environment can't run a GPU
window): `cargo run --features gui -- --gui`, then confirm an idle `K` hover
and an idle live-grep picker both surface results WITHOUT an extra keystroke.

### AW.4 — docs: §12 arrival-shapes correction + App::apply-tail audit  ✅
Rewrite §12 "The three arrival shapes": add the missing **mpsc-channel
action-result** shape (references/nav/symbols/code-actions/rename/format/
selection-range/…); correct the **popup-overlay** row (remove the
"owns a `paint_request` clone / already wired" claim — hover now uses
`async_landed`). State plainly: `async_landed` is the single
renderer-agnostic wake; the actor arm is the single drain; renderer paint
bridges are pure consumers, never the primary drain. Fold in the
**App::apply-tail audit** (below).

### AW.5 — App::apply-tail audit conclusion  ✅
`App::apply`'s tail `run_tick_pending` (`app/dispatch.rs`) is a
**synchronous fast-path** on the keystroke that fired the work — it keeps
keystroke-triggered results on the same round-trip (latency). After AW.1
it is no longer the *only* path: `async_landed` covers idle arrivals.
Audit conclusion to record: nothing user-visible must depend **solely** on
this tail; every async result also fires `async_landed`. Keep the tail
(latency win); it is not a bug once AW.1 lands.

## Sequencing

AW.1 → AW.2 (independent, can interleave) → AW.4/AW.5 (docs, after AW.1) →
AW.3 (picker `paint_request` → `async_landed` conversion, then the GPUI
paint-bridge simplification). AW.1 alone fixed issues #1/#3 for **both** peers
via the actor arm; AW.3 also fixed the same latent bug in the **picker** paths
(idle live-grep results/re-query stalling in the TUI) and made the actor arm
the single drain chokepoint. **All slices landed.**

## Status

| Slice | State |
|---|---|
| AW.1 — `async_landed` on every channel-delivered LSP result | ✅ |
| AW.2 — cancel position-anchored lookups on cursor move | ✅ |
| AW.3 — picker paths → `async_landed` + GPUI bridge is a pure forwarder | ✅ (GPUI runtime check recommended) |
| AW.4 — §12 docs + arrival-shapes correction | ✅ |
| AW.5 — App::apply-tail audit conclusion | ✅ |
