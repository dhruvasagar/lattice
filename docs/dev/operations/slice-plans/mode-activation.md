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

### EF.1 — implement `EventFilter`'s reserved fields ✅ (generic foundation)

- ✅ Added `path_glob: Option<GlobSet>`, `major_modes: Option<Vec<ModeId>>`,
  `predicate: Option<EventPredicate>` (`= Arc<dyn Fn(&Event) -> bool + Send +
  Sync>`) to `lattice_runtime::EventFilter`. AND-combined via a per-`Subscription`
  `ExtraFilter` checked at publish time on the already-`kinds`-bucketed
  candidates (`snapshot_bucket` / `queue_invocations` skip non-matching subs).
  Builder methods `with_path_glob` / `with_major_modes` / `with_predicate`;
  `kind`/`kinds`/`any` unchanged so all existing callers are source-compatible.
- ✅ `major_modes` matching reads `event_major_mode(&Event)`, which returns
  `None` for every current variant — MA.1's `MajorEntered { major }` adds the
  arm. Until then a `major_modes`-constrained filter matches nothing (correct
  "only inside these majors" semantics). `event_path` backs `path_glob`.
- ✅ One shared glob util: `lattice_runtime::glob::compile_glob_set`
  (re-exported at crate root). `lattice-host::lsp_watcher` migrated to it
  (removed its hand-rolled parse-skip-build loop); reused by `path_glob` and
  available to the MA.2 resolver. `lattice-lsp::file_watcher` keeps its
  index-aligned per-watcher matcher (different shape — not a third generic copy).
- **Locking note (audit M1):** the extra filter (incl. `predicate`) is
  evaluated under the bus mutex during the snapshot phase; `tx.send` still runs
  lock-dropped. `EventPredicate` rustdoc documents the non-reentrancy contract.
- Depends on: nothing. Useful beyond modes (any filtered subscription).
- Artifacts: ✅ design (§7.4 contract + EventFilter / EventPredicate rustdoc) ·
  ✅ bench (`benches/event_filter.rs`: publish stays O(subscribers-of-kind),
  `path_glob` adds a per-candidate constant — verified ~linear in subscriber
  count) · ✅ tests (each field + AND-combination + `None`-unconstrained +
  pathless-event rejection + invocation-target gating + glob util good/bad/empty)
  · ✅ graceful (bad glob → `tracing::warn!` + skip, build failure → empty set,
  never panic).

### MA.1 — filterable lifecycle events + `activation_policy()` + registry resolver query ✅ (substrate)

**Decision B (2026-06-12, user-confirmed).** Two stale assumptions in the
original plan were corrected at slice start (verify-before-recommending):
(1) `Mode::subscriptions()` doesn't exist — it was *removed* in MO.4.c
(reactive subscriptions are `on_activate` + Guard, a while-active mechanism;
the activation trigger is a distinct while-inactive concept). (2) Lifecycle
events were typed `ModeEvent`, not `Event` enum, so EF.1's filter didn't reach
them. The chosen model is a **single host resolver + mode-declared allowlist**
(not per-mode subscriptions) — see §7.4 (rewritten).

What landed (substrate; the host resolver wiring is MA.2):

- ✅ The four observable lifecycle events moved to the `Event` enum:
  `MajorEntered` / `MajorExiting { buffer, major: String }` +
  `MinorActivated` / `MinorDeactivated { buffer, minor: String }` (+ `EventKind`
  + `kind()`). `event_major_mode` reads `major`, so EF.1's `major_modes` filter
  lights up end-to-end. The dispatcher now `publish`es all four on the enum bus;
  only the **internal** `ModeActivationFailed` / `OptionConflict` cascade /
  rollback signals stay typed (`ModeEvent`). Rationale: the split "major public,
  minor private" had no principled basis (design.md §5.10.1 lists all four as
  the public catalog) and forced awkward dual-bus test plumbing — one lifecycle
  bus is the cleaner seam. Registry tests migrated to a `subscribe_lifecycle`
  helper on the `Event` bus; failure tests keep the typed `subscribe_mode_events`.
- ✅ `ActivationPolicy` (`Manual` default / `Global` / `Majors([ModeId])`) +
  `ActivationPolicy::admits(major)`; `Mode::activation_policy()` defaulted
  `Manual` + `DynMode` mirror + blanket impl.
- ✅ `ModeRegistry::auto_activatable_minors(major) -> Vec<ModeId>` — the (B)
  resolver core: walks registered minors, filters by kind + `admits`.
- Depends on: EF.1 (the `major_modes` filter).
- Artifacts: ✅ design (§5.1 trait sketch + §7.4 rewritten to the resolver
  model) · ✅ tests (`event_major_mode` positive/negative via EF.1 filter in
  `events.rs`; `ActivationPolicy::admits`; `auto_activatable_minors` kind+policy
  filtering; migrated major-lifecycle registry tests) · graceful (unchanged —
  resolver query is pure; no new failure surface). No bench (registration no
  longer wires per-mode subscriptions; the resolver is one boot subscription +
  an O(minors) walk on a rare event — benched when MA.2 lands the wiring).

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

	EF.1 ✅─┬─> MA.1 ✅ ──> MA.2 ─┐
	        │                     ├─> SN.3
	SN.2 ───┴─────────────────────┘
	SN.1 ✅ (independent — landed first to green the reds)

Note (decision B): MA.2 now also owns the **host minor-activation resolver**
wiring (subscribe once to `Event::MajorEntered` → `auto_activatable_minors` →
`activate_minor`), alongside its original major-selection-on-`DocumentOpened`
scope. MA.1 delivered the substrate that resolver consumes.

## Out of scope (separate triage)

The other pre-existing reds discovered alongside the snippet cluster are
**unrelated** and tracked separately:

- **Arg-slot completion (×3)** — `arg_slot_completion_*`,
  `typing_after_popup_*`; same `describe-command` family as the host K.3.2 red
  (`arm_missing_arg_prompt_canonical_name_works`).
- **Help tutor (×2)** — `tutor_*` (lesson temp-file / content).

These do not block mode activation and are not part of this plan.
