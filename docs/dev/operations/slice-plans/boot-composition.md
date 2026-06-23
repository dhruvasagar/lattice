# Boot composition — slice plan

Design fragment: `docs/dev/architecture/boot-composition.md` (the *what* + *why*;
this file owns the *when* + *in what order*).

Restructure `editor_boot.rs` from a god-function into **Phase A (generic
primitives) → Phase B (per-subsystem `install(boot_ctx)`)**, with a `BootContext`
that bundles the primitives and bakes the `async_landed` wake into an
`inbound::<T>` primitive. Cross-cutting + boot is load-bearing, so every
migration is **behaviour-pinned before it moves**.

## Sequencing

- **BC.0 — Design seed.** ✅ This plan + the design fragment.

- **BC.1 — `BootContext` skeleton + `inbound::<T>` primitive.** Additive, no
  migration. Define `BootContext` as a typed bundle of the *existing* primitives
  (event bus, registries, tick-callback registry, buffer-store / diagnostics
  handles, render-state cell, `async_landed`, runtime). Add the `inbound::<T>`
  primitive: a channel whose `send` calls `async_landed.notify_one()` and whose
  items are drained per-tick via the tick-callback registry through a handler.
  **Tests:** `inbound` send wakes (a fake `Notify` is notified); the per-tick
  drain runs the handler in order; dropped receiver → send error (graceful);
  `wake_on_event` fires the wake. No `editor_boot.rs` behaviour change yet.

- **BC.2 — Regression-pin tests (gate for all migrations).** For *each* subsystem
  (LSP, multibuffer, emacs-keys, diff, terminal, claude-code) pin its current
  boot behaviour: its modes are registered, its commands resolve by name, its
  services are present under the right `TypeId`, and one representative
  off-keystroke async path wakes `run_tick_pending` (no keypress). These are the
  guard rails; nothing migrates until its pin test is green on the *current* code.

- **BC.3 — Phase split + first migration (claude-code).** Restructure boot into
  Phase A (all primitives, incl. forwardable cells for `render_state_arc` /
  `BufferStore` seated at renderer wiring — the §5 crux) + Phase B. Migrate the
  Claude Code peer first (newest, smallest): collapse `spawn` +
  `register_claude_code_modes` + `register_claude_code_ex_commands` + the service
  registration + `install_read_services` + the I3 inbound wake into a single
  `lattice_claude_code::install(boot)`. The deferred `install_read_services`
  disappears (no late handle). **Green:** claude-code pin tests (BC.2) + all
  existing claude-code tests, unchanged behaviour.

- **BC.4 … BC.N — Migrate each remaining subsystem, one per slice.** terminal →
  emacs-keys → diff → multibuffer → LSP (LSP last: largest surface — its inbound
  buses [`InboundShowDocument`, `InboundApplyEdit`, configuration] become
  `inbound::<T>` calls, its `set_wake` / L1c forwarders become `wake_on_event`).
  Each slice: green against that subsystem's BC.2 pin tests; one `install(boot)`
  replaces its scattered calls.

- **BC.final — Remove dead scaffolding.** Retire the hardcoded `run_tick_pending`
  drains the tick-callback registry now subsumes, and the ad-hoc wake forwarders
  the `inbound` / `wake_on_event` primitives replace. Confirm `editor_boot.rs` is
  the two-list shape (primitives, then the Phase-B `install` list). Confirm the
  acid test holds: a hypothetical new subsystem touches boot in one place.

## Risks / decisions (carry into the slices)

1. **Phase-A hoisting (BC.3)** is the highest-risk step: forwardable cells for
   `render_state_arc` / `BufferStore` must preserve *exactly when* content goes
   live (pre-seat reads behave as today — empty/`None`, never panic). Pin this
   with a test that reads each primitive *before* renderer wiring.
2. **Migrate newest→oldest** (claude-code first, LSP last) so the riskiest,
   largest surface moves once the pattern is proven on small subsystems.
3. **No behaviour change** is the contract of every migration slice — BC.2 pin
   tests are the arbiter. A slice that changes behaviour is a bug, not progress.
4. `inbound::<T>` must keep the *optimistic-ack + validation* semantics the I3
   drain uses (ok=true on valid map, ok=false on unknown target) so the
   claude-code rebase is behaviour-preserving.

## Status

BC.0 ✅ · BC.1 🗒 · BC.2 🗒 · BC.3 🗒 · BC.4+ 🗒 · BC.final 🗒

**Resume after I3.** I3 lands on the current wiring with the wake already baked
into `ClaudeCodeInboundBus::send`, so claude-code is migration-ready at BC.3.
