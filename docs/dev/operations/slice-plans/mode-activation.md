# Slice plan — event-driven mode activation

Sequencing for the activation-trigger mechanism. Design contract:
[`../../architecture/mode-architecture.md`](../../architecture/mode-architecture.md)
**§7.4** (activation triggers) — read it first; this file is *when* and *in
what order*, not *what* or *why*.

Status icons: ✅ done · 🚧 in progress · 🗒 planned.

## Context

Triggered by a snippet-mode review (2026-06-11). Two findings drove it:

1. **Snippet's `SnippetCompletionMode` auto-activates by buffer *kind*,
   language-blind** (`auto_activated_minors_for_buffer_kind`). The user wants
   user-configurable, language-aware activation.
2. The naive fix — a `Mode::wants_buffer` predicate polled over every mode —
   is an O(modes)-per-event scan that doesn't scale and risks render-path
   blocks. Rejected (§7.4). The substrate already has the right primitive:
   `EventBus` + `EventFilter`, whose `path_glob` / `major_modes` / `predicate`
   fields are *reserved-but-unimplemented*. Mode activation is the caller that
   finally needs them.

## Slices

### SN.1 — green the snippet test harness ✅ (done; part of "triage reds")

The failing `snippet_*` tests (`input::tests`, `keymap_insert::tests`) were
**test-harness staleness, not a production regression**. Root cause: the test
helpers minted `ActionIds` from one `CommandRegistry` but built the snippet
mode-keymap layer from a *second*, throwaway registry — and `CommandId`s are
only stable *within* one registry instance, so the layer resolved
`action:snippet-next-placeholder` to a different id than the assertions
expected. `Tab` therefore fell through to base insert.

- Fix: keep the registry that minted the shared `ActionIds` alive
  (`shared_init()` now returns `(CommandRegistry, …)`), and translate the
  snippet layer against that *same* registry via
  `translate_mode_keymaps(h, &mr, shared_registry())`. Production was always
  fine — `sync_keymap_overlays` activates via `activate_minor` against the live
  registry.
- **Independent of the rest of this plan** — unblocked the red immediately. All
  27 `lattice-ui-tui` snippet tests pass; the only remaining `lattice-ui-tui`
  reds are the documented out-of-scope clusters below (tutor ×2, arg-slot ×3).
- Artifacts: tests (the snippet cluster, re-greened); no bench / design /
  error-handling surface (harness only).

### EF.1 — implement `EventFilter`'s reserved fields 🗒 (generic foundation)

- Add `path_glob: Option<GlobSet>`, `major_modes: Option<Vec<ModeId>>`,
  `predicate: Option<Arc<dyn Fn(&Event) -> bool + Send + Sync>>` to
  `lattice_runtime::EventFilter`; AND-combine, checked on the already-`kinds`-bucketed
  candidates (publish scan must not widen — bench gate).
- Extract / reuse **one** glob util (lattice-lsp's file-watcher already
  globs; consolidate rather than add a third). Shared by `path_glob` and the
  MA.2 resolver.
- Depends on: nothing. Useful beyond modes (any filtered subscription).
- Artifacts: design (§7.4 + EventFilter rustdoc) · bench (publish dispatch
  stays O(subscribers-of-kind), filter fields don't widen it) · tests
  (each field + AND-combination + `None`-is-unconstrained) · graceful (bad
  glob → `tracing::warn!` + skip the subscription, never panic).

### MA.1 — `Mode::subscriptions()` + registration wiring + filterable lifecycle events 🗒

- Land `Mode::subscriptions(&self) -> Vec<ModeSubscription>` as a defaulted
  trait method (the doc trait surface already declares it; make it real).
  `ModeSubscription { filter: EventFilter, action: SubscriptionAction }`.
- `ModeRegistry::register` wires each subscription to the bus **once** at
  registration (needs bus access — pass it in, or a `wire_subscriptions(bus)`
  follow-up call).
- **Decision (§7.4 open item):** make the `MajorEntered` / `MajorExiting`
  lifecycle events filterable by major-mode id. Lean: land them as `Event`
  variants (+ `EventKind`) so `EventFilter.major_modes` applies directly,
  rather than extending the typed-event path with filters.
- Depends on: EF.1 (the `major_modes` filter).
- Artifacts: design (§5.1 + §7.4) · bench (registration wiring is O(subs),
  one-time) · tests (subscription fires only for filtered kinds + major-mode
  match; deactivation on `MajorExiting`) · graceful (subscription handler
  error → log + drop, never poisons the bus).

### MA.2 — major-mode resolver on `DocumentOpened` 🗒

- Ordered resolver subscribed to `Event::DocumentOpened`: run registered
  major-mode matchers by priority, first wins, activate it, emit
  `MajorEntered`.
- Built-in language modes reuse `lattice_syntax::Lang::detect_from_path`;
  plugin majors use `path_glob` (the EF.1 util).
- **Open item:** how much of a formal "major mode" exists today vs. just
  `Lang` detection for syntax — confirm at slice start (this may be partly
  greenfield; today's syntax attach is language-detection without a
  registered major `Mode`). If major modes aren't yet first-class, MA.2 may
  reduce to "publish `MajorEntered` from the existing Lang-detection point"
  and the full resolver lands with the major-mode migration (§10).
- Depends on: MA.1 (`MajorEntered` event).
- Artifacts: design (§3.1 + §7.4) · bench (resolver is O(major-modes) on open
  only) · tests (priority order, first-match, no-match → Plain) · graceful
  (no matcher → Plain/no major mode, never panic).

### SN.2 — close the snippet ownership half-migration 🗒

The snippet *keymap* is already mode-owned (`SnippetActiveMode::keymap()` in
`lattice-snippet`). The **action registration** (`lattice-host/actions.rs`:
`action:snippet-*`) and **handlers** (`Editor::do_snippet_*`) are host-owned —
the half-migration the standing rule forbids (acid test: a provider crate
should need zero `Editor::` methods + zero host registrations).

- Move snippet action registration + handler bodies `lattice-host →
  lattice-snippet` via the `ActionHandlerRegistry` substrate (§5.3 — the path
  LSP/diff already use). Host keeps only generic primitives.
- Depends on: nothing hard; can run parallel to EF/MA. Best landed before SN.3
  so the activation work touches a mode-owned snippet surface.
- Artifacts: design (§5.3) · bench (n/a) · tests (snippet Tab/leave dispatch
  through the mode's registered handlers) · graceful (unchanged).

### SN.3 — snippet language-aware activation 🗒 (the payoff)

- `SnippetCompletionMode::subscriptions()` returns a `MajorEntered`
  subscription filtered by its language allowlist; drop the language-blind
  `auto_activated_minors_for_buffer_kind` path for snippet.
- Config: `snippet.activation = global | supported-languages | off` +
  `snippet.languages = [...]`. Host folds the option into the subscription's
  `major_modes` filter; re-fold on the `OptionChanged` subscription (`:set`
  live). **Default allowlist may be empty** → user opts in (§7.4).
- Depends on: EF.1, MA.1, MA.2, SN.2.
- Artifacts: design (§7.4 worked example) · bench (n/a — activation is
  lifecycle-time) · tests (activates only for allowlisted languages; `off` →
  never; `global` → everywhere; `:set` live re-fold; empty default → no
  activation) · graceful (unknown language in allowlist → log + skip).

## Dependency graph

	EF.1 ─┬─> MA.1 ──> MA.2 ─┐
	      │                  ├─> SN.3
	SN.2 ─┴──────────────────┘
	SN.1 ✅ (independent — landed first to green the reds)

## Out of scope (separate triage)

The other pre-existing reds discovered alongside the snippet cluster are
**unrelated** and tracked separately:

- **Arg-slot completion (×3)** — `arg_slot_completion_*`,
  `typing_after_popup_*`; same `describe-command` family as the host K.3.2 red
  (`arm_missing_arg_prompt_canonical_name_works`).
- **Help tutor (×2)** — `tutor_*` (lesson temp-file / content).

These do not block mode activation and are not part of this plan.
