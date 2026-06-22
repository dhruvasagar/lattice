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

## S3 — build `quit-all` + `only`, bind the rest 🗒

- `AppEffect::QuitAll` + `Action::QuitAll` + `ex:quit-all` (aliases
  `qa`, `qall`) + handler. Bind `<C-x><C-c>`→`ex:quit-all`.
- `AppEffect::OnlyPane` + pane-tree collapse-to-active + `ex:only`
  (alias `on`) + handler. Bind `<C-x>1`→`action:only`.
- TUI + GPUI parity for any new `AppEffect` classification.
- **Tests incl. failure modes:** `:qa` sets `should_quit` and fires
  `BeforeQuit`; `:qa` with a dirty buffer follows the existing dirty-quit
  policy; `only` on a single pane is a no-op (no panic).

## Status

S0 ✅ · S1 ✅ (S1a · S1b.1 · S1b.2) · S2 ✅ · S3 🗒
