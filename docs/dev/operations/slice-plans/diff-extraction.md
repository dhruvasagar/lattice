# Diff extraction — slice plan (`lattice-host::diff` → the `lattice-diff` crate, as diff modes)

**Status:** ✅ COMPLETE (DX.0–DX.final ✅, 2026-06-24). This is **BC.6** of the
boot-composition initiative (`docs/dev/operations/slice-plans/boot-composition.md`),
carved into its own file because it is a multi-slice cross-crate extraction, not
a one-line `install` migration like BC.3b/BC.4/BC.5. The host-side diff
subsystem (7 files) now lives in `lattice-diff` as `diff-mode` +
`diff-conflict-mode`, installed through the `SubsystemBoot` seam
(`lattice_diff::install(&mut boot)`). **MO.x ✅ (2026-06-24, post-BC.6):** the
diff `do`/`dp` keymap migrated to `DiffMode::keymap()` (the multibuffer
`keymap_entry!` pattern), pushed by the host's generic K.2.4
`translate_mode_keymaps` pass under `MinorMode(diff-mode)` — the bespoke
explicit host `push_layer` + `diff_mode_layer_bindings` are retired, so the host
pushes nothing diff-specific. Arbiter: the DX.1 chord pins
(`diff_get_put_chords_bound_on_diff_mode_layer` + `..._inactive_when_not_active`)
pass via the translate path. One follow-up remains (NOT part of BC.6): the
`diff-conflict-mode` resolution chords + bridge-driven activation (DX.8 left the
shell + predicate).

**Directive (Dhruva, 2026-06-23):** "properly extract all diff related
functionality into diff modes within `lattice-diff`" — and "identify any
specialized diff features to be modelled within separate minor modes, wherever
it makes sense." Mode decomposition **confirmed**: `diff-mode` +
`diff-conflict-mode`.

**Design fragment (the what/why):** `docs/dev/architecture/diff-extraction.md`
— split out at DX.1 (2026-06-23) per project discipline (design = what/why,
slice plan = when/how). The goal, the crate-vs-builtin rationale + cycle-safety
proof, the full coupling-resolution table (C1–C10), the mode decomposition, and
the module inventory live there. This file owns sequencing + status only;
slice rows below reference the C-couplings by id.

---

## Slice sequence

Each slice lands green + behaviour-pinned. Migrate by *moving types down first*
(so the big file-move in DX.6 is a clean import swap), then the file move, then
the host rewire, then the mode decomposition.

- **DX.0 — design fragment + this slice plan.** ✅ Design + sequence captured.
- **DX.1 — diff regression pins (the gate).** ✅ Landed (2026-06-23) as
  `crates/lattice-host/tests/diff_regression_pins.rs` (7 integration pins, BC.2
  style) + 2 mode-owned unit pins in `crates/lattice-host/src/diff/mode.rs`.
  Pins the current behaviour BEFORE any move:
  - `diff-mode` registered at boot;
  - the `:diff` family resolves via `excommand::resolve_command_name_or_alias`
    (`diff`/`diffoff`/`diffthis`/`diffsplit`/`diffget`/`diffput`/`describe-diff`);
  - the `+N ~M` modeline element (`DIFF_ELEMENT`) registered at boot;
  - `do`/`dp` resolve on the `MinorMode(diff-mode)` keymap layer to the
    diff-get/diff-put actions **and** are INACTIVE when diff-mode is not active
    (K.1.c gating) — via `keymap.resolve_trace`;
  - `DiffSubsystem` bound at boot (`diff_subscription_guard.is_some()`) **and**
    an end-to-end `#[tokio::test]`: publishing `DocumentClosed` on
    `editor.event_bus` for the active buffer's `DocumentId` drains to
    `note_buffer_closed` (proves the bind targets the editor's bus + the real
    `BufferRegistryDocumentResolver`, which a bare `is_some()` could not);
  - sign-gutter decorations: `DiffMode::gutter_decorations` projects a
    `DiffSignMap` into `GutterDecoration::Diff` per signed line (mode-owned unit
    test, moves with the mode into `lattice-diff` at DX.6).

  The drain *mechanism* itself stays unit-pinned in `diff/subsystem.rs`
  (`bind_routes_document_changed_to_debounced_recompute` /
  `bind_routes_document_closed_to_drop_session`), which move with the file.
  **Green:** 7 + 2 new pins + 188 diff lib tests + the 14 BC.2 boot pins. These
  are the arbiter for every later slice ("no behaviour change").

- **DX.2 — move `resolve_syntax_style` → `lattice-syntax`** (C5). ✅ Landed
  (2026-06-23). **Deviation from the planned home, decided on merit:** C5 said
  relocate to `lattice-theme` "(pure styling fn over lattice-theme types)" — but
  that reason is FALSE (risk #1 anticipated this): `resolve_syntax_style` /
  `syntax_element_id` take a `lattice_syntax::Style` and map it to a theme
  `ElementId`. Putting them in `lattice-theme` would force the deliberately-tiny
  renderer-hot-path leaf (deps: only arc-swap + tracing) to depend UP on the
  heavy `lattice-syntax` (tree-sitter, lattice-mode, …). The bridge is
  syntax-aware, so it belongs in the higher crate (`lattice-syntax`) depending
  DOWN onto the theme leaf — cycle-free (`lattice-syntax` did not dep
  `lattice-theme`; theme is a pure leaf). Moved both fns + their colour-identity
  tests to `lattice-syntax/src/theme_style.rs`; host keeps a **façade re-export**
  (`pub use lattice_syntax::{resolve_syntax_style, syntax_element_id}` in
  `ui/theme.rs`) so all `lattice_host::ui::theme::` / `crate::ui::theme::` call
  sites (cell builder, both renderers, the diff overlay) are unchanged — the
  overlay's import flips to `lattice_syntax::` for free at DX.6.
  **Consequence for the design §2 dep list:** `lattice-diff` must add
  `lattice-syntax` (the overlay already uses `lattice_syntax::{Lang,
  LangRegistry, oneshot_highlight_lines, StyledSpan, Style}`) — the §2 list
  omitted it. **Green:** `lattice-syntax` builds standalone (no cycle); host +
  `lattice-ui-tui` + `lattice-ui-gpui` build; 4 moved bridge tests + 7 DX.1 pins
  + 33 TUI cell-render tests pass.
- **DX.3 — split into C8 (✅) + C7 (re-scoped).**
  - **C8 — `RowMapper` → `lattice-core`.** ✅ Landed (2026-06-23). The trait is
    `map_row(&self, usize, usize, u32) -> u32` — zero lattice-type coupling — so
    it sits in `lattice_core::ui::pane` beside its `PaneGroupId`/`PaneId`
    identity siblings. Host `pane_group.rs` re-exports it
    (`pub use lattice_core::ui::pane::RowMapper`); the `PaneGroup` registry +
    `Identity`/`Offset` impls stay host-side; `HunkRowMapper` impls the moved
    trait (its import flips to `lattice_core::ui::pane::RowMapper` at DX.6).
    **Green:** `lattice-core` + host build; 27 pane-group tests.
  - **C7 — `FoldProvider`/`FoldContext` → `lattice-core`: BLOCKED as planned;
    re-scoped.** `FoldContext` carries `lattice_syntax::SyntaxSnapshot` +
    `lattice_diff::HunkIndex`; `lattice-core` (bottom of the graph, deps only
    `lattice-protocol`) cannot reference either without a cycle (`lattice-syntax`
    already deps `lattice-core`; `lattice-diff` will at DX.6). **Real
    resolution:** the only non-host `FoldProvider` impl is `HunkFoldProvider`;
    `lattice-core` already has a `FoldSource` trait
    (`compute_folds(&self) -> Vec<Fold>`, no context) that multibuffer's fold
    providers impl, wrapped by the host's `FoldSourceAdapter` + registered via
    `FoldOverlayService`. So C7 becomes: convert `HunkFoldProvider` from a
    context-driven host `FoldProvider` into a self-contained
    `lattice_core::FoldSource` (holding a diff-session handle), registered via
    `FoldOverlayService` — the multibuffer pattern, and exactly DX.8's
    "render providers mode-owned". `FoldProvider`/`FoldContext` then stay
    host-side; nothing relocates. ✅ **Landed (2026-06-24).** Dhruva chose
    **fully mode-owned now** (`diff-mode::on_activate`, pulling DX.8's fold
    work into C7). What changed:
    - `diff/fold.rs`: `HunkFoldProvider` (context-driven `FoldProvider`) →
      `HunkFoldSource` (`lattice_core::FoldSource` holding `Arc<DiffSession>`,
      per-buffer id via `HUNK_FOLD_NAMESPACE`); `compute_folds` reads
      `session.current_hunks()`. `fold_from_hunk` kept as the tested core.
    - `diff/mode.rs`: `DiffMode::Guard = DiffModeGuard { fold_registrations }`
      + `Drop` removes the source; `on_activate` pulls
      `FoldOverlayServiceHandle` + the new `DiffSubsystemHandle` from
      `ctx.service`, looks up the session, registers the source (mirrors
      `MultibufferMode::on_activate`).
    - `diff/subsystem.rs` + `editor_boot.rs`: `DiffSubsystemHandle =
      Arc<DiffSubsystem>` registered as a Phase-A service.
    - `fold_provider.rs`: pre-seed removed from `with_builtins`;
      `FoldContext.diff_hunks` field + the `lattice_diff` import GONE (the host
      fold substrate no longer depends on `lattice-diff`); pre-seed test
      inverted.
    - `dispatch.rs`: `recompute_folds` drops the `diff_hunks` load. **Behaviour
      preservation:** the `Manual && overlays==0` early-return (DEAD pre-C7 —
      the pre-seed kept overlays ≥ 1) now drops stale overlay-sourced folds
      (`identity: Some`) while preserving hand-curated `zf` folds
      (`identity: None`), so `:diffoff` doesn't strand hunk folds. The 3 fold
      integration tests migrated to `Editor::boot` + diff-mode activation are
      the no-behaviour-change pin.
    - `benches/fold_recompute.rs` rewritten to the `FoldSource` path.
    **Green:** full `lattice-host` lib suite (747) + DX.1 (7) + BC.2 (14) pins +
    host/TUI/GPUI/`lattice-diff` build.
- **DX.4 — move `ROLE_MODE_ITEM` → `lattice-mode` modeline module** (C9). ✅
  Landed (2026-06-24). The role *modes* tag contributed modeline content with
  (diff's `+N ~M`) now lives in `lattice-mode/src/modeline.rs` beside
  `ModelineRole`; the host's own element roles (`modeline.path`/`position`/
  `lang`/`mode`) stay host-side. Host re-exports it
  (`pub use lattice_mode::modeline::ROLE_MODE_ITEM`) so renderer style maps
  (`ml::ROLE_MODE_ITEM`) + the modeline bench + `crate::modeline` call sites are
  unchanged; diff's import flips to `lattice_mode::modeline` at DX.6. **Green:**
  mode + host + TUI + GPUI build; modeline (10) + diff-mode (13) tests.
- **DX.5 — diff owns its actions** (C10). ✅ Landed (2026-06-24). Converted
  `diff_mode_layer_bindings(actions: &crate::actions::ActionIds)` →
  `diff_mode_layer_bindings(registry: &lattice_grammar::CommandRegistry)`,
  resolving `action:diff-get`/`action:diff-put` **by name** (`id_by_name`) — the
  `emacs_keys_layer_bindings` pattern (BC.5), with the same graceful-skip-on-
  unregistered-name `warn!`. This drops the builder's only host-type dependency
  (`crate::actions::ActionIds`), so it moves to `lattice-diff` with the mode at
  DX.6 unchanged. The call site (`editor_boot.rs`) passes `&registry` (already in
  scope, populated by `actions::populate` above). **Scope note:** the action
  *registration* (`action:diff-get`/`-put` → `AppEffect::Diff{Get,Put}`) stays in
  host `actions::populate` — `AppEffect` is the host-owned effect vocabulary
  (`feedback_effect_vocabulary_is_host_boundary`), so the registration can't move
  to `lattice-diff` without an effect seam (a deferred WASM-stage concern). The
  `ActionIds::diff_get`/`diff_put` fields remain (consumed by the DX.1 pin's
  CommandId assertion + the `populate` reverse-map test); the diff module no
  longer reads them. This faithfully mirrors emacs-keys, which resolves
  host-registered pane actions by name without owning their registration.
  **Green:** 7 DX.1 pins (incl. `diff_get_put_chords_bound_on_diff_mode_layer`,
  asserting the name-resolved id equals `action_ids.diff_get`/`diff_put`) + 2 new
  mode-owned unit pins (`layer_binds_get_and_put_by_name`,
  `missing_actions_degrade_to_empty_layer_no_panic`) + `lattice-host` builds.
- **DX.6 — move the 6 source files into `lattice-diff` + sever couplings.** ✅
  Landed (2026-06-24). Moved `filler`/`fold`/`mode`/`overlay`/`pane_group`/
  `subsystem` via `git mv`; host `diff/mod.rs` is now a **façade** over them
  (the locked DX.2/DX.4 re-export pattern). Chosen over rewiring consumers
  because dispatch.rs alone has **119** `crate::diff::` refs + the TUI/GPUI
  renderers consume `lattice_host::diff::*` — rewiring all those to
  `lattice_diff::` is DX.7's job, not DX.6's. **What landed:**
  - **Cargo deps** (design §2): lattice-diff gained `lattice-core`,
    `lattice-protocol`, `lattice-cells`, `lattice-runtime`, `lattice-mode`,
    `lattice-grammar`, `lattice-keymap`, `lattice-syntax`, `lattice-theme`,
    `arc-swap`, `tokio`, `tracing` (+ `tokio` `test-util` dev-dep for the
    `start_paused` debounce tests). Cycle-free: the `lattice-diff` token in
    `lattice-syntax/Cargo.toml` is a comment, not a dep (verified) — the
    syntax→theme style bridge moved to lattice-syntax at DX.2 *precisely* so
    lattice-diff could reach it.
  - **Coupling severance (compiler-verified, not by inspection):**
    - C1–C4 / Bucket-B host re-exports → true lower-crate homes:
      `crate::chord::*`→`lattice_protocol::chord::*`;
      `crate::keymap::BindingMode`→`lattice_mode::BindingMode`;
      `crate::keymap_trie::{BoundCommand,KeymapLayer,KeymapTrie,LookupResult}`→
      `lattice_keymap::*` and `::ChordPattern`→`lattice_protocol::ChordPattern`;
      `crate::ui::theme::{resolve_syntax_style}`→`lattice_syntax::` (C5/DX.2),
      `crate::ui::theme::{ResolvedTheme,BuiltinElementIds,InMemoryThemeRegistry,
      ThemeRegistry}`→`lattice_theme::`;
      `crate::modeline::ROLE_MODE_ITEM`→`lattice_mode::modeline::` (C9/DX.4);
      `crate::pane_group::RowMapper`→`lattice_core::ui::pane::RowMapper` (C8/DX.3).
    - Bucket-D intra-module `crate::diff::X`→`crate::X`; self-refs
      `lattice_diff::X`→`crate::X`.
    - **C6 resolver split:** the `BufferTextProvider` / `DocumentBufferResolver`
      traits travel with `subsystem.rs`; the two `BufferRegistry`-backed impls
      (`BufferRegistryTextProvider` / `BufferRegistryDocumentResolver`)
      extracted to a NEW host file `crates/lattice-host/src/diff/resolver.rs`
      (they reference the host `BufferRegistry`), re-exported under
      `crate::diff::subsystem::*` so all ~10 construction call sites
      (editor_boot + dispatch) are unchanged.
  - **No Bucket-C surprises:** subsystem.rs's ONLY host references were the two
    resolver impls; no `Editor`/`App`/`AppEffect`/`dispatch` reference in any of
    the 6 files' non-test code. The test modules use only `super::*` +
    `lattice_diff` + an in-module `MockResolver`, so they moved wholesale.
  - **Green:** `cargo build -p lattice-diff` standalone + **226 lattice-diff
    tests** (incl. the moved mode/overlay/fold/subsystem suites); host lib **560
    passed / 0 failed** (was 747 — ~187 diff tests now live in lattice-diff);
    TUI + GPUI build; all benches compile in both crates; **7 DX.1 pins + 14
    BC.2 pins** + diff dispatch/fold tests green.
- **DX.7 — host installs diff via `install(boot)`** (closes BC.6). ✅ Landed
  (2026-06-24). Added `lattice_diff::install(boot: &mut impl SubsystemBoot)`
  (`crates/lattice-diff/src/install.rs`) and a one-line Phase-B entry
  `lattice_diff::install(&mut boot)` alongside claude-code + terminal; the
  Phase-A `register_diff_modes` call (editor_boot ~388) is gone.
  **Scope decided at execution — the terminal pattern, NOT the full collapse**
  the entry above sketched. `SubsystemBoot` deliberately exposes no
  keymap-push or modeline primitive, and the Phase-B install list runs
  *before* the `ModelineService` is created (boot ordering), so three diff
  touch-points stay host-side and are **documented in `install.rs` as
  deliberate, not mode-ownership violations**:
  - the **name-based keymap-layer push** (the emacs-keys/BC.5 pattern: the host
    owns the live `KeymapHandle` + reads the registry; the mode owns the
    binding choice + builder + `do_diff_*` handler bodies);
  - the **`DiffSubsystem` bind** (uses the host `BufferRegistryDocumentResolver`
    via the C6 seam, produces the `diff_subsystem` / `diff_subscription_guard` /
    `diff_forwarders` actor-loop Editor fields) — same category as terminal's
    invocation runner; the subsystem handle is still published as
    `DiffSubsystemHandle` for the mode's `on_activate`;
  - the **`+N ~M` modeline element** registration (ordering — its service is
    created after the install list). The descriptor + formatter are mode-owned;
    only the registration *call* is host-sequenced.
  install itself registers the modes (`register_diff_modes`). Migrating the
  keymap to `Mode::keymap()` (so the K.2.4 pass owns the push, retiring the
  explicit push) was tracked as **MO.x** at DX.7 and **✅ landed (2026-06-24)** —
  the feared minor-mode→`MinorMode`-layer path turned out to be already tested
  (`keymap_mode_contributions` asserts a `ModeKind::Minor` keymap lands on
  `MinorMode(mode_id)`), so the migration was low-risk. **Green:** lattice-diff
  + host build; **7 DX.1 pins + 14 BC.2 pins** (incl. `diff_mode_registered_at_boot`,
  the arbiter that the install-list move preserved registration).
- **DX.8 — mode decomposition** (design §4). ✅ Landed (2026-06-24). The
  render-provider mode-ownership half was already done at **DX.3-C7**
  (`HunkFoldSource` registered from `diff-mode::on_activate`). DX.8 adds the
  **`diff-conflict-mode` shell + activation predicate** — `DiffConflictMode`
  (marker minor, `Guard = ()`, `mode_id = "diff-conflict-mode"`) registered
  alongside `diff-mode` in `register_diff_modes`, plus the pure activation
  predicate `sign_map_has_conflicts(&DiffSignMap) -> bool` (true iff a
  `DiffSignKind::Conflict` region is present). **Deliberately forward-looking
  per design §4:** conflict-*resolution* actions don't exist yet, so DX.8
  establishes the separately-activatable surface **without inventing
  behaviour** — the resolution chords (keep-ours/keep-theirs/keep-both/
  next-conflict) + the bridge-driven activation wiring (consulting the
  predicate the way `DiffModeBridge` gates `diff-mode`) are the tracked
  follow-up. **Green:** 2 new mode tests (`conflict_predicate_detects_conflict_regions`,
  `register_diff_modes_registers_base_and_conflict_modes`) + 14 BC.2 / 7 DX.1
  pins unperturbed by the extra mode.
- **DX.final — four artefacts + cleanup.** ✅ Landed (2026-06-24).
  - **Architecture doc:** `architecture/diff-extraction.md` §4 (mode
    decomposition) + the C6/C10 coupling rows confirmed landed.
  - **Benches:** the overlay/fold/filler/pane-group hot paths were a **pure
    move** (no algorithm change), so viewport-boundedness (paramount #1) is
    preserved by construction; bench coverage is present + compiling in both
    crates — `lattice-diff/benches/recompute.rs` and host
    `diff_subsystem.rs` / `pane_group.rs` / `fold_recompute.rs`.
  - **Graceful error handling:** preserved on the moved seams — the resolver
    `buffer_rope` returns `Option` (None → empty rope, never panics on a
    dropped buffer); `DiffMode::on_activate` logs + skips on a missing fold
    service / diff subsystem / session (`tracing::debug!`, returns an empty
    guard). No actor-loop panic path introduced.
  - **Final verification:** full build (lattice-cli TUI + GPUI peer + diff +
    host); **228 lattice-diff tests** + **560 host lib** + **7 DX.1 / 14 BC.2**
    pins all green.
  - **BC.6 marked ✅** in `boot-composition.md`.

## Risks / open questions (carry into the slices)

1. **`resolve_syntax_style` move (DX.2)** — confirm its body only touches
   `lattice-theme` types (it takes `&ResolvedTheme`, `&BuiltinElementIds`, a
   style); if it reaches any host type, that's a further C-blocker to resolve
   first.
2. **Fold/RowMapper trait homes (DX.3)** — confirm `lattice-core` is the right
   home (it already owns `FoldOverlayServiceHandle`); check no host-only types in
   the trait signatures. `RowMapper` may fit `lattice-cells` better (virtual-row
   substrate) — decide by what its methods reference.
3. **The resolver hand-off (DX.7)** — `DiffSubsystem::bind` needs an
   `Arc<dyn DocumentBufferResolver>` over the host `BufferRegistry`. Mirror
   terminal: host registers the impl as a service / passes it in `install`. This
   is the one genuine host↔diff seam; keep it a clean trait.
4. **`diff-conflict-mode` may be ahead of the code** — if conflict *resolution*
   actions don't exist, DX.8 establishes the mode + activation predicate only,
   and the resolution chords are a tracked follow-up.
5. **No behaviour change** is the contract of every slice — DX.1 pins are the
   arbiter. The overlay/fold/filler render paths are viewport-bounded
   (paramount #1); the move must not turn any of them O(file).
6. **Cross-renderer parity** — diff gutter signs + filler rows + the diff
   modeline element render in BOTH TUI and GPUI. Any `DiffSignKind` / effect /
   theme-field touched in the move updates both peers in the same slice
   (`feedback_tui_gpui_parity`).

## Cross-references

- **Design fragment (what/why + coupling table C1–C10):**
  `docs/dev/architecture/diff-extraction.md`.
- Parent initiative + the `SubsystemBoot` seam diff installs through:
  `docs/dev/operations/slice-plans/boot-composition.md` (BC.6) +
  `docs/dev/architecture/boot-composition.md`.
- The emacs-keys move (BC.5) is the template for C1–C4 + the name-based keymap
  builder (C10): `crates/lattice-mode/src/emacs_keys_mode.rs`.
