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

### MA.2 — host minor-activation resolver ✅

**Open item resolved at slice start.** Documents already activate a major:
`Editor::activate_major_for_buffer_kind(id, Document)` detects the language
(`Lang::detect_from_path`), resolves a major via `resolve_major_mode`
(language major, else `text-mode` fallback), and `activate_major` publishes
`Event::MajorEntered`. So the original "ordered major resolver on
`DocumentOpened`" was unnecessary — documents (and every other kind) already
emit `MajorEntered`. What MA.2 actually needed was the *minor* side: the
single host resolver that consumes `MajorEntered` (decision B).

What landed:

- ✅ `pending_major_entered_rx`: one channel subscribed to
  `EventKind::MajorEntered` at boot (editor_boot.rs), mirroring the
  `pending_mode_lifecycle_rx` rollback channel.
- ✅ `Editor::drain_minor_activation` (per-tick, beside
  `drain_mode_lifecycle_events`): drains `MajorEntered`, looks up
  `buffers.kind_of(id)`, queries `auto_activatable_minors(major, kind)`,
  calls `activate_mode_by_id` for each. Idempotent (already-active minors
  no-op); unknown buffers skipped, never panic.
- ✅ Kind-aware policy (refines MA.1's API): `ActivationPolicy::admits(major,
  kind)` + `auto_activatable_minors(major, kind)` — **`Global` minors only in
  real document buffers** (`BufferKind::Document`), per the user (2026-06-12);
  `Majors([..])` stays kind-independent (explicit opt-in works in synthetic
  buffers too).
- Depends on: MA.1 (the `MajorEntered` event + `auto_activatable_minors`).
- Artifacts: ✅ design (§7.4 resolver + Global-document gate) · ✅ bench
  (`benches/activation_resolver.rs`: `auto_activatable_minors` O(minors) on a
  rare event; per-tick is O(1) `try_recv` when idle) · ✅ tests (lattice-mode:
  `admits` kind-gate, `auto_activatable_minors` Global-in-doc / gated-in-Help;
  lattice-host: `major_entered_resolver_activates_global_minor_on_document`
  end-to-end wiring + non-matching-allowlist negative) · ✅ graceful
  (unknown buffer → skip).
- **Deferred (follow-up, not blocking):** re-evaluation / deactivation on
  *major switch* (a buffer changing language) — rare; the existing kind-based
  `auto_activated_minors_for_buffer_kind` path still coexists (idempotent) and
  SN.3 migrates snippet off it onto a declared policy.

### SN.2 — close the snippet ownership half-migration ✅ (SN.2a + SN.2b landed)

The snippet *keymap* is already mode-owned (`SnippetActiveMode::keymap()` in
`lattice-snippet`). The **handlers** (`Editor::do_snippet_*`) are host-owned —
the half-migration the standing rule forbids. Move the handler bodies
`lattice-host → lattice-snippet` via the `ActionHandlerRegistry` substrate
(§5.3 — the path the project-search provider already uses;
`ActionContext → Effect`, no `&mut Editor`).

**Boundary correction (2026-06-12, confirmed with the user).** Investigation
found the three snippet actions split across *two* modes by trigger context:

- **`<Tab>` / `<S-Tab>` (next/prev placeholder)** fire *while a snippet is
  active* → owned by **`SnippetActiveMode`**. This is SN.2.
- **`<C-x><C-s>` (expand)** is a base insert binding that fires *when no
  snippet is active yet* — a *completion*-mode operation. It additionally needs
  `snippet_registry` + buffer text + the `expand_snippet` splice. It belongs to
  **`SnippetCompletionMode`** and migrates in **SN.3** (where that mode is built
  out + activated language-awarely). Cramming it into `SnippetActiveMode` would
  be a mis-migration (handler only registered while a snippet runs).

Sub-sliced (the session is host state ~15 readers touch, so relocation comes
first):

- **SN.2a ✅ (committed `7a897a79`)** — relocate the live session off the
  `Editor` into a `lattice_snippet::SnippetSession` service
  (`Arc<Mutex<Option<ActiveSnippet>>>`, registered in `ServiceRegistry`). The
  `ActionHandlerRegistry` seam gives a handler only `&ActionContext` (no
  `&mut Editor`), so the session both the host (expand) and the mode (nav)
  mutate must live in a shared service. `Editor.active_snippet` → service; all
  ~15 host readers + ui-tui snippet-test assertions migrated. Host nav handlers
  read the service (still host-side this step). 27 ui-tui snippet tests green.
- **SN.2b ✅ (2026-06-12)** — migrated the two nav handlers into
  `SnippetActiveMode`; removed the host `Action` surface. As landed:
  1. `lattice-snippet` gained a `lattice-grammar` dep (name/construct `Effect`).
     `lattice-runtime` proved unnecessary as a *direct* dep — `Document` /
     `DocumentSnapshot` reach the handler transitively via
     `BufferStore::handle_for`'s return type; it's a `[dev-dependencies]` entry
     only (the handler-level test builds a `ModeContext`, which needs `EventBus`).
  2. The two handlers are `ActionContext → Effect` closures in
     `SnippetActiveMode::on_activate`: advance the shared `SnippetSession`
     (`<Tab>` clears it on walk-off-`$0`), resolve the new cursor via
     `BufferStoreHandle → snapshot → byte_to_position`, return
     `Effect::SelectionChange`. The cursor math is a free fn
     (`snippet_group_cursor_effect(&Buffer, &TabstopGroup)`) so it's unit-testable
     without a store.
  3. `SnippetActiveMode::Guard` `()` → `SnippetActiveModeGuard` holding the two
     `ActionHandlerRegistration` tokens; `on_activate` resolves
     `action:snippet-next/prev-placeholder` → `CommandId` via
     `CommandRegistryHandle` and registers on `ActionHandlerRegistryHandle` (the
     search-provider template).
  4. Host cleanup: deleted `Editor::do_snippet_next/prev_placeholder` +
     `move_cursor_to_snippet_group`, the `Action::SnippetNext/PrevPlaceholder`
     variants + their dispatch arm, and removed them from the ui-tui App
     dispatch band. The `AppEffect::SnippetNext/PrevPlaceholder` arms became
     no-ops (kept because `register_simple` still registers the action specs —
     same shape as the post-M.10.x `SearchJumpToSource` / `SearchRefresh` arms).
     The chord now flows through the generic `ActionHandlerRegistry` lookup
     (`dispatch.rs` `run_document_invocation`). `snippet_expand` kept host-side.
  - Tests: the two ui-tui nav tests relocated to `lattice_snippet::modes` as a
    handler-level dispatch test (6 tests: cursor-effect math ×2, registration +
    drop-unregister, `<Tab>` walk-through-+-drop-on-`$0`, `<S-Tab>` walk-back,
    no-session no-op). 70 `lattice-snippet` tests green; host + ui-tui +
    **gpui** build clean. GPUI needed no change — the nav now flows through the
    renderer-agnostic `ActionHandlerRegistry`, so the migration *removed*
    renderer surface rather than adding a per-renderer arm.
- Depends on: nothing hard; SN.2a landed independently. Closed before SN.3 so
  the activation work touches a mode-owned snippet surface.
- Artifacts: design (§5.3) · bench (n/a — nav is per-`<Tab>`, off any hot path)
  · tests (✅ handler-level dispatch test in `lattice-snippet`) · graceful
  (handlers tolerate missing services / missing buffer → no-op).

### SN.3 — snippet language-aware activation 🚧 (the payoff)

**Three-mode decomposition (confirmed with the user 2026-06-14).** The old
plan put `<C-x><C-s>` on `SnippetCompletionMode` and drove activation off a
`subscriptions()` method that was never built. Corrected:

- **`snippet-mode`** (new) — the "snippets enabled here" gate. Owns `<C-x><C-s>`
  direct-expand. Carries the config-driven `ActivationPolicy`. `implies`
  `snippet-completion-mode` so the source rides the same language gate.
- **`snippet-completion-mode`** (exists) — provides the `gen:snippet`
  completion *source* only. No activation policy of its own; rides
  `snippet-mode`'s `implies`.
- **`active-snippet-mode`** (exists, SN.2b) — in-flight placeholder nav; lit by
  the session-backed reconciler (SN-activation slice) when a snippet expands
  (via `<C-x><C-s>` or `<CR>`-accept of a `gen:snippet` candidate).

Substrate note: the landed mechanism is `Mode::activation_policy() ->
ActivationPolicy` (`Manual`/`Global`/`Majors([mode_ids])`) resolved on
`MajorEntered` by `auto_activatable_minors` (MA.1/MA.2) — NOT `subscriptions()`.

**Default `snippet.activation = global`** (not the old empty-allowlist
opt-in): the snippet *source* already self-filters by language
(`matching_prefix(lang, …)` + `*`), so `Global` means "each buffer sees its own
language's snippets", not "all snippets everywhere". With built-in packs now
shipped, `global` keeps them working out of the box; `supported-languages` /
`off` stay as opt-in restrictions. `supported-languages` resolves to
`Majors([<lang>-mode …])`, which only matches *registered* major modes (today:
rust / python / javascript / markdown) — `global` has no such dependency, which
is why it's the right default for "support all major languages" now.

Sub-slices:

- **SN.3a ✅ (2026-06-14)** — `SnippetMode` (minor) with a static
  `ActivationPolicy::Global` + `implies snippet-completion-mode`; registered in
  `register_snippet_modes`. Dropped `SnippetCompletionMode` from the
  language-blind `auto_activated_minors_for_buffer_kind` list — the source now
  rides the gate via the resolver. Behavior-preserving (Global = the old
  every-Document activation). `<C-x><C-s>` unchanged (still host-bound).
  Tests: lattice-snippet (id/kind, default-Global, implies, registration) +
  host integration (`major_entered_resolver_activates_snippet_mode_and_implied_source`:
  Document → Global activates + implied source rides). 76 snippet ✅; host
  708 ✅ / 1 pre-existing red; ui-tui 5 pre-existing reds; gpui builds.
- **SN.3b ✅ (2026-06-14)** — `SnippetMode::activation_policy()` is now
  config-driven. The mode holds a shared `SnippetActivationPolicyHandle`
  (`Arc<ArcSwap<ActivationPolicy>>`, default `Global`) it reads on every
  resolver call; `register_snippet_modes` creates the cell and returns it.
  Boot folds `snippet.activation` + `snippet.languages` into it
  (`editor_boot.rs`, before the `Editor {…}` literal); `apply_option_cascade`
  gains a `"snippet.activation" | "snippet.languages"` arm that re-folds
  (`:set` live). New buffers pick up the live policy on their next
  `MajorEntered` (the resolver reads `activation_policy()` live —
  `registry.rs:184`).
  - **Mode-ownership:** the two options + the `SnippetActivationMode` enum +
    `fold_activation_policy` live in `lattice-snippet` (`activation.rs`); the
    `Snippet` option-group lives in `lattice-config::group` (same split as
    `search.context_size`). The host arm is a thin call into the mode-owned
    fold (same shape as the `messages.filter` arm) — no policy meaning leaks
    host-side.
  - **Decision — `snippet.languages` is a comma-separated `String`, not a
    list.** The typed-option loader rejects TOML arrays for scalar options
    (`loader::apply_scalar` warns + skips) and no list `OptionType` exists;
    adding one is a separate lift out of SN.3b scope. TOML:
    `snippet.languages = "rust,python"`; `:set snippet.languages=rust,python`.
    `snippet.activation` IS a typed enum (closed 3-value set), so
    `:set snippet.activation=<Tab>` self-documents like `foldmethod`.
  - **Decision — not retroactive.** `:set` changes apply on the next
    `MajorEntered`, per spec; already-open buffers are not re-resolved. Known
    follow-up: a policy change could iterate open document buffers and
    activate/deactivate `snippet-mode`, but that touches the mode-guard
    lifecycle and is deferred.
  - Tests: `lattice-snippet` `activation::tests` (parse/format/enumerate +
    fold for global/off/supported-languages incl. empty-list graceful case) +
    `modes::tests` (live-cell read; `register_snippet_modes` returns a Global
    default) + host `dispatch::tests::set_snippet_activation_refolds_the_policy_cell`
    (`do_set` → cascade → folded policy across all three modes). 86 snippet ✅
    (76 → +8 activation +2 modes); host snippet tests ✅; config
    group-uniqueness ✅; gpui builds (no renderer surface touched).
- **SN.3c 🗒 — close the expand/leave ownership half-migration.** SN.2b moved
  the *nav* handlers (`<Tab>`/`<S-Tab>`) into `active-snippet-mode`, but the
  *expand* and *leave* handlers are still host-side
  (`Editor::do_snippet_expand_at_cursor` + `Action::SnippetExpand` +
  `AppEffect::SnippetExpand`; `Action::SnippetLeave`). Per
  `feedback_mode_owns_its_surface` that is a half-migration; SN.3c removes all
  of it.

  **Design decided 2026-06-14 (see `feedback_effect_vocabulary_is_host_boundary`).**
  The review's first instinct — register the expand handler in
  `SnippetMode::on_activate` — is **wrong**: `snippet-mode` is active on *every*
  document buffer at once, but `ActionHandlerRegistry` is keyed by `CommandId`
  alone (global, non-refcounted, `unregister` removes the key). Per-buffer
  `on_activate` registration of a multi-buffer mode would let one buffer
  closing evict the handler for all the others. The expand handler is
  **buffer-agnostic** (reads the active buffer/cursor from `ActionContext` at
  call time, closes over no per-buffer state), so it is a single **global**
  handler — registration *site* must match handler *scope*. Nav/leave are
  genuinely per-buffer (tied to a live session) and stay in `on_activate`.

  Split into three sub-slices:

  - **SN.3c.0 🗒 — declarative `Mode::action_handlers()` (mode-agnostic
    substrate).** Add a declarative trait method (default empty), forwarded
    through `DynMode`, joining `keymap()` / `completion_sources()`:

    ```
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> { Vec::new() }
    // ActionHandlerContribution { action_name: &'static str, handler: ActionHandler }
    ```

    The boot loop already walks the mode registry to apply `keymap()`
    (`translate_mode_keymaps`); a sibling walk applies `action_handlers()`:
    resolve each `action_name` → `CommandId` via the command registry,
    `ActionHandlerRegistry::register(id, handler)`, collect the tokens into an
    app-lifetime `Vec<ActionHandlerRegistration>` on `Editor` (registered once,
    dropped at shutdown). This is the reusable, mode-agnostic path for
    **global** action handlers — a standalone `register_snippet_global_handlers`
    fn was rejected because per-mode host wiring fails the mode-ownership acid
    test (a new provider crate must require ZERO host additions). Per-buffer
    handlers keep using `on_activate` + Guard. Artifacts: tests (a contributed
    global handler resolves for any buffer; survives an unrelated mode's
    activate/deactivate) · no bench/doc beyond this entry.

  - **SN.3c.1 ✅ (2026-06-14) — snippet uses it (expand migration).**
    (design refined during build; supersedes the earlier `{ snippet_name,
    replace_range }` shape and the `:snippet-expand`/`RunModeAction` plan).
    As landed: `<C-x><C-s>` moved off Builtin onto `SnippetMode::keymap()`
    (`KeymapLayer::MinorMode("snippet-mode")`, Insert); `SnippetMode::action_handlers()`
    contributes one *global* handler bound to `action:snippet-expand` that does the
    word-prefix scan (`snippet_trigger_range`, pure + unit-tested) and emits
    `Effect::ExpandSnippet { replace_range }`. The host arm
    (`Editor::expand_snippet_from_range`) owns resolution + expansion (language +
    registry + variables + splice via `expand_snippet`). Removed:
    `do_snippet_expand_at_cursor`, `Action::SnippetExpand`, `AppEffect::SnippetExpand`,
    `Effect::SnippetExpand`, the `:snippet-expand` ex-command + `ExBuiltins.snippet_expand`,
    the `<C-x><C-s>` Builtin binding, and the `App::do_snippet_expand_at_cursor` delegate.
    `action:snippet-expand` CommandSpec kept (chord binds it + handler keys on it); its
    `apply` is a dead `Effect::None`. TUI+GPUI parity: both peers' apply arms call
    `expand_snippet_from_range`; classifier matches updated. Tests: lattice-snippet 92 ✅
    (+6 SN.3c.1: keymap/handler/trigger-range ×4 + no-op-when-buffer-unavailable);
    host green bar (1 pre-existing arg-slot red unrelated); ui-tui 1464 ✅ / 5 pre-existing
    reds (tutor ×2, arg-slot ×3); gpui builds. Docs synced (`docs/user/completion.md`,
    `docs/dev/architecture/insert-completion.md`). **Original FINALIZED design follows
    (preserved for rationale):**
    1. **Binding.** `<C-x><C-s>` moves off Builtin (`keymap_insert.rs`) onto
       `SnippetMode::keymap()` at `KeymapLayer::MinorMode("snippet-mode")`
       (Insert mode); K.1.c gates it to `snippet-mode`-active buffers.
    2. **Handler.** `SnippetMode::action_handlers()` returns a handler bound to
       `action:snippet-expand` that does ONLY the word-prefix scan: read the
       line at `ctx.cursor` via `BufferStoreHandle` (from
       `ActionContext.services`), walk back over word chars to find the trigger
       token's start, and emit `Effect::ExpandSnippet { replace_range }`
       (token-start..cursor). Returns `None` (no effect) when there's no word
       prefix. **No registry / language in the mode** — see point 3 for why.
    3. **`Effect::ExpandSnippet { replace_range: Range }`** (final shape — just
       the range). The **host** arm owns resolution + expansion: prefix = buffer
       text in `replace_range`; `language = active_language_id()` (host-owned
       path→language detection — duplicating it in the mode is worse, so the
       mode does NOT resolve the snippet); lookup `(language, prefix)` then
       `"*"` in `self.snippet_registry`; render with host-owned `VariableContext`;
       reuse existing `Editor::expand_snippet(&body, replace_range.start)` for
       splice + session + cursor (the range end IS the cursor, so the existing
       `anchor..cursor` replace is correct). Graceful: no match → quiet info
       echo. This is a deliberate **first-party** effect (typed `Effect` stays a
       host-owned vocabulary — `feedback_effect_vocabulary_is_host_boundary`);
       the mode owns the *trigger* (chord + token scan), the host owns the
       first-party *expansion mechanics* (language + registry + variables +
       buffer mutation).
    4. **Remove** `Editor::do_snippet_expand_at_cursor` (its body splits: scan →
       mode handler; lookup+expand → the `Effect::ExpandSnippet` host arm),
       `Action::SnippetExpand`, `AppEffect::SnippetExpand`, `Effect::SnippetExpand`,
       the `:snippet-expand` ex-command (UX-useless per Dhruva 2026-06-14), and
       the `<C-x><C-s>` Builtin binding. `action:snippet-expand` CommandSpec
       stays (the chord binds it + the handler keys on it); its apply becomes a
       dead `Effect::None` (the ActionHandlerRegistry handler always intercepts).
       The `<CR>`-accept path keeps calling `expand_snippet` directly (host-side,
       not chord-owned).
    - **`RunModeAction` NOT built.** A generic "ex-command/programmatic → mode
      action" effect would be dead code with no caller once `:snippet-expand` is
      removed, violating the no-speculative-abstraction rule. The reusable
      plugin-facing API already ships: `Mode::action_handlers()` (SN.3c.0) +
      `Mode::keymap()` + the generic ActionHandlerRegistry dispatch let any mode
      own + trigger an action via a chord with zero host knowledge;
      `Effect::ExpandSnippet` is the reusable expansion primitive a handler can
      emit. Programmatic ex-command-by-name invocation of a mode action is a
      broader ex-command/action unification to build when a concrete caller
      lands (ex-commands-go-mode-owned, or the WASM plugin host). Tracked in
      `feedback_effect_vocabulary_is_host_boundary`.
    - TUI+GPUI parity (`feedback_tui_gpui_parity`): `Effect::SnippetExpand` →
      `Effect::ExpandSnippet { replace_range }` in both peers' apply arms
      (the peer arm calls `e.expand_snippet_from_range(replace_range)` — the
      host helper wrapping resolve+expand) and in the exhaustive
      `effect_*` classifier matches (host `dispatch.rs` + TUI `app/dispatch.rs`
      + GPUI).

  - **SN.3c.2 ✅ (2026-06-14) — leave migration.** Landed in two sub-slices
    after the build surfaced a pre-existing insert-mode gating gap (the leave
    `<Esc>` exposed it):

    - **SN.3c.2a ✅ (`ad33b2f9`) — gate insert-mode dispatch + relocate leave.**
      `dispatch_insert` never had the K.1.c per-buffer minor-mode gate Normal
      mode has (D.5) — it called `handle.lookup`, which folds in EVERY
      registered minor layer unconditionally, so `active-snippet-mode`'s
      `<Tab>`/`<S-Tab>`/`<Esc>` shadowed base Insert in every buffer (live bug:
      `<Tab>` in plain insert inserted nothing). Fix: `dispatch_insert` threads
      `active_minor_modes` → `lookup_with_context`. The leave body moved into
      `active-snippet-mode::on_activate` (clears the session); removed
      `Action::SnippetLeave` / `AppEffect::SnippetLeave` + the dispatch arm.
      Interim: the handler hardcoded `Effect::EnterMode(Normal)` to exit insert.
    - **SN.3c.2b ✅ (`3df8521b`) — `fall_through` binding primitive + snippet
      `<Esc>` uses it.** `:map`-style augment-and-continue: a binding marked
      `fall_through: true` runs its action, then the dispatcher re-resolves the
      same chord with the owning mode peeled out of the active set and runs the
      native binding too (bounded — each hop peels a layer, terminating at
      Builtin; cannot loop like vim `:map`). Threads `KeymapEntry.fall_through`
      (+ macro form) → `KeymapBinding` → `BoundCommand` →
      `LookupResult`/`KeymapResolution`. Continuation executes via a new
      `Action::Chain(Vec<Action>)` (`dispatch_insert` returns
      `Chain([mode_action, native_action])`; applied in order by host
      `handle_action` + the TUI `App::apply`; GPUI routes through
      `Editor::dispatch`). `active-snippet-mode`'s `<Esc>` is now
      `fall_through: true`; the leave handler returns `Effect::None` (session
      clear only) — exiting insert is the native `<Esc>` via the continuation,
      so the mode no longer hardcodes what `<Esc>` means (a user rebind
      composes). `:describe-key` surfaces it: `(fires now → falls through ↓)`
      on the winner, `↳ then: …` continuation lines, and
      `[active · fall-through]` / `[inactive · fall-through]` layer tags.
      The `fall_through` API is mode-agnostic + plugin-facing (any mode/plugin
      can augment a native chord without owning its meaning).

  Acid test (standing rule): after SN.3c.2, no `Editor::do_snippet_*`
  expand/leave method and no `Action::SnippetExpand` / `Action::SnippetLeave`
  remain in `lattice-host`; the only host snippet surface is the generic
  `Effect::ExpandSnippet` arm (splice + variable render + session install).
  - Depends on: EF.1, MA.1, MA.2, SN.2, SN.3a, SN.3b, SN-activation reconciler.
  - Artifacts: design (this section) · bench (n/a — expand is keystroke-rare,
    off the render path) · tests (per sub-slice: substrate walk; mode-owned
    expand emits `Effect::ExpandSnippet`; host arm splices + installs session;
    leave clears + deactivates; `<C-x><C-s>` no-ops in a non-`snippet-mode`
    buffer) · graceful (no prefix / no matching snippet → quiet info echo).
  - **WASM-stage revisit (Dhruva, 2026-06-14):** plugins cannot require ANY
    host wiring or per-feature handling, so the first-party `Effect::ExpandSnippet`
    exception must be re-examined at the plugin phase, when the Effect boundary
    becomes a capability-gated extension contract (§9 / deferred M.10), not a
    closed enum. Tracked in `feedback_effect_vocabulary_is_host_boundary`.

### SN.3 follow-ups (from the 2026-06-14 review)

Filed as their own slices rather than folded into SN.3c — each is independent
of the expand/leave migration and carries its own risk surface.

- **SN.3d 🗒 — build vim Select mode (core), snippets consume it (finding B;
  UX). Design LOCKED 2026-06-14 (user): a dedicated vim grammar mode, NOT a
  snippet feature.** The bug: `snippet_group_cursor_effect`
  (`lattice-snippet/src/modes.rs`) emits a zero-width
  `Selection::cursor(range.start)`, so the cursor lands at the *start* of a
  `${1:default}` and typing inserts before the default. The fix is **not** a
  snippet patch — `do_insert_text` does not overtype a selection, and bolting
  that on would couple snippets to a workaround. Instead build **vim Select
  mode** as a first-class `ModalState::Select(VisualKind)` /
  `BindingMode::Select`: same selection extent as Visual, but a printable key
  replaces the selection and drops into Insert. It's a reusable primitive
  (rename, template fields, "select word & replace"), decoupled from snippets
  entirely. Snippets become one *consumer*: a non-empty placeholder emits
  `Effect::Many([SelectionChange(span), EnterMode(Select(Charwise))])`; empty
  placeholders (`$1`) keep the bare Insert cursor. Design fragment:
  [`../../architecture/select-mode.md`](../../architecture/select-mode.md)
  (contract, state machine, keymap/dispatch, render, rejected alternatives,
  paramount-goal alignment). **Doc debt fixed by this slice:**
  `docs/user/completion.md` + the `active-snippet-mode` doc-comment already
  claim "keep typing inside a placeholder to overtype the default" — true once
  Select mode + the snippet consumer land.

  **Review tightening (2026-06-14, folded into the design fragment).** Before
  building, the design now pins five things the original sketch left open:
  (1) adding `ModalState::Select` is **not** "just another arm" — ~677
  `ModalState::` use-sites, several exhaustive (e.g. `cursor_shape.rs`); the
  audit + a Visual≡Select parity test are explicit work (d.0). (2) The
  Visual/Select motion-table is **duplicated** (`register_select_bindings`
  parallels `register_visual_bindings`) **guarded by a parity test** — LOCKED;
  the test is the drift guard, no speculative shared-list abstraction. (3) The
  overtype is **one replace-range edit** (`Effect::Edits([replace(span → c)])`),
  not `Effect::Many([delete, insert])` — single undo via `apply_edit_batch`.
  (4) `selection_is_active()` is **not** a blanket `is_visual()` rename — Select
  can't dispatch operators (printables overtype), so convert callers per-site.
  (5) `cursor_shape.rs` `Select(_) => Block`; macros must record the overtype
  as a `CommandInvocation`.

  **Sub-slices (sketch, sequenced at build):**
  - **(d.0) ✅ (`cd5f9cb9`)** `ModalState::Select(VisualKind)` + `is_select()`
    (grammar) + `BindingMode::Select` (keymap, label/all/count 22→23). The
    exhaustive-match audit used the compiler as the tool: only **two** sites were
    truly exhaustive `ModalState` matches — `cursor_shape.rs` (`Select(_) => Block`
    + a Visual≡Select parity test) and the TUI status tag (`Select(_) => "SELECT"`,
    kind-agnostic like `"VISUAL"`). The ~677 other `ModalState::` use-sites are
    constructions / `matches!` / `_`-arms; the `Visual(kind)`-sibling sites
    (selection render, status) that *should* treat `Select` identically but won't
    fail to compile are d.2 work (render parity). `selection_is_active()` was
    **deferred** per no-speculative-abstraction — verified zero production
    `is_visual()` callers (production branches via `matches!` directly), and
    Select can't dispatch operators, so no consumer exists yet.
  - **(d.1) ✅** `translate_select` dispatch (`keymap_select.rs`, new module) —
    genuinely new logic: a bound motion/exit wins, an **unbound printable falls
    through to overtype** (`dispatch_visual` has no such fallthrough; the
    reference is `dispatch_insert`'s `literal_text_fallback`). Control chords:
    `<Esc>` → `ExitSelect`, `<C-g>` → `ToggleVisualSelect` (one handler flips
    Visual↔Select either way, selection preserved), `<C-o>` → swallowed
    (post-MVP per §3). New `Action`s: `SelectOvertype(char)` / `ExitSelect` /
    `ToggleVisualSelect`. Host handlers: `do_select_overtype` lands the overtype
    as **one `Edit::replace(span → c)`** (single undo, verified by test) then
    `enter_mode(Insert)`; `do_exit_select` collapses + stashes `last_visual` for
    `gv`; `do_toggle_visual_select` is a pure modal pivot. Selection geometry
    extracted to one shared `visual::selection_extent` helper (Visual + Select,
    no drift — §2). Routing arm `Select(kind) => translate_select` added in
    `input.rs` (was the `_ => Action::None` catch-all). Graceful: overtype on a
    degenerate selection falls back to insert-at-cursor, never panics.
    Tests: 6 dispatch (`keymap_select`) + 4 host-handler (overtype single-undo,
    empty-buffer graceful, exit→Normal+gv, toggle both ways). No new bench —
    the overtype is one edit, same cost class as Visual `c`; the keystroke→glyph
    bench covers the dispatch path holistically.
  - **(d.2a) ✅** entry chords + bindings + parity. `gh`/`gH`/`g<C-h>` bound in
    Normal (`[g,h]`/`[g,H]`/`[g,<C-h>]`) → `AppEffect::EnterSelect(kind)` →
    `Action::EnterSelect` → `do_enter_select` (anchors a zero-width selection,
    mirrors `do_enter_visual`). CTRL is preserved by `normalize_for_normal_lookup`
    so `g<C-h>` ≠ `gh`. Visual `<C-g>` → `ToggleVisualSelect` as a hardcoded
    intercept in `dispatch_visual` (before the CTRL short-circuit), symmetric with
    `translate_select`'s `<C-g>` — no new Effect needed; `<C-g>` confirmed free in
    Visual. `register_select_bindings` (motions + `o` + text-objects from the
    SHARED `motion_rows`/`text_object_rows`; NO operators, NO exits) wired in
    `editor_boot`. **Parity test** (`keymap_select::tests`): every Visual motion
    binds identically in Select; `o` + `iw` parity; **operators bind in Visual but
    stay UNBOUND in Select** (they overtype). 8 new tests (4 parity + entry handler
    + the d.1 dispatch set).
  - **(d.2b) ✅** status-line + TUI/GPUI selection-render + cursor-shape parity.
    The render-publish surface (`Editor::visual_selection_range` /
    `visual_block_extents` in `visual.rs`) was the single seam — both gates now
    fire for `ModalState::Select(_)` (blockwise gate for `Select(Blockwise)`), so
    Select selections flow to BOTH renderer peers through the existing published
    `ActiveDocumentRenderState.{visual_range,visual_block_extents}` with ZERO
    paint-arm change (TUI `render.rs` and GPUI `window.rs`/`editor_element.rs`
    already read those fields). Status label: TUI `app/mode.rs::modal_label`
    already returned kind-agnostic `"SELECT"` (d.0); added the matching
    `ModalState::Select(_) => "SELECT"` arm to GPUI `window.rs`'s status-line match
    (it was the only non-exhaustive peer). Cursor shape was already done in d.0
    (`cursor_shape.rs`: Select shares Visual's Block cursor, with a parity test).
    No `-- SELECT LINE/BLOCK --` long-form variants — Visual itself is kind-agnostic
    in the short tag, and Select mirrors Visual (standing rule). 2 new render-publish
    parity tests in `dispatch::tests` (`select_publishes_same_selection_range_as_visual`,
    `select_blockwise_publishes_block_extents_like_visual`). Host Select suite 19/19;
    GPUI builds clean with `--features window`.
  - **(d.2c) ✅** describe-surface + macro record/replay — **plus two
    half-migration fixes uncovered here**. *Describe-surface*: modal states are
    deliberately NOT mode-registry entries (the two axes don't collapse), so
    `:describe-mode select` is the wrong home — Visual isn't a registered mode
    either. The describe-surface for modal states is the `modal-editing` help
    topic (`docs/user/modal-editing.md`, `:help modal-editing`); documented Select
    there mirroring Visual (modes table row, `gh`/`gH`/`g<C-h>` + `<C-g>`-toggle
    quick-ref rows, a dedicated `## Select mode` section, frontmatter summary).
    *Macro record/replay*: the recorder captures `Action`s (not keystrokes), and
    the printable→overtype is `Action::SelectOvertype(c)` — captured like an
    Insert char; Select entry rides the same `Invoke → EnterSelect` path as
    Visual's `v`. Test `select_overtype_records_and_replays_faithfully`
    (`app/macros.rs`) records enter→motion→overtype and replays in the SAME app
    (recorded `Invoke` embeds a registry-local `CommandId` — macros are not
    portable across registries; restore via undo + replay). **Half-migration
    fixes** (d.2a bound Select's motions but two handlers still gated on
    `Visual(_)` only): (1) the `Effect::SelectionChange` host arm in `dispatch.rs`
    now extends the selection for `Visual(_) | Select(_)` — without this, motions
    in Select COLLAPSED the selection instead of extending it; (2)
    `do_swap_visual_ends` (`o`, bound in Select) now fires for both. Host test
    `motion_extends_and_swap_ends_work_in_select`. Host Select suite 20/20;
    ui-tui macros 10/10; help 48/48; no new reds (full host 732✅/1 pre-existing
    K.3.2; ui-tui 1466✅/5 pre-existing env cluster).
  - **(d.3)** snippet consumer (`snippet_group_cursor_effect` → enter Select over
    multi-char defaults via `Effect::Many([SelectionChange, EnterMode(Select)])`)
    + the doc-debt fix. Mirrors keep the selection on the *focused* group only;
    test that overtyping a default with ≥1 mirror ripples **and** a later `<Tab>`
    lands on the correct post-overtype range. Select-entry `SelectionChange`
    targets the reconciled buffer (SN.3e is in by now).
  Depends on: nothing hard; orthogonal to SN.3e. **Sequencing: SN.3e lands
  first** (per the user) — a smaller, design-settled refactor — then SN.3d.

- **SN.3e ✅ (2026-06-14) — key the snippet session by buffer (finding C; latent
  correctness).**
  `SnippetSession` is one global `Option<ActiveSnippet>` and
  `snippet_active_predicate` is buffer-agnostic, so starting a snippet in
  buffer A then switching to B lights `active-snippet-mode` on B and routes
  `<Tab>` to A's tabstops against B's cursor. The 2026-06-13 reconciler note
  already flagged "a single global slot whose activation target isn't in any
  event payload."

  **Decision: per-buffer map** (`HashMap<BufferId, ActiveSnippet>`), NOT
  suspend/clear-on-switch — the genuinely-better long-term design per
  heuristic #1 (everything-is-a-buffer ⇒ snippet state is buffer-local, not a
  global singleton).

  **Key type (decided at build):** the map is keyed by `lattice_core::BufferId`
  — both producers reach it (mode handlers via the existing
  `core_buffer_id(ctx.buffer_id)`; host paths use `self.document_buffer_id`,
  which is exactly what `sync_keymap_overlays` reconciles against, so the
  set-key always equals the check-key). The protocol↔core split flagged in
  SN.3g is handled at the one conversion site already in `modes.rs`.

  **Scope (as built):** smaller than the original "~50 reader" estimate — most
  of those were test sites. Production surface was ~8 call sites + the predicate
  + the generic reconciler. SN.3e.0 and SN.3e.1 are **compile-coupled** (changing
  `is_active()` → `is_active(buffer)` forces the predicate *and* the reconciler
  call to change together), so they landed as one green commit; SN.3e.2 (the
  multi-buffer test) followed.

  **Sub-slices:**
  - **SN.3e.0 ✅ + SN.3e.1 ✅ (one commit)** — `SnippetSession` →
    `Mutex<HashMap<BufferId, ActiveSnippet>>`; `is_active` / `set` / `clear` /
    `with_mut` each take a `BufferId` (`with_mut` removes-then-reinserts so the
    `*s = None` "end session" closure contract is preserved exactly).
    `snippet_active_predicate` → `Arc<dyn Fn(BufferId) -> bool>`;
    `SessionBackedMinor.active` generalized to `Fn(BufferId) -> bool` and its
    `Debug` impl no longer evaluates the closure (no buffer in scope).
    `sync_keymap_overlays` passes the buffer it reconciles (`document_buffer_id`).
    Readers threaded: snippet handlers (`<Tab>`/`<S-Tab>` `with_mut`, `<Esc>`
    now `clear(core_buffer_id(ctx.buffer_id))`); host expand ×2 + render-snapshot
    ×2 use `document_buffer_id`; ui-tui test reads use `a.editor.document_buffer_id`.
    The completion-popup minor is **inline** in `sync_keymap_overlays` (reads
    host `insert_completion`), not a `SessionBackedMinor`, so it needed no change.
  - **SN.3e.2 ✅** — `sessions_are_isolated_per_buffer` (two buffers, interleaved
    `<Tab>`: A advances, B never moves; clearing A leaves B live) +
    `unknown_buffer_is_inactive_never_panics` (graceful). The SN.3c.2b e2e `<Esc>`
    still passes.

  Tests: 96 `lattice-snippet` ✅ (+1 isolation +1 graceful); host 713 ✅ /
  1 pre-existing red (`arm_missing_arg_prompt_canonical_name_works`, K.3.2,
  unrelated); ui-tui snippet 25 ✅, esc/fall-through e2e ✅ (the only ui-tui reds
  are the documented arg-slot/tutor clusters). `cargo check --tests` clean across
  snippet + host + ui-tui; gpui untouched (nav/session flow through the
  renderer-agnostic registry).

  Depends on: nothing hard; orthogonal to SN.3d. **Next: SN.3d (build Select
  mode), now unblocked.**

- **SN.3f 🗒 — diagnose silent handler-skip (finding D; observability).**
  `SnippetActiveMode::on_activate` (and SN.3c's `SnippetMode::on_activate`)
  silently register no handlers when `CommandRegistryHandle` /
  `ActionHandlerRegistryHandle` / session services are absent. The
  "tolerate test harness" intent is right, but a mis-wired production boot
  would dead-chord with no signal. Add a single `tracing::debug!` on the skip
  path ("active-snippet-mode: nav handlers not registered — services absent")
  — `debug!` not `info!` per `feedback_log_levels`. No behavior change; pure
  observability. Artifacts: test (skip path is hit when a service is absent)
  · no bench/doc.

- **SN.3g 🗒 — minor cleanups (finding E; cosmetic).** Low-risk tidy, batch:
  (1) the snippet completion-source `default_priority: 150` literal
  (`modes.rs`) duplicates `completion.source.snippet.priority`'s default in
  `core_options.rs` — hoist to a shared `const` (or read the option default)
  so the two can't drift; (2) replace the `ctx.buffer_id.raw() as u32`
  narrowing casts (`modes.rs`) with a `lattice_core::BufferId` conversion
  helper (protocol id is u64, core is u32 — unchecked truncation, safe today
  but a footgun); (3) delete `SnippetCompletionMode::options()` — it returns
  the trait default `OptionOverrideSet::default()`, redundant noise.
  Artifacts: tests stay green (behavior-preserving) · no bench/doc.

## Dependency graph

	EF.1 ✅─┬─> MA.1 ✅ ──> MA.2 ✅ ─┐
	        │                        ├─> SN.3
	SN.2 ✅─┴────────────────────────┘   (SN.2a ✅ · SN.2b ✅)
	SN.1 ✅ (independent — landed first to green the reds)

Note (decision B): MA.2 is the **host minor-activation resolver** (subscribe
once to `Event::MajorEntered` → `auto_activatable_minors(major, kind)` →
`activate_mode_by_id`, with `Global` gated to document buffers). The original
"major-selection-on-`DocumentOpened`" scope turned out unnecessary —
documents already activate a major + emit `MajorEntered` via
`activate_major_for_buffer_kind`. SN.2 (snippet ownership half-migration)
closed 2026-06-12; SN.3a/SN.3b (the `snippet-mode` gate + config-driven
policy — the language-aware payoff) landed 2026-06-14.

**Status (2026-06-14):** SN.3c ✅ fully closed — SN.3c.0 (`Mode::action_handlers()`),
SN.3c.1 (mode-owned `<C-x><C-s>` expand, `873fde26`), SN.3c.2a (insert-mode
K.1.c gating + leave relocation, `ad33b2f9`), SN.3c.2b (`fall_through` binding
primitive + snippet `<Esc>` + `:describe-key`, `3df8521b`; e2e test `b89f53c5`;
design fragment keymap-architecture §14 `7283502e`). SN.3f ✅ (`4b49b810`),
SN.3g ✅ (`687ed1c3`). SN.3e ✅ (buffer-keyed snippet session, 2026-06-14).
SN.3d ✅ **complete** (vim Select mode — see
[select-mode.md](../../architecture/select-mode.md), `b14b2fc7` + review-tightening
`db10ba4b`). **d.0 ✅** (`cd5f9cb9`: `ModalState::Select` + `BindingMode::Select`
+ exhaustive-match audit + cursor-shape parity test). **d.1 ✅** (`translate_select`
dispatch + printable-overtype single replace-edit + `<Esc>`/`<C-g>`/`<C-o>`
controls + `do_select_overtype`/`do_exit_select`/`do_toggle_visual_select` host
handlers + shared `selection_extent`; 10 tests).
**d.2 ✅** — d.2a (`7a3f9628`: entry chords `gh`/`gH`/`g<C-h>` +
`register_select_bindings` + Visual≡Select parity test), d.2b (`a8d135ea`:
TUI/GPUI selection-render + status-line label), d.2c (`4d6c3f83`:
`:describe-mode` docs + macro record/replay; two half-migration fixes).
**d.3 ✅** — snippet consumer: a non-empty placeholder default focuses in
charwise Select (`snippet_group_cursor_effect` returns
`Effect::Many([EnterMode(Select), SelectionChange])` on the `<Tab>`-nav path;
`Editor::expand_snippet` mirrors it on initial expand) so the next printable
key overtypes the whole default and drops to Insert; an empty tabstop keeps a
bare Insert cursor. Plus doc-debt (`docs/user/completion.md`); 2 ui-tui tests.

**Snippet activation relocation ✅ (2026-06-13, generic reconciler).**
`Editor::sync_keymap_overlays` previously *polled* `snippet_session.is_active()`
to activate / deactivate `active-snippet-mode` — a snippet-specific seam in a
generic host method (the activation-side half-migration).

Decision: a **generic session-backed-minor reconciler** (B), not the typed-event
path (C). The session is a single global slot whose activation target (the
active buffer) isn't in any event payload, and the synchronous poll has a real
UX property the event path would lose — it reconciles in the *same* apply cycle
that expands the snippet, so the next `<Tab>` always lands (an event→channel→
per-tick-drain path opens a one-tick window against the keystroke contract).
(B) removes the exact rule violation while keeping that synchronous,
drift-proof reconcile.

As landed:
- `Editor` gained a generic `session_backed_minors: Vec<SessionBackedMinor>`
  (`{ active: Arc<dyn Fn() -> bool + Send + Sync>, mode_id }`); `sync_keymap_overlays`
  loops it, toggling each minor on the active buffer via `activate_minor` /
  `deactivate_minor`. No `snippet_session.is_active()` / `SnippetActiveMode`
  literal remains in the generic method.
- `lattice-snippet` owns the policy: `snippet_active_predicate(SnippetSessionHandle)`
  returns the `|| session.is_active()` closure. The host pairs it with
  `SnippetActiveMode::mode_id()` at the boot composition root — `activate_minor`
  *mechanism* stays host machinery (it mutates host-owned per-buffer mode state).
- The completion-popup minor stays inline (its predicate reads host-owned
  `insert_completion` state, legitimately a host concern — not mode-owned data).
- Behavior preserved: 6 `snippet_active_*` ui-tui tests + the SN.2b handler
  tests green; the snippet path activates on expand and deactivates on
  `<Esc>` / walk-off-`$0` exactly as before.

Remaining for SN.3: `SnippetCompletionMode` taking over the `<C-x><C-s>` expand
binding + language-aware activation (the predicate stays; only *who creates the
session* moves from the host into the completion mode).

## Out of scope (separate triage)

The other pre-existing reds discovered alongside the snippet cluster are
**unrelated** and tracked separately:

- **Arg-slot completion (×3)** — `arg_slot_completion_*`,
  `typing_after_popup_*`; same `describe-command` family as the host K.3.2 red
  `arm_missing_arg_prompt_canonical_name_works` (the host red is now FIXED
  2026-06-15: the test was stale — it expected the internal `ex:` prefix to
  leak into the cmdline; the 2026-06-08 picker refactor deliberately strips it).
- **Help tutor (×2)** — `tutor_*` (lesson temp-file / content).

These do not block mode activation and are not part of this plan.
