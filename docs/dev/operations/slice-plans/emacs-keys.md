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

## S2 — Tier-2 pane bindings (reuse) 🗒

- `<C-x>2`→`SplitPaneHorizontal`, `<C-x>3`→`SplitPaneVertical`,
  `<C-x>0`→`ClosePane`, `<C-x>o`→`NextPane`.
- **Tests:** each chord triggers the corresponding pane action;
  single-pane edge cases behave (close/next no-op or sane).

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

S0 ✅ · S1 ✅ (S1a · S1b.1 · S1b.2) · S2 🗒 · S3 🗒
