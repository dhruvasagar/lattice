# Diff conflict resolution + full mode-ownership — slice plan (CR.x)

**Status:** 🚧 in progress (2026-06-24). CR.0 ✅; CR.1 ✅; CR.2 ✅; CR.3–CR.5 planned. Builds on
BC.6 (diff extraction → `lattice-diff`) + MO.x (diff keymap mode-owned). Design
context: `docs/dev/architecture/diff-extraction.md` §4 (mode decomposition). This
plan owns sequencing + status.

## Why this exists

Two threads, decided with Dhruva (2026-06-24):

1. **`diff-conflict-mode` resolution** — DX.8 left the mode as a shell + activation
   predicate (`sign_map_has_conflicts`). Dhruva chose to build the resolution
   feature now ("Wrap up DX.X" → "Build conflict resolution").
2. **Full mode-ownership** — Dhruva confirmed the whole diff command surface
   should be owned by the diff modes in `lattice-diff`, not the current
   half-migration (keymap moved at MO.x, but handler bodies + `Action`/`AppEffect`
   variants + action/ex-command registration still host-side). Scope **(A) full
   ownership** chosen.

## Locked design decisions

- **Conflict model = 3-way session**, NOT git `<<<<<<<` marker parsing. `HunkKind::Conflict`
  is produced by the 3-way merge path (`compute_diff(&[base, local, remote])`),
  already user-reachable via `:diffsplit <base> <remote>` (dispatch produces a
  `[base, local, remote]` session). No marker-parsing infra exists or is added.
- **Resolution target = local / "ours" (slot 1)** — the editable working side the
  cursor sits in; matches git + the existing 3-way wiring.
- **Chords = vim-fugitive 3-way family** (extends the existing `do`/`dp`, zero
  `c`-operator collision): `d2o`/`d3o` (keep-ours/keep-theirs = diffget from
  local/remote), `d2p`/`d3p` (put), `dB` (keep-both = ours⌢theirs, the one new
  edit), `]c`/`[c` (next/prev conflict). `:diffget`/`:diffput` already take an
  optional `<bufnr>` target — the chords desugar onto that existing target-aware
  path, not a parallel one.
- **Full ownership via `Effect::ApplyEdit`** — the diff handler bodies move off
  `Editor::do_diff_*` + the host `Action::Diff*` enum into mode-owned
  `Mode::action_handlers()` closures (`Fn(&ActionContext) -> Option<Effect>`, the
  snippet/lsp pattern). Today there is NO generic edit-apply `Effect` (only
  `Effect::Edits`, a record), so a mode handler can't drive an arbitrary document
  edit. CR.0 adds a generic `Effect::ApplyEdit` primitive; the diff handlers then
  compute the edit (subsystem is a registered service) and return it. The Effect
  *vocabulary* stays the host boundary by design
  (`feedback_effect_vocabulary_is_host_boundary`); everything else (bindings +
  handler logic + registration + ex-commands) lands in `lattice-diff`. Acid test
  then passes: zero `Editor::` diff methods, zero `Action::Diff*` variants.
- **Migration mechanism (proven by snippet, dispatch.rs:5666):** keep the
  `action:diff-*` CommandSpec shell, **empty** its `AppEffect::Diff*` translation
  arm, and let the mode's `action_handlers()` (registered into the
  `ActionHandlerRegistry`) return the real `Effect`. The `Editor::do_diff_*`
  bodies + `Action::Diff*` variants are deleted.

## Grounding facts (code)

- `compute_get_edit(buffer_id, cursor_row, target) -> DiffGetOutcome` already
  exists; `DiffGetOutcome::Edit { target_buffer_id, edit: lattice_protocol::edit::Edit, post_cursor_row }`.
  `Edit` is a low-crate type both crates see → it's what `Effect::ApplyEdit` carries.
- `apply_edit_blocking(&mut self, edit: lattice_protocol::edit::Edit)` /
  `apply_edit_batch_blocking(Vec<Edit>)` are the host appliers the new
  `Action::ApplyEdit` arm calls + sets the post-cursor row.
- `ActionContext { buffer_id, cursor, services, events }`; handler returns
  `Option<Effect>`. `DiffSubsystemHandle` is already a registered service (DX.3-C7).
- `:diff-accept`/`:diff-reject` (`do_diff_accept`/`do_diff_reject`) + `]c` (NextHunk)
  already exist — they migrate / get reused, not reinvented.

## Slice sequence

- **CR.0 ✅ — generic `Effect::ApplyEdit { target: BufferId, edit, cursor: Option<u32> }`.**
  Landed: `lattice-grammar` effect vocab + host `Action::ApplyEdit` + the
  `Effect::ApplyEdit → Action::ApplyEdit` translation (in `handle_effect`, queued
  on `out.next_actions` — the `AppEffect::DiffGet → Action::DiffGet` round-trip, so
  the edit lands once in the action dispatch) + the applier arm
  (`handle_action` → new `Editor::apply_targeted_edit`). The applier routes
  **active-document** targets through `apply_edit_blocking` (full LSP/syntax/
  highlight pipeline) and **peer** targets through the registry handle + a peer
  `DocumentChanged` (mirrors `do_diff_put`'s peer path, so CR.1's relocation is
  behaviour-preserving); `cursor` parks the active cursor. Cross-renderer parity:
  both `effect_mutates`/`effect_mutates_or_yanks` classifiers (host + TUI) +
  TUI `apply`/`apply_effect_app_arms` no-op bands + GPUI `apply_effect_gpui` band
  updated in the same patch. Error handling: `apply_targeted_edit` returns
  `Result`; the arm logs + leaves the cursor put on failure (peer-closed race →
  `Cancelled`). No hot-path/bench change (not yet wired to a keystroke). Green:
  new `apply_edit_effect_edits_active_buffer_and_parks_cursor` (host lib, proves
  translate-then-apply single-apply) + 561 host lib + 227 lattice-diff + 14 BC.2
  + 7 DX.1 + 15 diff get/put behaviour tests.
- **CR.1 ✅ — migrate `do_diff_get`/`do_diff_put` to `DiffMode::action_handlers()`.**
  Landed: `DiffSubsystem::diff_get_effect`/`diff_put_effect` (lattice-diff) wrap
  `compute_get_edit`/`compute_put_plan` outcomes into `Effect::ApplyEdit` (get →
  active buffer; put → peer `target_buffer_id`) or an error `Echo`
  (`TargetRequired`/`NoPeerBuffer`, wording preserved verbatim) or `None`.
  `DiffMode::action_handlers()` registers `action:diff-get`/`action:diff-put`
  closures that read buffer/cursor from `ActionContext` + the
  `DiffSubsystemHandle` service and call the resolvers (`target = None`),
  auto-registered by the host's existing `register_mode_action_handlers` boot
  walk. Host: `AppEffect::DiffGet/DiffPut` arms emptied to the no-op fallback;
  `Editor::do_diff_get`/`do_diff_put` + `Action::DiffGet`/`DiffPut` **deleted**;
  the `:diffget`/`:diffput` ex-commands rewired to the same resolver + a shared
  `apply_diff_effect_inline` (synchronous, no `next_actions` dependency). Acid
  test passes: zero `Editor::do_diff_*`, zero `Action::Diff*`. **Arbiter green:**
  DX.1 pins (7) + the 15 diff get/put behaviour tests migrated to drive through
  `diff_*_effect` + `apply_diff_effect_inline` + 4 new lattice-diff resolver/
  `action_handlers` tests + 561 host lib + 14 BC.2 pins. (Residue for CR.4: the
  ex-command CommandSpec registration still lives host-side.)
- **CR.2 ✅ — conflict edit + ops in `lattice-diff`.** Landed:
  `DiffSubsystem::compute_keep_both_edit(active, cursor, theirs)` beside
  `compute_get_edit` — finds the Conflict hunk under the cursor on the active
  (local/ours) side, splices `ours_text ⌢ theirs_text` (ours-then-theirs, base
  omitted — risk #4) into the active range, and returns a `DiffGetOutcome::Edit`
  whose apply target is the active buffer. Conflict-only (a `Change`/`Add`/none
  → `Nothing`); unknown/self `theirs` → `Nothing`. Keep-ours/keep-theirs need NO
  new compute — they reuse `compute_get_edit(active, cursor, Some(slot→bufnr))`
  (already conflict-capable: `allow_conflict = target.is_some()`), as pinned by
  the existing `compute_get_edit_three_way_with_target_resolves_conflict`. Green:
  3 new keep-both unit tests + 228 lattice-diff. (Effect wrapper + chords land in
  CR.3.)
- **CR.3 — `diff-conflict-mode` chords + activation.** `d2o`/`d3o`/`d2p`/`d3p`/`dB`
  via `DiffConflictMode::keymap()` (`keymap_entry!`, name-resolved, MO.x pattern)
  + `action_handlers()` for `dB`; bridge-driven activation (consume
  `sign_map_has_conflicts` in `DiffModeBridge` so the mode toggles on
  conflict-bearing sessions). New mode tests + a DX.1-style pin.
- **CR.4 — conflict nav + ex-command rehoming.** `]c`/`[c` confirmed/filtered to
  land on conflicts in a 3-way session; migrate `:diffget`/`:diffput`/`:diffsplit`/
  `:diff-accept`/`:diff-reject` registration to be diff-owned (or confirm they
  desugar to the mode-owned actions). Green: ex-command resolve pins.
- **CR.5 — four artefacts + cleanup.** Cross-renderer parity (conflict gutter
  already renders — confirm both peers); design `diff-extraction.md` §4 → landed;
  user docs (the `:diffget`/`:diffput` optional-target + the fugitive chords);
  bench (no hot-path change — confirm). Mark CR ✅; update the diff-conflict-mode
  follow-up in `boot-composition.md` + memory.

## Risks / open questions

1. **`Effect::ApplyEdit` + replay/macro** — confirm the applied edit records once
   (not double) through `publish_document_changed`; macros replay the action, not
   the raw edit, so the handler must be deterministic from `ActionContext`.
2. **ActionHandler vs CommandSpec precedence** — confirm the dispatcher prefers
   the `ActionHandlerRegistry` handler over the CommandSpec apply for the same
   `CommandId` (the snippet shape); the emptied `AppEffect` arm is the fallback.
3. **3-way target resolution in a pure handler** — `compute_get_edit` needs the
   slot→bufnr mapping; the handler reads it from the session via the subsystem
   service. Confirm no `&mut Editor` is needed (only the edit + cursor come back).
4. **`dB` keep-both ordering** — ours-then-theirs is the v1 convention (git
   `merge.conflictStyle` diff3 shows base too; v1 omits base). Document it.
5. **No behaviour change for `do`/`dp`** — CR.1 is a pure relocation; the DX.1
   pins + migrated behaviour tests are the arbiter.

## Cross-references

- Design fragment (mode decomposition, §4): `docs/dev/architecture/diff-extraction.md`.
- Parent: BC.6 (`slice-plans/diff-extraction.md`) + MO.x; the `SubsystemBoot`/
  mode-ownership rules in `CLAUDE.md` (the acid test CR closes).
- Pattern precedents: `lattice-snippet` + `lattice-lsp` `action_handlers()`.
