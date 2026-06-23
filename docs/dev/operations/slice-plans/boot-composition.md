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

- **BC.1 — `BootContext` skeleton + `inbound::<T>` primitive.** ✅ Additive, no
  migration. Landed as:
  - `lattice-mode/src/inbound.rs` — the reusable primitive: `InboundBus<T>`
    (manual `Clone` so `T` need not be `Clone`) whose `send` wakes the editor +
    `make_inbound(wake, handler) -> (InboundBus<T>, TickCallback)`. Generalizes
    the I3 `ClaudeCodeInboundBus`; the wake is baked into the sender. Pairs with
    the I1 `tick_callback` registry. (`tokio` promoted dev → regular dep for
    feature `sync`.)
  - `lattice-host/src/boot_context.rs` — `BootContext` bundling
    `event_bus` / `tick_callbacks` / `async_landed` / `runtime_handle`, with
    `inbound::<T>(handler)`, `wake_on_event::<E>()`, `tick_callback(cb)`, and
    `into_registrations()` (boot-lifetime RAII tokens handed to the `Editor` at
    BC.3).
  - **Scope call (heuristic #1):** the `render_state` cell / `BufferStore` /
    `DiagnosticsQueryHandle` are **deferred to BC.3** — their correct shape is a
    *forwardable cell* (the §5 crux), so eager fields here would bake the wrong
    primitive. The mode/command/service registration helpers likewise land at
    BC.3 against the live registries.
  - **Tests (10):** 6 in `inbound.rs` (drain runs handler in order; empty drain;
    handler may drop items → no effect; `send` wakes; dropped receiver → graceful
    `Err(item)`; `Clone` without `T: Clone`) + 4 in `boot_context.rs` (inbound
    drains via the tick registry + `send` wakes; tick-callback token retained;
    `into_registrations` hand-off is the lifetime anchor; `wake_on_event` fires
    the wake). No `editor_boot.rs` behaviour change. **No bench:** `send` is an
    `mpsc::send` + `Notify::notify_one` (upstream-benched); the per-tick drain
    cost is already covered by `lattice-mode/benches/tick_callback.rs`.

- **BC.2 — Regression-pin tests (gate for all migrations).** ✅ Landed as
  `crates/lattice-host/tests/boot_regression_pins.rs` (14 pins) against the
  *current* code. For each subsystem (LSP, multibuffer, emacs-keys, diff,
  terminal, claude-code) it pins:
  - **Modes registered** — `mode_registry.is_registered(ModeId::new(name))` for
    all 8 LSP modes, `multibuffer-mode`, the 3 terminal modes, `diff-mode`,
    `emacs-keys-mode`, `claude-code-mode`.
  - **Subsystem-wired commands resolve** — `registry.lookup_by_name`:
    `claude-code-start` / `-stop` (claude-code), `narrow` / `widen` (multibuffer
    narrow provider). LSP's `lsp-*` commands come from the generic
    `ex_commands::populate`, NOT LSP-subsystem boot wiring, so they are not a
    meaningful "LSP boot" pin — excluded by design.
  - **Services present under the right `TypeId`** — `services.get::<T>()` with the
    EXACT register-site `T` (the Arc/TypeId rule): `LspSupervisorHandle`,
    `DiagnosticsQueryHandle`, `LspLogger`, `TerminalStoreHandle`,
    `MultibufferRegistryHandle`, `ClaudeCodeServerHandle`, plus the generic
    Phase-A primitives `Arc<EventBus>`, `BufferStoreHandle`,
    `TickCallbackRegistryHandle`, `ActionHandlerRegistryHandle`,
    `CommandRegistryHandle`.
  - **Off-keystroke wake** — publishing a boot-subscribed typed event on
    `editor.event_bus` wakes `editor.async_landed` with no keystroke
    (`LspInlayHintRefresh` → LSP refresh forwarder; `MultibufferExcerptsReady` →
    multibuffer forwarder). The claude-code inbound→wake is already covered by
    `lattice-claude-code` + the BC.1 `inbound` tests; terminal/diff wakes fire
    from `on_activate`/subsystem tasks (not boot-wired without activation), so
    they are out of scope for a *boot* pin.

  These are the guard rails; nothing migrates until its pin is green on the
  current code. `Editor::boot(scratch)` is cheap + side-effect-free (LSP attach
  is lazy), so the pins are plain `#[test]` (wake pins `#[tokio::test]`).

- **BC.3 — Phase split + first migration (claude-code).** Split into **BC.3a**
  (Phase-A hoist, behaviour-identical) + **BC.3b** (claude-code migration) — the
  hoist is plumbing, the migration is behaviour-sensitive; mixing them makes a
  regression hard to localize (heuristic #4: land each slice green). **No UX /
  perf impact: all of this is boot-time-only; registration is not a hot path, so
  runtime is untouched (paramount #1 preserved by construction).**

  **Crux re-assessment (the §5 "forwardable cell" worry does not apply).**
  `editor_boot.rs` is renderer-agnostic; `render_state_arc` (821) is
  `Arc::new(ArcSwap::from_pointee(RenderState::default()))` — a default-init cell
  populated at runtime by `publish_render_state`, depending on nothing before it;
  `BufferRegistry` (767) is created + immediately seeded with the initial doc,
  `Arc`-shared via `Clone`; `async_landed` (706) / `tick_callbacks` (1153) are
  `Arc::default()` / `Arc::new(...)`. **None are "created during renderer
  wiring."** Phase-A hoisting is therefore a mechanical `let`-binding reorder that
  preserves Arc identities, NOT a seat-content-later refactor. The real
  structural facts that shape the work are different: the `ServiceRegistry` is
  created *and* populated late (1220–1352), and the `CommandRegistry` is frozen
  (`Arc`-wrapped) mid-boot before the registry Arc is consumed (picker registry,
  document handles). See risk #1 (rewritten).

  - **BC.3a — Phase-A hoist; `BootContext` owns the registries (decision 2-b).**
    Open `Editor::boot` with a delineated **Phase-A block** creating the
    BootContext primitives up front: `event_bus`, `runtime_handle`,
    `async_landed`, `tick_callbacks`, `render_state_arc`, `BufferRegistry`
    (+`buffer_store_handle`), `diag_query`, and the three registries
    (`CommandRegistry`, `ModeRegistry`, `ServiceRegistry`). Extend `BootContext`
    (BC.1-deferred): add `buffer_store: BufferStoreHandle`,
    `diagnostics: DiagnosticsQueryHandle`, **ownership of the three registries**,
    the typed extension helpers `register_mode` / `register_command` /
    `register_service` (the public seam migrated subsystems use), and the
    transitional `modes_mut()` / `commands_mut()` / `services_mut()` accessors
    (the seam un-migrated inline boot code uses until its subsystem migrates;
    removed at BC.final once nothing inline remains). Route **all** inline
    registration through `boot` (the 2-b cost, accepted: do it right once rather
    than thread `&mut` registry params we'd unwind later). The `ServiceRegistry`
    instance is created in Phase A but **populated** where the 1220 block sits
    today (it reads mid-boot values); `CommandRegistry` is frozen via
    `boot.freeze_command_registry()` after all command registration, before the
    Arc is consumed. Editor literal seats the frozen registries + primitives from
    `boot`. **Green:** all 14 BC.2 pins + full `lattice-host` suite + workspace
    build; zero behaviour change. **Risk:** the "same Arc identity" invariants the
    boot comments stress (overlay/cells cells, render_state) — move the `let`s,
    never reconstruct; BC.2 wake pins + render/decoration tests catch a break.

  - **BC.3b — `lattice_claude_code::install(boot)` (first migration).** Add
    `pub fn install(boot: &mut BootContext)` collapsing claude-code's five
    scattered sites (`spawn` 242, `register_claude_code_ex_commands` 271,
    `register_claude_code_modes` 356, `install_services` 1254, service register
    1315) into one Phase-B call, against Phase-A primitives only:
    `boot.register_command` / `register_mode` / `register_service`, `buffer_store`
    + `diagnostics` for the I2 read tools, and the I3 write bus **rebased onto
    `boot.inbound::<ClaudeCodeInboundRequest>(handler)`** (decision 3-a) — delete
    the bespoke `ClaudeCodeInboundBus`, which `InboundBus<T>` was modeled on. The
    deferred `install_services` vanishes (no late handle). **Green:** the BC.2
    claude-code pins (mode + `claude-code-start/-stop` + `ClaudeCodeServerHandle`
    + inbound→wake) + all existing `lattice-claude-code` tests, behaviour
    unchanged. `inbound::<T>` keeps the I3 optimistic-ack semantics (risk #4).

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

1. **Phase-A hoisting (BC.3a) — re-assessed.** The original "forwardable cell"
   worry (render_state / BufferStore created during renderer wiring, content
   seated later) does NOT match the code: both are default-init / early-seeded
   `Arc`-shared cells (see BC.3 crux re-assessment). The actual risk is narrower
   and mechanical: **preserve the "same Arc identity" invariants** when moving
   the `let` bindings (the overlay/cells worker cells, `render_state_arc`,
   `async_landed` are shared by Arc identity across worker spawns + Editor fields
   + service registrations — reconstruct one and the worker's writes stop being
   observable through `RenderState::load_full`). Move bindings, never
   re-`Arc::new`. The BC.2 wake pins + existing render/decoration tests are the
   guard. Secondary: `ServiceRegistry` is created Phase-A but populated late
   (reads mid-boot values); `CommandRegistry` freezes mid-boot before its Arc is
   consumed — sequence `boot.freeze_command_registry()` accordingly.
2. **Migrate newest→oldest** (claude-code first, LSP last) so the riskiest,
   largest surface moves once the pattern is proven on small subsystems.
3. **No behaviour change** is the contract of every migration slice — BC.2 pin
   tests are the arbiter. A slice that changes behaviour is a bug, not progress.
4. `inbound::<T>` must keep the *optimistic-ack + validation* semantics the I3
   drain uses (ok=true on valid map, ok=false on unknown target) so the
   claude-code rebase is behaviour-preserving.

## Status

BC.0 ✅ · BC.1 ✅ · BC.2 ✅ · BC.3a 🚧 · BC.3b 🗒 · BC.4+ 🗒 · BC.final 🗒

**Decisions locked (2026-06-23):** BC.3 split into BC.3a (hoist) + BC.3b
(claude-code); **2-b** — `BootContext` owns the three registries + typed
`register_*` helpers (done right the first iteration, no transitional
registry-param `install`); **3-a** — claude-code's I3 write bus rebases onto the
generic `boot.inbound::<T>` (delete `ClaudeCodeInboundBus`). Boot-time only; zero
UX/perf impact.

**Resume after I3.** I3 lands on the current wiring with the wake already baked
into `ClaudeCodeInboundBus::send`, so claude-code is migration-ready at BC.3.

**Next: BC.3** — Phase split (Phase A: all generic primitives, incl. forwardable
cells for `render_state_arc` / `BufferStore` seated at renderer wiring — the §5
crux) + first migration (claude-code): collapse its `spawn` +
`register_claude_code_modes` + `register_claude_code_ex_commands` + service
registration + `install_services` + the I3 inbound wake into a single
`lattice_claude_code::install(boot)` against BC.1's `BootContext`. **Green
bar:** the `boot_regression_pins.rs` claude-code pins (mode + commands +
`ClaudeCodeServerHandle` service + inbound→wake) stay green, behaviour
unchanged.
