# Boot composition — generic-primitives-first + per-subsystem `install`

`editor_boot.rs` is a ~1500-line function that accumulates per-subsystem wiring.
Every async subsystem (LSP, multibuffer, emacs-keys, diff, terminal, the Claude
Code IDE peer) hand-wires the **same six things**: mode registration, command
registration, service registration, an `async_landed` wake, a per-tick drain,
and a deferred install for handles that don't exist yet when the subsystem's own
handle is created. This is a maintainability + *correctness-by-discipline* smell.
It is **not** a runtime-UX cost (boot runs once; the per-tick drain aggregate is
sub-µs, see `benchmarks.md` I1.1) — but it makes good UX a less *reliable*
outcome and the host harder to evolve. This fragment specifies the restructure.
Sequencing: `docs/dev/operations/slice-plans/boot-composition.md`.

## 1. The two concrete pains

- **Boot-ordering hazard.** Several generic handles are created *late* because
  they depend on renderer-created state — `render_state_arc` (the published
  `Arc<ArcSwap<RenderState>>`), the renderer's `BufferStore`, and from them the
  `DiagnosticsQueryHandle` + the `TickCallbackRegistry` service. But a
  subsystem's *own* handle is needed *early* (its ex-commands capture it, and the
  `CommandRegistry` is frozen early). The collision forces fragile two-phase
  wiring — e.g. the Claude Code peer spawns its server early but must call
  `handle.install_read_services(buffer_store, diagnostics)` *late*, once those
  generic handles finally exist. The ordering is implicit and easy to break.

- **Wake-robustness is by-discipline.** Off-keystroke async work only reaches the
  screen if something calls `async_landed.notify_one()` so the actor runs
  `run_tick_pending` (the §12 / B-series event-driven wake). Today each subsystem
  must *remember* to wire that — `lsp_diagnostics.set_wake(...)`, the
  `MultibufferExcerptsReady` forwarder task, the L1c LSP-refresh forwarders. The
  `editor_boot.rs` L1c comment documents the bug class verbatim: forget it and a
  refresh "only reaches the screen on the next keypress." Paramount goal #4 wants
  async-correctness *architectural, not by discipline* — this is by discipline.

## 2. Move 1 — generic primitives first, then subsystems

Restructure boot into two ordered phases:

- **Phase A — primitives.** Create *every* host-exposed generic primitive up
  front: the event bus, the mode / command / service registries, the
  tick-callback registry, the buffer-store handle, the diagnostics-query handle,
  the published render-state cell, the `async_landed` wake handle, the runtime
  handle. Handles that currently depend on renderer state are created as
  **forwardable cells** (`Arc<ArcSwap<…>>` / `Arc::default()`) in Phase A and
  *seated* when the renderer wiring runs — so the *handle* exists early even
  though the *content* goes live later. (This is the trickiest part; see §5.)

- **Phase B — subsystem install.** Each subsystem self-installs through **one**
  crate-owned entry point, `Subsystem::install(boot: &mut BootContext)`, which
  does all of its wiring against the Phase-A primitives. Order-independent: every
  primitive already exists, so there is no "late handle" and no deferred install.

`editor_boot.rs` collapses to two lists: build the primitives, then
`for s in subsystems { s.install(boot) }`.

## 3. Move 2 — a `BootContext` that hands over primitives *and* bakes in robustness

`BootContext` is the host's generic-primitive surface made explicit. Beyond the
registries, it exposes the operations that are easy to get wrong as *primitives
that cannot be wired without their safety property*:

- `inbound::<T>(handler) -> Sender<T>` — **the bundled inbound primitive.**
  Creates a channel whose **`send` wakes `async_landed`** (the wake is inside the
  sender — structurally impossible to forget) and whose items are drained
  per-tick via the tick-callback registry, each run through `handler`
  (validate → map to existing `Effect` → resolve oneshot). This generalizes
  LSP's hand-rolled inbound buses *and* the Claude Code peer's `ClaudeCodeInbound`
  into one primitive.
- `wake_on_event::<E>()` — subscribe a typed event and wake `async_landed`;
  generalizes the `MultibufferExcerptsReady` / L1c forwarder tasks.
- `tick_callback(closure)` — register a per-tick drain (the I1 registry).
- `register_mode` / `register_command` / `register_service`.

The acid test becomes **structural**: a new subsystem touches `editor_boot.rs` in
exactly one place — its entry in the Phase-B list — and zero host internals.

## 4. Paramount-goal alignment

> **UX (higher court):** no runtime-UX change (boot is startup-only; the inbound
> wake + drain are the *same* mechanism, just bundled). The win is that good UX
> becomes the *reliable* outcome — the off-keystroke-wake bug class is designed
> out, not guarded by review.
> **#1 perf:** unchanged. Same tasks/subscriptions, fewer ad-hoc; the per-tick
> cost is identical.
> **#2 extensibility:** a new subsystem (incl. the future WASM plugin host)
> installs through the uniform `BootContext` — the cleanest extension seam.
> **#4 async:** the wake is a *property of the primitive*, so off-keystroke
> delivery is guaranteed by construction — meeting the "architectural, not by
> discipline" bar #4 demands.
> **Heuristic #1 (long-term fit, on merit):** this is the genuinely-better
> design — it removes a real bug class and a real ordering hazard, and relocates
> wiring into the owning crate (mode-ownership). Not novelty; the merit is named.

## 5. Risks / hard parts

- **Cross-cutting.** Touches LSP, multibuffer, emacs-keys, diff, terminal,
  claude-code. Each migrates one slice at a time, behaviour-pinned first (§ slice
  plan BC.2).
- **Phase-A hoisting — re-assessed (the "forwardable cell" worry was wrong).**
  An earlier draft of this fragment expected `render_state_arc` and the
  renderer's `BufferStore` to be "created during renderer wiring", requiring
  *forwardable cells* seated later. Reading the code (2026-06-23, BC.3 planning)
  corrected that: `editor_boot.rs` is renderer-agnostic; `render_state_arc` is
  `Arc::new(ArcSwap::from_pointee(RenderState::default()))` — a default-init cell
  populated at runtime by `publish_render_state` — and the `BufferRegistry` is
  created + immediately seeded with the initial document and shared by `Arc`
  clone. Neither depends on anything created before it, so Phase-A hoisting is a
  mechanical `let`-binding reorder, **not** a seat-content-later refactor. The
  actual hazard is narrower: **preserve the "same Arc identity" invariants** —
  `render_state_arc`, `async_landed`, and the overlay/cells worker cells are
  shared by Arc identity across worker spawns, Editor fields, and service
  registrations; reconstruct one instead of moving it and the worker's writes
  stop being observable through `RenderState::load_full`. The genuinely
  structural facts are that `ServiceRegistry` is created *and* populated late
  (it reads mid-boot values) and `CommandRegistry` freezes mid-boot before its
  `Arc` is consumed — both handled by sequencing, not forwardable cells.
- **Boot is load-bearing.** A behaviour change here ripples everywhere; the
  regression-pin tests (BC.2) are mandatory *before* each migration, not after.

## 6. Rejected alternatives

- **Leave boot as-is.** Works, but the wake bug class stays by-discipline, the
  ordering hazard stays, and each subsystem keeps scattering ~6 calls. "It works /
  it's well-tested" is explicitly *not* sufficient (heuristic #1).
- **A wiring macro per subsystem.** Hides the scatter but fixes neither ordering
  nor ownership; cosmetic.
- **A full runtime DI container.** Over-engineered. `BootContext` is a *typed
  bundle* handed to compile-time-known subsystems, not a runtime registry of
  everything.

## 7. Relationship to the IDE-protocol work

The Claude Code peer (I-series) is the immediate motivator and the **first
migration target** (newest + smallest surface). I3 (write tools) lands on the
*current* two-phase wiring but bakes the `async_landed` wake into
`ClaudeCodeInboundBus::send` already — so the I-series rebases onto the
`BootContext` `inbound::<T>` primitive cleanly at BC.3 with no behaviour change.
