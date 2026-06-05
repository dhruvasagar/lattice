# Slice plan: K.3 — Help-prefix bindings (`<C-h>` map)

**Design:** [keymap-architecture.md §12](../../architecture/keymap-architecture.md#12-help-prefix-bindings-c-h-map).

**Status:** ✅ landed (2026-06-02). K.2 closed at commit
`0938572`; K.3 shipped as a thin slice on top, ending at
K.3.4. The original ambiguous-leaf-timeout verification
plan was rejected in favor of Option 2 (no bare `<C-h>`
leaf binding) — see K.3.0 below.

**Why:** Lattice ships emacs-style discoverability for the
self-documenting help facility (design.md §5.11) — every
`:describe-*` command reachable from a single Normal-mode
prefix. Vim has `K` for buffer-context hover; that stays.
Emacs's `C-h` map covers cross-cutting introspection
(keymap / command / option / mode / event / buffer / apropos)
which Lattice has the metadata to support out of the box.

## Sequencing

### K.3.0 — Ambiguous-leaf resolution (trie audit) ✅ (2026-06-02)

Audit completed. **Finding:** `keymap_trie.rs:251-263`
returns `Bound` immediately when a node has a binding, even
if it also has children. The dispatcher has no `timeoutlen`
machinery anywhere — partial-chord handling goes through
`Action::AbsorbPartialChord` which only fires when the trie
returns `Partial` (no leaf binding at this node). No
mid-state "Bound-but-also-has-children" signal exists for a
timer to act on.

**Decision (user-confirmed):** Option 2 — skip the bare
`<C-h>` leaf binding. `<C-h>` stays a pure prefix node.
`<C-h><C-h>` and `<C-h>?` serve as the explicit
`:help-for-help` entry points. One keystroke of friction;
zero new infrastructure.

The discarded alternatives + rationale are recorded in
[keymap-architecture.md §12.3](../../architecture/keymap-architecture.md#123-no-bare-c-h-leaf--option-2-from-k30-2026-06-02).
Full `timeoutlen` (Option 1) is the right long-term answer
when timer-driven dispatch arrives for other reasons (a
future `j10j` count-absorbing slice, say). At that point
the bare `<C-h>` binding can land alongside.

### K.3.1 — `:help-for-help` command ✅ (commit `ed2a1bf`)

`("help-for-help", "ex:help")` row added to the host's
ex-command alias table in
`crates/lattice-host/src/excommand.rs`. Resolves to the
same canonical `ex:help` command; `:help` remains the
user-facing entry point.

### K.3.2 — Help-prefix keymap module ✅ (commit `ed2a1bf`)

Landed. `crates/lattice-host/src/keymap_help.rs` registers
the §12.1 binding table at `KeymapLayer::Builtin`,
`BindingMode::Normal`. Each row carries a
`CommandInvocation::of(cmd_id)` — the K.2 post-stub
encoding. The function takes `&CommandRegistry` and resolves
each canonical `ex:*` name via `id_by_name`; unresolvable
names emit `tracing::warn!` and skip the binding (matches
the K.2.4.A.0.3 translation-pass convention).

Boot wiring in `editor_boot.rs` calls
`register_help_prefix_bindings(&h, &registry)` right after
the four `register_<mode>_bindings` calls
(replace / visual / insert / normal). `registry` is the
host's `CommandRegistry` populated earlier in boot by
`lattice_grammar::ex_commands::populate` and
`crate::actions::populate`.

Bindings wired (per the §12.1 table, Option-2-shaped):

| Chord | Command |
|---|---|
| `<C-h><C-h>` | `ex:help` (resolves `:help-for-help` alias) |
| `<C-h>?` | `ex:help` (easier-to-type form) |
| `<C-h>k` | `ex:describe-key` |
| `<C-h>c` | `ex:describe-command` |
| `<C-h>o` | `ex:describe-option` |
| `<C-h>e` | `ex:describe-event` |
| `<C-h>m` | `ex:describe-mode` |
| `<C-h>b` | `ex:describe-buffer` |
| `<C-h>a` | `ex:apropos` |
| `<C-h>K` | `ex:keymap` |

6 unit tests (in the same file): drift-test that every row's
name resolves; positive checks that `<C-h><C-h>` and
`<C-h>k` produce `Bound` with the right `CommandId` at the
right layer; verification that bare `<C-h>` returns
`Partial`; warn-and-skip behavior when the registry is
empty; one Insert-mode negative.

### K.3.3 — Mode-scope enforcement ✅ (commit `05c2e25`)

5 mode-scope tests added to `keymap_help.rs`'s test module
(complementing the Normal positive + Insert negative from
K.3.2). Each asserts
`KeymapHandle::lookup_with_context(<binding-mode>,
&[<C-h>, k], &[])` returns `Unbound`:

- `help_prefix_does_not_fire_in_visual_mode`
- `help_prefix_does_not_fire_in_operator_pending_mode`
  (critical: `<C-h>` mid-operator must NOT absorb the chord
  and surprise the user)
- `help_prefix_does_not_fire_in_cmdline_mode`
  (`BindingMode::Command` — cmdline backspace stays put)
- `help_prefix_does_not_fire_in_search_mode`
  (`/` `?` minibuffer — search-line backspace stays put)
- `help_prefix_does_not_fire_in_replace_mode`
  (`<C-h>` in Replace is restore-last-overwritten-byte)

The K.1.c per-keystroke filter enforces the scoping
naturally — bindings register with `BindingMode::Normal`,
other modes don't match — but the tests pin the contract
so a future regression that broadened the registration
would surface immediately.

### K.3.4 — Doc + bench artefacts ✅ (this commit)

- ✅ Design landed: [keymap-architecture.md §12](../../architecture/keymap-architecture.md#12-help-prefix-bindings-c-h-map).
  §12.1 binding table updated to Option-2 shape (no bare
  `<C-h>`; explicit `<C-h><C-h>` + `<C-h>?` for
  help-for-help). §12.3 rewritten as "No bare `<C-h>` leaf
  — Option 2 from K.3.0" — records the decision +
  discarded alternatives + when full `timeoutlen` (Option 1)
  becomes the right answer.
- ✅ User docs: section added in
  [`docs/user/modes.md`](../../user/modes.md) and the
  modal-editing.md pointer (this slice's commit).
- ✅ This slice plan + ledger refresh.
- 🗒 BENCHMARKS row deferred. Pre-K.3.0, the bench would
  have targeted timer-driven dispatch latency; Option 2
  removes the timer, so the relevant numbers are already
  measured by the existing
  `keymap_trie_lookup_partial` row in
  [`benchmarks.md`](../../operations/benchmarks.md)
  (11.8ns, well inside the §8.2 budget).

## Risk + roll-back

- **Risk:** if the trie dispatcher lacks timeout-based
  ambiguity resolution today, K.3.0 expands the slice
  beyond "thin bindings add." Mitigation: timebox the
  audit; if extension needed, scope it as K.3.0 explicitly,
  not hidden inside K.3.2.
- **Roll-back:** `register_help_prefix_bindings` is a
  single call site in boot; commenting it out reverts the
  feature. No state migration required.

## Cross-references

- Design: [keymap-architecture.md §12](../../architecture/keymap-architecture.md#12-help-prefix-bindings-c-h-map).
- Gated on: [slice plan: keymap-substrate](./keymap-substrate.md) (K.2).
- Touches: [design.md §5.11](../../architecture/design.md) (self-documenting help — already spec'd).
