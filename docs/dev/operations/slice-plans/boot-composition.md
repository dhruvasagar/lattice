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
    ✅ Landed in two commits: **(1/2)** `74496960` — the additive `BootContext`
    API surface (`buffer_store` / `diagnostics` read handles, owned
    `CommandRegistry` / `ModeRegistry` / `ServiceRegistry` behind `Option`,
    `commands_mut` / `modes_mut` / `services_mut` + `register_service` seams,
    `freeze_*` take-on-freeze, register-after-freeze panics). **(2/2)** — the
    `editor_boot.rs` rewire: a delineated Phase-A block builds the primitives
    (`event_bus`, `runtime_handle`, `async_landed`, `tick_callbacks`,
    `render_state`, `buffers` + `buffer_store_handle`, `diag_query`) and
    `BootContext::new`; all command / mode / service registration routes
    through `boot.commands_mut()` / `boot.modes_mut()` /
    `boot.register_service()`; the `services: { … }` field is hoisted out of
    the `Editor` literal as statements on `boot`; the three `freeze_*` calls
    hand back the `Arc`s the literal seats. **Green:** 14 BC.2 pins + 756
    `lattice-host` lib tests + the boot-driven integration suites + the full
    workspace build (incl. the GPUI peer); zero behaviour change.
    **Deviation (behaviour-neutral):** the `ModeRegistry` is frozen *before*
    `register_mode_toggle_commands`, not after — the toggle helper borrows
    `&mut CommandRegistry` + `&ModeRegistry` at once and both live in `boot`,
    so freezing modes first hands back an `Arc<ModeRegistry>` (derefs to
    `&ModeRegistry`). The registry is fully populated either way, so the
    generated toggles are identical; `boot_context.rs`'s freeze-order doc was
    corrected to match. **Deferred to BC.3b:** `into_registrations()` → the
    `Editor` — no `boot.inbound()` / `boot.tick_callback()` is wired at BC.3a,
    so the hand-off is empty and `boot` drops harmlessly after the last freeze;
    BC.3b (claude-code's inbound bus) is the first non-empty registration and
    adds the Editor field + hand-off then.

    Original plan (retained for reference):
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

  - **BC.3b — `lattice_claude_code::install(boot)` (first migration).** ✅
    Landed. claude-code's **five scattered `editor_boot` sites** (server `spawn`,
    `register_claude_code_ex_commands`, `register_claude_code_modes`, the
    `ClaudeCodeServerHandle` service register, and the late `install_services`
    read/write wiring) collapse into **one Phase-B line**:
    `lattice_claude_code::install(&mut boot)`. The I3 write bus is **rebased onto
    `boot.inbound::<ClaudeCodeInboundRequest>(handler)`** (decision 3-a) — the
    bespoke `ClaudeCodeInboundBus` + `make_drain` are deleted; `inbound.rs` now
    owns only the payload + `make_handler` (the per-item map → Effect + oneshot
    resolve), the generic primitive owns the channel/drain/wake. The drain's
    registration token rides `boot.into_registrations()` onto the new
    `Editor._boot_tick_registrations` field (this is the slice that makes
    `into_registrations` load-bearing, deferred from BC.3a).

    **Circular-dependency resolution (the gap the original plan missed).**
    `install(boot)` must be *crate-owned* (mode-ownership), but `BootContext`
    lives in `lattice-host`, which depends on `lattice-claude-code` → a cycle.
    Decided with Dhruva (2026-06-23): introduce a **`SubsystemBoot` trait in
    `lattice-mode`** (below every subsystem crate, zero new deps) exposing the
    generic install surface (`commands_mut` / `modes_mut` / `services_mut` /
    `register_service` / `service::<T>` / `inbound` / `wake_on_event` /
    `tick_callback` / `event_bus` / `runtime_handle` / `buffer_store`);
    `BootContext` implements it; installs take `&mut impl SubsystemBoot`. The
    LSP-specific diagnostics handle is reached via the generic
    `boot.service::<DiagnosticsQueryHandle>()` (the trait names no lattice-lsp
    type); the host registers it as a Phase-A service. **Deviation from the
    original prose:** used the existing `commands_mut`/`modes_mut` seam rather
    than adding `register_command`/`register_mode` wrappers (the registration fns
    take `&mut CommandRegistry`/`&mut ModeRegistry`, so wrappers would be
    redundant); and dropped `BootContext`'s typed `diagnostics()` accessor (its
    only consumer now uses the generic `service` lookup — substrate-vs-helper).
    Chose the trait (not moving `BootContext` down) per Dhruva — keeps the
    concrete bundle in the host, subsystems depend on the capability.

    **Placement:** the install list sits after the inline mode block + before the
    mode freeze, so an installed mode is present when
    `register_mode_toggle_commands` enumerates the registry and both registries
    are still open. Adding subsystem #N+1 = write its `install()` + add one line
    to this list. **Green:** all 14 BC.2 pins (claude mode + `claude-code-start/
    -stop` + `ClaudeCodeServerHandle` + inbound→wake-covered) + the full
    `lattice-claude-code` suite (43) + 756 `lattice-host` lib + `lattice-mode`
    (117) + the full workspace build incl. the GPUI peer; behaviour unchanged.

- **BC.4 — Migrate `terminal`.** ✅ Landed. `lattice_terminal::install(boot)`
  collapses terminal's mode registration (`register_terminal_modes`) into one
  Phase-B line. A **thinner** migration than claude-code: terminal's only
  host-side *wiring* was mode registration. Two touch-points stay host-side, by
  design, and are NOT mode-ownership violations:
  - the `TerminalStoreHandle` service is a **host-published primitive** (the
    host `BufferRegistry` exposed under `dyn TerminalStore`; `impl TerminalStore
    for BufferRegistry` lives in `lattice-host`) — sibling to `buffer_store` /
    `diagnostics`, host owns the data + the terminal *mode* consumes it via
    `services.get`. The `SubsystemBoot` surface can't carry a terminal type, so
    it stays host-registered. Terminal owning it = a follow-up that impls
    `TerminalStore` over `BufferStoreHandle` (terminal-crate slice).
  - the `Editor`-coupled invocation runner (`Editor::run_terminal_invocation`)
    is the **shared** invocation-runner mechanism (Help / Oil / FileTree /
    Terminal all bind `Editor::run_*_invocation`, deeply `&mut Editor`-coupled);
    migrating it is a cross-cutting cleanup, not a terminal slice.
  **Green:** 14 BC.2 pins (incl. `terminal_modes_registered_at_boot` +
  `terminal_service_present_at_boot`) + 42 `lattice-terminal` + 756
  `lattice-host` lib + full workspace build incl. the GPUI peer.

- **BC.5 — `emacs-keys` reclassified as a `lattice-mode` builtin.** ✅ Landed.
  Resolved the BC.5 fork (emacs-keys was a *host module*, not a crate) by
  Dhruva's directive: **`emacs-keys-mode` is a builtin mode, and the home for
  builtin modes is `lattice-mode`.** The whole module moved to
  `lattice-mode/src/emacs_keys_mode.rs` (named for parity with the mode) — the
  mode AND `emacs_keys_layer_bindings`, since every keymap-trie type it needs
  (`KeymapTrie`/`BoundCommand`/`KeymapLayer` from `lattice-keymap`,
  `ChordPattern` from `lattice-protocol`, `CommandRegistry`/`CommandInvocation`
  from `lattice-grammar`) lives below `lattice-mode`. `EmacsKeysMode` now
  registers with `register_foundation_modes` (it is a builtin), so it **leaves
  the Phase-B install list entirely** — the host's `register_emacs_keys_modes`
  boot line is gone. The host keeps only the keymap-layer **push** (it owns the
  live `KeymapHandle` + reads `config` for prefix/enable), at both sites
  (editor_boot keymap block + dispatch `:set` re-push), now calling
  `lattice_mode::emacs_keys_layer_bindings` / `EmacsKeysMode::mode_id`. Tests
  moved too (Tier-2 registers the `action:*` pane names inline, no host dep).
  **Green:** 14 BC.2 pins (incl. `emacs_keys_mode_registered_at_boot`) + 10
  `emacs_keys_dispatch` + 123 `lattice-mode` (+6 moved) + 750 `lattice-host` lib
  + full workspace incl. GPUI.

- **BC.6 — `diff` extraction.** ✅ COMPLETE (DX.0–DX.final ✅, 2026-06-24). Slice
  plan: **`docs/dev/operations/slice-plans/archive/diff-extraction.md`**; design
  fragment: **`docs/dev/architecture/diff-extraction.md`**. The
  own-crate-vs-builtin fork resolved to **extract the whole diff subsystem into
  the existing `lattice-diff` crate** (Dhruva, 2026-06-23) — diff is a real
  subsystem (DiffSubsystem + resolver + overlay/fold/filler render providers +
  modeline element + bridge), too machinery-heavy for a `lattice-mode` builtin.
  It now installs through the `SubsystemBoot` seam (`lattice_diff::install(&mut
  boot)`, one Phase-B line) and decomposes into `diff-mode` +
  `diff-conflict-mode`. **What landed:** DX.1 regression-pin gate (7 + 2 pins);
  DX.2–DX.5 cross-cutting prep (move-down-first: `resolve_syntax_style` →
  **lattice-syntax** [not lattice-theme — the planned home was falsified on
  merit], RowMapper → lattice-core, `HunkFoldProvider` → mode-owned
  `HunkFoldSource` [C7], `ROLE_MODE_ITEM` → lattice-mode, name-based diff keymap
  [C10/DX.5]); **DX.6** the 6-file `git mv` into lattice-diff (façade
  re-export, since dispatch.rs alone has 119 `crate::diff::` refs — rewiring is
  deferred) with the C6 resolver split (traits travel, `BufferRegistry`-backed
  impls stay host in `diff/resolver.rs`); **DX.7** `lattice_diff::install`
  (terminal pattern — modes via install; keymap-push + subsystem-bind +
  modeline element stay host-side as documented residue, NOT half-migrations);
  **DX.8** the `diff-conflict-mode` shell + `DiffSignKind::Conflict` activation
  predicate (forward-looking — resolution chords deferred). **Green:** 228
  lattice-diff tests + 560 host lib + 7 DX.1 / 14 BC.2 pins; full build (TUI +
  GPUI). **Post-BC.6:** MO.x ✅ (2026-06-24) migrated the diff `do`/`dp` keymap
  to `DiffMode::keymap()` (host K.2.4 pass owns the push; explicit host push +
  `diff_mode_layer_bindings` retired). Follow-up (NOT BC.6) **✅ landed in CR.x**
  (2026-06-24): `diff-conflict-mode` resolution chords
  (`d2o`/`d3o`/`d2p`/`d3p`/`dB`) + bridge-driven activation — see
  `slice-plans/archive/diff-conflict-resolution.md`. CR.1 also fully mode-owned `do`/`dp`
  (deleted `Editor::do_diff_*` + `Action::Diff*`), closing the diff
  mode-ownership acid test.

- **BC.7 … BC.N — Migrate the remaining subsystems, one per slice.** multibuffer
  → LSP (LSP last: largest surface — its inbound buses [`InboundShowDocument`,
  `InboundApplyEdit`, configuration] become `inbound::<T>` calls, its `set_wake`
  / L1c forwarders become `wake_on_event`). Each slice: green against that
  subsystem's BC.2 pin tests; one `install(boot)` replaces its scattered calls.
  (Residual classes seen so far: host-published primitives a mode merely
  consumes stay host-side [BC.4]; a default-on builtin with no owning crate
  becomes a `lattice-mode` builtin, not a subsystem install [BC.5]; a real
  subsystem living in-host gets extracted to its own crate [BC.6].)

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

BC.0 ✅ · BC.1 ✅ · BC.2 ✅ · BC.3a ✅ · BC.3b ✅ · BC.4 ✅ · BC.5 ✅ · BC.6 ✅ · BC.7+ 🗒 · BC.final 🗒

**Decisions locked (2026-06-23):** BC.3 split into BC.3a (hoist) + BC.3b
(claude-code); **2-b** — `BootContext` owns the three registries + typed
`register_*` helpers (done right the first iteration, no transitional
registry-param `install`); **3-a** — claude-code's I3 write bus rebases onto the
generic `boot.inbound::<T>` (delete `ClaudeCodeInboundBus`). Boot-time only; zero
UX/perf impact.

**BC.6 ✅ COMPLETE (2026-06-24)** — the diff subsystem is extracted into
`lattice-diff` as `diff-mode` + `diff-conflict-mode`, installed via
`lattice_diff::install(&mut boot)`. See
**`docs/dev/operations/slice-plans/archive/diff-extraction.md`** for the DX.0–DX.final
record (coupling table, the DX.2 lattice-syntax deviation, the DX.6 façade +
C6 resolver split, the DX.7 terminal-pattern residue).

**Next: BC.7+** migrate
multibuffer → LSP
(LSP last, largest: its `InboundShowDocument`/`InboundApplyEdit`/configuration
buses become `boot.inbound::<T>`, its `set_wake`/L1c forwarders become
`boot.wake_on_event`); each green against its BC.2 pins, one `install(boot)` per
subsystem. **BC.final** then retires the `*_mut` transitional accessors + the
hardcoded `run_tick_pending` drains, leaving `editor_boot` as the two-list shape
(Phase-A primitives, then the Phase-B install list).
