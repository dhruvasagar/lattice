# Emacs-keys — slice plan

Design fragment: `docs/dev/architecture/emacs-keys.md` (the *what* and
*why*; this file owns the *when* and *in what order*).

Feature: a default-on `emacs-keys` minor mode contributing a configurable
`<C-x>` leader. Tier-1 = buffer/file/save/quit (reuse existing commands);
Tier-2 = pane/window (reuse existing actions); plus two new actions
(`quit-all`, `only`).

## S0 — feasibility spike ✅ (done inline during design)

Verified against source:
- Trie has a generic `Partial` (multi-key) match — `lattice-keymap/src/trie.rs:157`;
  no hardcoded state machine needed for a new prefix.
- A minor mode can bind **multi-key** chords and push them at
  `MinorMode` layer — `crates/lattice-host/src/diff/mode.rs:196`
  (`&[lit_char('d'), lit_char('o')]` → `actions.diff_get`).
- **Default-on** is supported via `ActivationPolicy::Global` ("every
  document buffer") — `lattice-snippet` precedent.
- Reuse targets exist: `AppEffect::SplitPaneHorizontal` / `SplitPaneVertical`
  / `ClosePane` / `NextPane` (`crates/lattice-host/src/actions.rs`,
  `action.rs`); ex-commands `ex:files` / `ex:write` / `ex:buffer-picker`
  / `ex:buffers` / `ex:bdelete` (`excommand.rs` alias table).
- Gaps: **no `quit-all`** (`app_effect.rs` ships `Quit` only) and **no
  close-other-panes / `only`** effect. Both built in S3.

## S1 — `emacs-keys` mode + Tier-1 (reuse) ✅

Landed in three sub-slices:
- **S1a** ✅ — mode + `emacs_keys_layer_bindings` Tier-1 + boot push
  (hardcoded `<C-x>`).
- **S1b.1** ✅ — configurable `emacs-keys-prefix` string option;
  boot reads it; `:set emacs-keys-prefix=…` re-pushes the layer live.
- **S1b.2** ✅ — `emacs-keys` bool enable option (default `true`).
  The mode stays unconditionally `Global`; the *layer* carries the
  gate — `enabled=false` rebuilds the leader map empty, so
  `:set noemacs-keys` reclaims `<C-x>` live without churning the
  per-buffer mode set. 4 unit tests green (bound/partial/unbound,
  alternate-prefix retarget, malformed-prefix degrade, disabled→empty).


- New module `crates/lattice-host/src/emacs_keys.rs` (host-coupled mode,
  mirroring `diff/mode.rs`): `emacs_keys_mode_id()`,
  `emacs_keys_layer_bindings(prefix_chord, actions)` building
  `&[prefix, suffix]` chords into a `KeymapTrie` at
  `KeymapLayer::MinorMode(emacs_keys_mode_id())`.
- Options (typed): `emacs-keys` (bool, default `true`) → folds to
  `ActivationPolicy::Global` / `Manual`; `emacs-keys-prefix` (string,
  default `"<C-x>"`).
- Bindings: `<C-x><C-f>`→`ex:files`, `<C-x><C-s>`→`ex:write`,
  `<C-x>b`→`ex:buffer-picker`, `<C-x><C-b>`→`ex:buffers`,
  `<C-x>k`→`ex:bdelete`.
- Register the layer at boot; auto-activate via the mode's
  `ActivationPolicy`. Wire the prefix-option change to rebuild the layer.
- Introspection: bindings show in `:describe-key` / `:keymap` with source.
- **Tests:** chord resolves to the expected command; `:set noemacs-keys`
  makes `<C-x>` fall through (no tribute); changing `emacs-keys-prefix`
  re-targets the chords; Insert-mode `<C-x>` expansion unaffected.

## S2 — Tier-2 pane bindings (reuse) ✅

- `<C-x>2`→`action:split-pane-horizontal`,
  `<C-x>3`→`action:split-pane-vertical`, `<C-x>0`→`action:close-pane`,
  `<C-x>o`→`action:next-pane`. Resolved by name via the same Tier-1
  mechanism (the pane actions are pre-registered `action:*` commands).
- **Digit-precedence fix (`input::compute_normal_action`):** `<C-x>2` /
  `<C-x>3` were broken at runtime — slice 8.i.4.f hoists digit→count
  parsing ahead of the partial-chord continuation, swallowing the digit.
  The fix (anticipated by 8.i.4.f's own comment) lets a *bound* `[prefix
  + digit]` chord win over count parsing, while an unbound one (`d2w`)
  still counts. Mode-agnostic. Behavior-preserving for all vim count
  flows (749 lib tests green).
  - **Blast radius reviewed (sound, no count regression):** the rule also
    covers the wildcard prefixes `"` / `m` / `` ` `` / `'` / `@` / `q`,
    where a digit is the prefix's *argument* (never a count) — `"5p` now
    selects register 5 (vim-correct), *correcting* a latent mis-count.
    Enumerated prefixes (`g`/`z`/`<C-w>`/operators) bind no `[prefix,1-9]`
    → `<C-w>5+`/`g5`/`d2w` count unchanged; `f`/`t`/`r` use a separate
    binding mode. See the design fragment's digit-precedence bullet for
    the full enumeration + heuristic mapping.
- **False-green caught:** the S1/S2 trie unit tests passed while the
  feature was inert at runtime (the `Global` mode only activates on the
  per-tick `MajorEntered` drain). New integration test
  `tests/emacs_keys_dispatch.rs` boots a real editor, activates the
  leader, and drives the chords through `dispatch_chord` — covers Tier-1
  liveness implicitly + Tier-2 + the digit fix + count regression.
- **Tests:** trie unit `default_prefix_binds_every_tier2_pane_chord`
  (+ id-target check) and 8 integration tests (split-h/split-v/close/next
  resolve + execute; bare digit still counts; + 3 count-regression guards
  for `<C-w>`/`g`/`"` prefixes).

## S3 — build `quit-all` + `only`, bind the rest

**Mechanism correction (vs S0's gap note).** S0 said "no `quit-all`
(`app_effect.rs` ships `Quit` only)" and the plan assumed
`AppEffect::QuitAll` + `Action::QuitAll`. That was the wrong layer:
`AppEffect::Quit` → `Action::Quit` is the brute `<C-c>` quit (no dirty
guard, no pane awareness). `:q` actually flows through the grammar
`Effect::QuitEditor` → pane-aware `Editor::do_quit`, which *already*
implements the close-pane-unless-last semantics. `:qa` therefore mirrors
`:q`'s `Effect` path, not the `AppEffect` path. See the design fragment's
"New commands required" for the full heuristic mapping.

### S3a — `quit-all` (`:qa`) via `QuitScope` ✅

- `QuitScope { Pane, All }` in `lattice-grammar::effect`; extend
  `Effect::QuitEditor { force }` → `{ force, scope }`. `apply_quit` →
  `Pane`, new `apply_quit_all` → `All`, `apply_write_quit` → `Pane`.
- `ex:quit-all` registered (reached by name, no `ExBuiltins` field, like
  `:tabonly`); host aliases `qa` / `qall` / `quitall`.
- `Editor::do_quit(force, scope)` — close-pane short-circuit gated on
  `scope == Pane && len > 1`; shared dirty guard + shutdown below.
  Threaded through the TUI `App::do_quit` wrapper + GPUI arm (parity).
- Bind `<C-x><C-c>` → `ex:quit-all` (Tier-1). **Discovery:** the
  universal `<C-c>` → Quit hatch in `input::translate` short-circuited
  before partial-chord resolution, so `<C-x><C-c>` resolved to the brute
  quit. Fixed by gating the hatch to yield to a bound `[partial + <C-c>]`
  leader continuation — the same digit-precedence rule S2 applied to
  `<C-x>2`. Normal-only (`lookup_normal_with_prefix`); bare `<C-c>` and
  Visual/Insert/Command `<C-c>` unaffected.
- **Tests:** grammar `quit_all_is_editor_scoped` (+ `:q`/`:wq` scope
  Pane assertions); TUI `quit_all_with_multiple_panes_quits_editor`,
  `quit_all_dirty_refuses_then_force_quits`; integration
  `leader_ctrl_c_resolves_quit_all`; emacs-keys unit tier-1 now asserts
  `<C-x><C-c>`. Existing `ctrl_c_quits_*` guards stay green.

### S3b — `only` (`:only`) ✅

- `PaneTree::collapse_to_active` (keep active leaf, drop siblings, no-op
  on single pane); `AppEffect::OnlyPane` + `Action::OnlyPane` +
  `Editor::do_only_pane` (mirrors `do_only_tab`).
- `action:only-pane` registered; `ex:only` emits
  `Effect::AppAction(AppEffect::OnlyPane)` (the `:tabonly` carrier);
  host aliases `only` / `on`. Bind `<C-x>1` → `action:only-pane`.
- Parity: pane `AppEffect`s are host-handled (`Effect::AppAction(_)` is a
  GPUI no-op); the TUI `App` `match action` grouped no-op arm gains
  `Action::OnlyPane`. No renderer-specific change.
- **Tests:** pane `collapse_to_active_keeps_active_drops_siblings` +
  `collapse_single_pane_is_a_noop`; TUI `only_pane_collapses_all_other_panes`
  + `only_pane_single_pane_is_a_noop` (failure-mode); integration
  `leader_1_collapses_to_only_pane`; emacs-keys unit tier-2 asserts
  `<C-x>1`.

## S4 — follow-up polish (post-S3 review)

Landed in two commits.

### Commit 1 (`2beba529`) — pane ex-commands + tab-quit + mode rename ✅

- **`:split` / `:vsplit` / `:close`** (vim `:sp` / `:vs` / `:clo`, emacs
  `C-x 2/3/0`) — pane ops that only existed as chords. No-arg; emit the
  existing Split/Close `AppEffect` carriers like `:only`.
- **`:q` is tab-aware** — last pane of a tab with other tabs open closes
  the TAB (vim tab-page close), not the editor; only the last pane of the
  last tab quits. `:qa` still ignores pane + tab count. (`do_quit` gains a
  `tabs.len() > 1` branch before the dirty guard.)
- **`emacs-keys-mode` rename** — the mode id now carries the conventional
  `-mode` suffix (was bare `emacs-keys`, which read wrong in
  `:describe-mode`). The *option* stays `emacs-keys` (`:set emacs-keys`).

### Commit 2 — universal activation + `-mode` enforcement ✅

- **`ActivationPolicy::Universal`** (new variant; `admits` returns true
  for every `BufferKind`). emacs-keys adopts it so the `<C-x>` leader is
  live in synthetic buffers (`*messages*`, help, file-tree, …), not just
  documents — matching emacs. `Global` stays document-only for content
  modes (snippets, LSP). Normal-only layer ⇒ Terminal-Insert passthrough
  unaffected.
- **`-mode` suffix enforced at registration** — `ModeRegistry::register`
  returns `RegistrationError::MissingModeSuffix` when a mode id lacks
  `-mode`. The single choke point every built-in + future plugin mode
  flows through; M.2 groups aren't modes and bypass it. Caught the
  `emacs-keys` slip; renamed ~5 non-conforming test fixtures
  (`oil-a`→`oil-a-mode`, `test-global-minor`→`…-mode`, the
  `test-mode/<x>` helper appends `-mode`, …). True compile-time wasn't
  feasible (`ModeId` is a runtime-interned string shared with non-mode
  groups); registration-time is the uniform enforcement point.

## Status

S0 ✅ · S1 ✅ (S1a · S1b.1 · S1b.2) · S2 ✅ · S3 ✅ (S3a · S3b) · S4 ✅
