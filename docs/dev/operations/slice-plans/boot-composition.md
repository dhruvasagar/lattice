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

- **BC.7 — `multibuffer` migration.** ✅ COMPLETE (2026-06-24). multibuffer was
  *already its own crate* (`lattice-multibuffer`), so — unlike BC.6 — there is
  **no extraction**; BC.7 is the lighter terminal/diff-shaped migration. The
  host's ~6 scattered `editor_boot` sites collapse into one Phase-B line
  (`lattice_multibuffer::install(&mut boot)`, in
  `crates/lattice-multibuffer/src/install.rs`):
  - **modes** — `register_multibuffer_modes` (+ its `DocumentClosed` cleanup
    subscriber), `register_narrow_mode`, `register_project_search_mode`;
  - **commands** — the excerpt-jump motions (`]e`/`[e`/`]E`/`[E`), the
    `:multibuffer-*` / `:narrow` / `:widen` / `:search` ex-commands, and the
    `zn` narrow operator SPEC;
  - **services** — the `MultibufferRegistryHandle` + the project-search service;
  - **wake** — the `MultibufferExcerptsReady`→`async_landed` forwarder becomes
    `boot.wake_on_event::<MultibufferExcerptsReady>()` (the wake is now a
    property of the primitive, not a hand-rolled mpsc task).

  Search-provider registrations move behind the **crate's own**
  `#[cfg(feature="search")]` (the gate travels to the code it guards; the host's
  `install(boot)` call is unconditional).

  **New residual class — a crate-owned registry handle created *inside*
  `install`.** The `MultibufferRegistryHandle` carries **no host-state
  dependency** (unlike diff's resolver-backed `DiffSubsystemHandle` or terminal's
  host-`BufferRegistry`-backed `TerminalStoreHandle`, both of which the host must
  construct), so `install` creates it locally and publishes it as a service; the
  host reads it back via `services.get::<MultibufferRegistryHandle>()` at dispatch
  time (`Editor::resolve_narrow_target`). This is *better* than the host-published
  pattern where there's no host-state tie.

  **Decision A (the one fork, locked with Dhruva 2026-06-24):** the host's
  universal `zn` operator-pending binding resolves its `OperatorId` **by name**
  (`registry.id_by_name("operator:narrow")`) instead of threading the
  registration return value — the K.2.5 motion name-resolution pattern. This
  severs the id-threading coupling and lets the operator SPEC live entirely in
  `install`.

  **Residue staying host-side (NOT mode-ownership violations):** (1) the
  universal `zn` operator *binding* at the `Builtin` operator-pending layer — `zn`
  is universal grammar (composes with the resolved `Builtins`), the same category
  as `lattice-syntax`'s structural text objects (N.1.4c); the *spec* is
  mode-owned, only the binding is host-side, and there is no single owning mode
  for a universal operator. (2) `Editor::resolve_narrow_target` + the
  `AppEffect::{SearchTrigger,NarrowTrigger,NarrowLines,NarrowWiden,MultibufferExpand}`
  dispatch arms — Effect-vocabulary-is-the-host-boundary (diff's `do_diff_*`
  precedent); the trigger substrate fns are crate-owned helpers those arms call.
  (3) the generic `event_bus` *service* stays a Phase-A host primitive (many
  subsystems consume it). **Green:** 14 BC.2 pins (4 multibuffer, incl. the
  `wake_on_event`-migrated `multibuffer_excerpts_ready_wakes_async_landed_off_keystroke`)
  + 118 `lattice-multibuffer` + 562 `lattice-host` lib + 9
  `multibuffer_is_a_regular_buffer` + 14 `diff_regression_pins` + full workspace
  build incl. the GPUI peer. Zero behaviour change; boot-time-only, no UX/perf
  impact.

- **BC.8 — LSP migration (last + largest, sub-sliced BC.8a–e).** LSP is too
  large + intricate for one slice; sub-sliced so risk is contained per slice.

  **Design finding (re-evaluated at execution, per the slice-plan rule):** the
  original plan said "its inbound buses [`InboundApplyEdit`, `InboundShowDocument`,
  configuration] become `inbound::<T>` calls." Reading the code falsified the
  *uniform* version of that on merit — the four inbound drains
  (`drain_inbound_apply_edits` → `Vec<RendererSignal>`, `…show_documents`,
  `…show_message_requests`, `…configuration_requests`) are **deep `&mut Editor`
  methods** (apply workspace edits to the user's buffers, open docs, drive a
  picker), and the buses carry **no wake** today. The four are **heterogeneous in
  reply shape**: configuration = pure read→reply; show-document = optimistic-ack
  → `OpenBufferAt`; apply-edit = real-outcome reply (optimistic-ack with
  pre-validation, a documented fidelity trade); show-message-request = **deferred
  user choice** via a picker (the optimistic-ack `Vec<Effect>` shape does NOT fit
  — forcing it would be unsound). **Decision (Dhruva, 2026-06-24): full reshape —
  behaviour change is acceptable; the test is soundness + mode-ownership, not the
  no-behaviour-change contract.** The reshape *extends* the blessed claude-code I3
  pattern (inbound request → mode-owned handler maps to a generic `Effect` +
  resolves the oneshot); the Effect-boundary layering (no `lsp_types` in
  `lattice-grammar`) is respected because the handler resolves the oneshot and
  emits existing generic Effects. **UX/perf:** off the keystroke hot path
  (server-initiated); the new wakes are a UX *improvement* (off-keystroke
  repaint, not a regression); perf is sub-µs registry relocation. UX risk
  concentrates in BC.8d (apply-edit mutates user buffers) + BC.8e (interactive
  picker), behaviour-pinned per slice.

  Sub-slices:
  - **BC.8a — foundation.** ✅ COMPLETE (2026-06-24). `lattice_lsp::install(&mut
    boot)` (`crates/lattice-lsp/src/install.rs`): the LSP modes
    (`register_lsp_log_modes` + `register_lsp_completion_mode`, which reads the
    `LspSupervisorHandle` via `boot.service::<…>()` — the handle is a host-created
    Phase-A service, the diff `DiffSubsystem`-bind residue class) + the four
    `workspace/*/refresh` wakes → `boot.wake_on_event::<E>()` (byte-identical to
    the retired L1c `wake_on` forwarders). Behaviour-preserving. Residue
    host-side: `build_lsp_subsystem` (produces Editor fields), the host-created
    services (logger / diagnostics-query), the four inbound buses + drains
    (reshaped in BC.8b–e). **Green:** 14 BC.2 pins (incl. `lsp_modes_registered`,
    `lsp_services_present`, `lsp_refresh_event_wakes_async_landed`) + full
    `lattice-lsp` suite + 562 host lib + full GPUI build.
  - **BC.8b — configuration.** ✅ COMPLETE (2026-06-24). Reshaped onto
    `boot.inbound::<InboundConfigurationRequest, _>` — the pattern-setter. The
    bespoke `ConfigurationBus` is now a type alias for the generic
    `lattice_mode::inbound::InboundBus<InboundConfigurationRequest>` (its `send`
    wakes the editor → off-keystroke reply); the drain logic moved into the
    mode-owned `lattice_lsp::configuration::make_handler` (pure read → reply over
    the shared `lsp.*` tree, **no Effect**), registered via `boot.inbound` in
    Phase A (pre-spawn so the supervisor fans the bus to its actors). Deleted:
    `Editor::drain_inbound_configuration_requests` + `pending_configuration_rx`
    field + the TUI drain wrapper + 4 TUI tests (coverage moved to lattice-lsp).
    `lsp_config_tree` is now shared (`Arc<ArcSwap<toml::Table>>`) so the handler
    reads current config (loader `store`s on reload). **Pattern for c/d/e:** bus
    → `InboundBus<T>` alias; `dispatch`→`send`; `build_lsp_subsystem` takes the
    bus param (drops the `*_rx` return); host wires `boot.inbound(make_handler)`
    in Phase A; delete the Editor drain + field. **Green:** 14 BC.2 pins +
    lattice-lsp 195 (3 new handler tests) + 562 host lib + TUI 185 lsp / 5
    lifecycle-config + full GPUI build.
  - **BC.8c — show-document.** ✅ COMPLETE (2026-06-24). Reshaped onto
    `boot.inbound::<InboundShowDocument, _>` (the 8b pattern): `ShowDocumentBus`
    is now a type alias for the generic `InboundBus`; `actor.rs`
    `bus.dispatch`→`bus.send` (now wakes off-keystroke); the mode-owned
    `lattice_lsp::show_document::make_handler(LspLogger)` maps the 4 request
    shapes (external / non-file / file±selection / malformed) to an open
    [`Effect`] + an optimistic reply; `build_lsp_subsystem` takes the bus +
    `logger` params (logger created in Phase A so the handler captures an
    Arc-shared clone); `Editor::drain_inbound_show_documents` +
    `pending_show_document_rx` field + the TUI drain wrapper + 2 TUI tests
    deleted (coverage moved to lattice-lsp).
    **Design finding (re-evaluated at execution, per the slice-plan rule):**
    the "→ `Effect::OpenBufferAt`" sketch was falsified on merit twice over.
    (1) **The selection conversion needs the open buffer.** LSP positions are
    UTF-16 code units; Lattice stores UTF-8 byte offsets; the conversion needs
    the target line's text, which only exists *after* the open. `OpenBufferAt`
    carries a pre-converted byte the handler can't compute. (2) **The async
    path discards peer-applied effects.** show-document drains through the
    generic inbound tick-callback (`drain_tick_callbacks` → `apply_effect_host`),
    which returns only `renderer_signals` and drops `out.effects`; the host
    `handle_effect` no-ops `OpenBuffer`/`OpenBufferAt` (they're peer-applied on
    the keystroke path). So an open emitted there would be silently dropped.
    **Resolution (Dhruva, 2026-06-24 — Option 1, contained):** two NEW
    **host-applied** generic effects in `lattice-grammar`:
    `OpenExternalUri { uri }` (the relocated OS-handler spawn) and
    `OpenBufferAtColumn { path, column: Option<Utf16Pos>, force }` (open +
    optional UTF-16-column cursor, converted host-side in `handle_effect`
    against the opened line via `move_cursor_to_utf16_column`). `Utf16Pos`
    is a plain `{ line, col }` — no `lsp_types` in the grammar. They run
    host-side (do_edit + signals to `out.renderer_signals`), so they work on
    the off-keystroke async path by construction; the active-slot swap is
    reflected by the next render-state publish. Both peers no-op them
    (TUI/GPUI parity); the 4 exhaustive `effect_mutates*` sites classify them
    non-mutating. **Side-finding (out of scope, follow-up):** claude-code I3's
    `openFile` emits `Effect::OpenBufferAt` from this same tick path → also
    silently dropped (its tests only assert the oneshot, not the open). Fixing
    it (general "forward tick-path effects to the peer" Option 2, or migrating
    I3 to `OpenBufferAtColumn`) is a separate slice. **Green:** 14 BC.2 pins +
    lattice-lsp 200 (6 new show_document handler tests) + 562 host lib + 2 new
    `show_document_open_effect` host pins (incl. the café `col 9 → byte 10`
    conversion) + lattice-ui-tui 183 lsp + full GPUI build.
  - **BC.8d — apply-edit.** ✅ COMPLETE (2026-06-24). UX-sensitive (mutates user
    buffers), but the only behavioural change is the **off-keystroke wake** — the
    apply path, reply, field, and mid-tick drain ordering are byte-identical to
    before. **Design finding (re-evaluated at execution):** the I3/8b/8c
    "handler → generic `Effect`" pattern does NOT extend — the apply
    (`Editor::apply_inbound_workspace_edit` → `apply_lsp_text_edits` / `do_edit`)
    is irreducibly `&mut Editor` AND carries `lsp_types::WorkspaceEdit`, which
    cannot cross the `Effect` boundary into a mode-owned handler. The slice
    plan's "optimistic-ack with pre-validation" + "move into `handle_effect`"
    sketch was falsified: optimistic-ack would *downgrade* today's real-outcome
    reply, and a generic effect can't carry the edit. **Resolution (A3, locked
    with Dhruva after a UX/perf review):** add a *host-drained* variant to the
    inbound primitive — `lattice_mode::inbound::make_inbound_raw(wake) ->
    (InboundBus<T>, Receiver<T>)` (+ inherent `BootContext::inbound_raw`). The
    bus's `send` still wakes (the whole win — applyEdit was the L1c
    "lands-on-next-keypress" bug class); the host keeps the
    `pending_apply_edit_rx` field + `drain_inbound_apply_edits` in
    `run_tick_pending` (the apply is documented host residue — the diff-lifecycle
    / multibuffer Effect-arm class). `ApplyEditBus` becomes a type alias for
    `InboundBus<InboundApplyEdit>`; the bespoke struct/`new`/`dispatch` are
    deleted; `actor.rs` `dispatch`→`send`; `build_lsp_subsystem` takes the bus +
    drops the `apply_edit_rx` return. **No new `Effect`** (vs the rejected
    trigger-effect A1 — keeps the Effect vocabulary free of an internal pump) and
    **no peer/classification changes**. Real-outcome reply preserved.
    **UX/perf:** strict UX improvement (immediate off-keystroke apply); off the
    keystroke hot path; ~1 extra cheap `run_tick_pending` per (rare) applyEdit,
    N-batched edits coalesce to one wake. **Green:** 14 BC.2 pins + lattice-mode
    inbound 7 (incl. `raw_send_wakes_and_receiver_gets_item`) + lattice-lsp
    apply_edit + 562 host lib + lattice-ui-tui 3 `drain_inbound_apply_edits_*`
    (real apply preserved) + full workspace build incl. the GPUI peer.
  - **BC.8e — show-message-request** (the genuine wall: deferred user choice →
    needs a **host-published choice/picker primitive** that `lattice-lsp` drives;
    forcing optimistic-ack would be unsound. Mechanism to be settled when reached.)

  (Residual classes seen so far: host-published primitives a mode merely
  consumes stay host-side [BC.4]; a default-on builtin with no owning crate
  becomes a `lattice-mode` builtin, not a subsystem install [BC.5]; a real
  subsystem living in-host gets extracted to its own crate [BC.6]; a crate-owned
  handle with **no** host-state dependency is created *inside* `install`, not
  host-published [BC.7]; a universal-grammar operator/text-object keeps its
  *spec* in the crate but its *binding* host-side at `Builtin`, resolved by name
  [BC.7].)

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
   claude-code rebase is behaviour-preserving. **BC.8 caveat:** the LSP inbound
   buses are NOT uniform optimistic-ack — see the BC.8 design finding. config =
   pure read→reply; show-document/apply-edit = optimistic-ack (apply-edit with
   target pre-validation, a documented fidelity trade); show-message-request =
   deferred user choice (does NOT fit optimistic-ack → host-published picker
   primitive). The "no behaviour change" contract (#3) is *waived* for BC.8 by
   Dhruva's decision (full reshape adds off-keystroke wakes); the BC.2 LSP pins
   still guard modes/services/refresh-wakes, and each inbound sub-slice adds its
   own behaviour pins.

## Status

BC.0 ✅ · BC.1 ✅ · BC.2 ✅ · BC.3a ✅ · BC.3b ✅ · BC.4 ✅ · BC.5 ✅ · BC.6 ✅ · BC.7 ✅ · BC.8a ✅ · BC.8b ✅ · BC.8c ✅ · BC.8d ✅ · BC.8e (LSP inbound: show-message) 🗒 · BC.final 🗒

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

**BC.7 ✅ COMPLETE (2026-06-24)** — multibuffer (already its own crate; no
extraction) migrated to `lattice_multibuffer::install(&mut boot)`: modes +
commands (motions, `:narrow`/`:widen`/`:search`, the `zn` operator SPEC) +
services (`MultibufferRegistryHandle`, project-search) + the
`MultibufferExcerptsReady` wake (now `boot.wake_on_event::<…>()`). The registry
handle is crate-owned (created *inside* install — no host-state dependency).
Decision A: the universal `zn` binding resolves `operator:narrow` by name
host-side (severs id-threading); residue host-side = the universal `zn` *binding*
at `Builtin` + the `AppEffect::{Search,Narrow,MultibufferExpand}` dispatch arms.

**BC.8a ✅ COMPLETE (2026-06-24)** — LSP foundation: `lattice_lsp::install(&mut
boot)` registers the LSP modes (completion mode reads `LspSupervisorHandle` via
`boot.service`, a host-created Phase-A service) + the four `workspace/*/refresh`
wakes → `boot.wake_on_event`. Behaviour-preserving; 14 BC.2 pins + lattice-lsp
suite + 562 host lib + GPUI build green. The inbound-bus reshape (the design
finding: 4 heterogeneous `&mut Editor` drains, Dhruva's full-reshape decision) is
sub-sliced BC.8b–e.

**BC.8b ✅ COMPLETE (2026-06-24)** — configuration reshaped onto
`boot.inbound::<T>`; the pattern-setter for c/d/e (bus → `InboundBus<T>` alias,
`dispatch`→`send`, `build_lsp_subsystem` takes the bus param, host wires
`boot.inbound(make_handler)` in Phase A, delete the Editor drain + `*_rx` field).
mode-owned handler in `lattice_lsp::configuration`; `lsp_config_tree` shared
(`Arc<ArcSwap>`). Green across BC.2 pins + lattice-lsp + host lib + TUI + GPUI.

**BC.8c ✅ COMPLETE (2026-06-24)** — show-document reshaped onto
`boot.inbound::<T>`. The "→ `Effect::OpenBufferAt`" sketch was falsified on
merit: the async/tick path discards peer-applied effects (`drain_tick_callbacks`
drops `out.effects`; host `handle_effect` no-ops `OpenBuffer*`) AND the UTF-16
selection conversion needs the opened line. Resolved (Option 1, Dhruva) with two
NEW **host-applied** generic effects — `OpenExternalUri { uri }` +
`OpenBufferAtColumn { path, column: Option<Utf16Pos>, force }` (host-side
UTF-16→byte conversion via `move_cursor_to_utf16_column`). Mode-owned handler in
`lattice_lsp::show_document::make_handler`; `Utf16Pos` is a plain `{line,col}` —
no `lsp_types` in the grammar. Follow-up (separate slice): claude-code I3's
`openFile` shares the latent tick-path-effect-drop and should move to
`OpenBufferAtColumn` (or the general "forward tick-path effects to the peer"
fix).

**BC.8d ✅ COMPLETE (2026-06-24)** — apply-edit reshaped onto the generic bus
via the new *host-drained* variant `make_inbound_raw` (+ `BootContext::inbound_raw`).
The apply is irreducibly `&mut Editor` + carries `lsp_types::WorkspaceEdit`
(can't cross the `Effect` boundary), so — unlike show-doc — there is no
mode-owned handler and no new `Effect`: the bus contributes only the
off-keystroke wake; the host keeps `pending_apply_edit_rx` + the
`drain_inbound_apply_edits` mid-tick drain (documented host residue, real-outcome
reply preserved). Chosen (A3) over the trigger-effect shape (A1) after a UX/perf
review — A3 keeps the Effect vocabulary clean, preserves the exact mid-tick
ordering, and degrades nothing (strict off-keystroke UX win).

**Next: BC.8e (show-message-request)** — the genuine wall: deferred user choice
→ needs a host-published choice/picker primitive that `lattice-lsp` drives
(optimistic-ack is unsound for a deferred choice). **BC.final** then retires the
`*_mut` transitional accessors + the hardcoded `run_tick_pending` drains, leaving
`editor_boot` as the two-list shape (Phase-A primitives, then the Phase-B install
list).
