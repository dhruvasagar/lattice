# Slice plan: K.3 — Help-prefix bindings (`<C-h>` map)

**Design:** [keymap-architecture.md §12](../../architecture/keymap-architecture.md#12-help-prefix-bindings-c-h-map).

**Status:** 🗒 spec'd, gated on K.2 (keymap substrate). Thin
slice — only adds bindings + verifies ambiguous-leaf
timeout resolution.

**Why:** Lattice ships emacs-style discoverability for the
self-documenting help facility (design.md §5.11) — every
`:describe-*` command reachable from a single Normal-mode
prefix. Vim has `K` for buffer-context hover; that stays.
Emacs's `C-h` map covers cross-cutting introspection
(keymap / command / option / mode / event / buffer / apropos)
which Lattice has the metadata to support out of the box.

## Sequencing

### K.3.0 — Ambiguous-leaf resolution (trie audit)

Before adding the bindings, verify the keymap trie
dispatcher supports the `<C-h>`-as-both-leaf-and-prefix
shape described in [keymap-architecture.md §12.3](../../architecture/keymap-architecture.md#123-ambiguous-leaf-resolution):

- Trie node carries both a value and children.
- Dispatcher arms a `timeoutlen` timer on partial match.
- Follow-on chord → fires deeper match.
- Timeout / non-matching follow-on → fires bare value.

Audit `crates/lattice-host/src/keymap_trie.rs` +
`input.rs` (dispatcher) for the existing chord-timeout
machinery. If missing or partial, extend it here — the same
mechanism vim uses for general chord ambiguity. Test
coverage: a synthetic mini-trie with a leaf+prefix node;
assert (a) deeper match wins when follow-on arrives in
time, (b) bare value fires on timeout, (c) bare value
fires on non-matching follow-on.

### K.3.1 — `:help-for-help` command

Register `:help-for-help` as an alias of the existing
`:help` ex-command (per design.md §5.11). `:help-for-help`
is the canonical name to match emacs vocabulary; `:help`
remains the user-facing entry point.

### K.3.2 — Help-prefix keymap module

Create `crates/lattice-host/src/keymap_help.rs` with
`register_help_prefix_bindings(&mut KeymapHandle, &ActionIds)`
that inserts the §12.1 binding table at
`KeymapLayer::Builtin`, `BindingMode::Normal`. Boot path
calls this once alongside the other built-in registration
helpers (`register_normal_bindings`, etc.).

Bindings to wire (per the §12.1 table):

| Chord | Action |
|---|---|
| `<C-h>` | `Action::ExCommand("ex:help-for-help")` |
| `<C-h><C-h>` | `Action::ExCommand("ex:help-for-help")` |
| `<C-h>k` | `Action::ExCommand("ex:describe-key")` |
| `<C-h>c` | `Action::ExCommand("ex:describe-command")` |
| `<C-h>o` | `Action::ExCommand("ex:describe-option")` |
| `<C-h>e` | `Action::ExCommand("ex:describe-event")` |
| `<C-h>m` | `Action::ExCommand("ex:describe-mode")` |
| `<C-h>b` | `Action::ExCommand("ex:describe-buffer")` |
| `<C-h>a` | `Action::ExCommand("ex:apropos")` |
| `<C-h>K` | `Action::ExCommand("ex:keymap")` |

(Exact `Action` shape depends on the post-K.2 dispatch
encoding; if `CommandInvocation::of(ex_command_id)` is the
post-K.2 idiom, use that.)

### K.3.3 — Mode-scope enforcement

Confirm by test that the bindings fire **only in Normal
mode**. Per K.1.c per-keystroke filter, bindings at
`KeymapLayer::Builtin` with `BindingMode::Normal` only
match when the active binding-mode is `Normal`. Test cases:

- Normal mode: `<C-h>k` → `:describe-key`. ✓
- Insert mode: `<C-h>` → existing backspace behavior. ✓
  (no help-prefix interference)
- Visual mode: `<C-h>` → unbound (or pre-existing binding).
  No help-prefix interference.
- OperatorPending: `<C-h>` → unbound. No help-prefix.
- Cmdline (rich minibuffer): `<C-h>` → existing
  cmdline backspace. No help-prefix.

### K.3.4 — Doc + bench artefacts

- ✅ Design landed: [keymap-architecture.md §12](../../architecture/keymap-architecture.md#12-help-prefix-bindings-c-h-map).
- Update `docs/user/modal-editing.md` with a "Discovering
  the editor" section pointing at `<C-h>`.
- BENCHMARKS row: dispatcher latency on
  ambiguous-leaf resolution should be within the existing
  K.1.a budget (sub-µs per keystroke; the timeout
  bookkeeping is one arm + one cancel, no allocation).

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
