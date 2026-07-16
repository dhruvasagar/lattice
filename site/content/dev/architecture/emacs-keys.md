+++
title = "Emacs-keys — a configurable `<C-x>` leader (tribute layer)"
+++

# Emacs-keys — a configurable `<C-x>` leader (tribute layer)

A built-in, default-on minor mode (`emacs-keys`) that contributes an
emacs-style `<C-x>` prefix in Normal (and Visual) mode. The prefix is a
configurable leader; its sub-bindings invoke commands and actions that
already exist (plus two small new ones). It is a tribute to emacs muscle
memory, **not** a change to the vim grammar.

## Why a minor mode, not Builtin

`<C-x>` is not vim grammar. The Builtin keymap is reserved for universal
vim grammar (`feedback_mode_owns_its_surface`). Shipping the tribute as a
separable, named, default-on minor mode:

- keeps the vim Builtin keymap pure (paramount goal #3, strict vim);
- makes the tribute toggleable — `:set noemacs-keys` reclaims `<C-x>`, so
  a vim purist can have it back (e.g. for a future `<C-a>` / `<C-x>`
  number increment/decrement, which lattice does not implement today);
- keeps the binding choice **and** the handler wiring with the mode that
  owns them, not the host.

The mode owns its keymap layer, its prefix option, and its activation
policy. The two new actions it needs (`quit-all`, `only`) are host-level
pane/lifecycle actions and live with the other host actions, exactly as
the `diff-mode` chords target host `action:diff-get` / `action:diff-put`.

## The `<C-x>` slot

- **Normal `<C-x>` is unbound today** — no increment/decrement exists, so
  the prefix clobbers nothing.
- **Insert `<C-x>` is taken and stays untouched** — it is vim's expansion
  prefix (`<C-x><C-s>` snippet expand, `AfterCtrlX`). emacs-keys is
  Normal/Visual only, a different binding mode → no conflict.
- **Trade named:** claiming Normal `<C-x>` forecloses giving vim's
  number-decrement its canonical binding. The toggle is the escape hatch;
  this is a deliberate, reversible default, not a silent grab.

## Configurable prefix (leader), not a general `mapleader`

`emacs-keys-prefix` (chord string, default `"<C-x>"`) sets the leader.
The mode rebuilds its keymap layer as `&[prefix, suffix]` per binding, so
changing the option re-targets every tribute chord.

This is a **mode-local** configurable prefix. It is deliberately *not* a
general vim `mapleader`: introducing a foundational, reusable leader
mechanism as a side effect of a tribute — and defaulting it to a non-vim
key (`<C-x>` vs vim's `\`) — would design the leader around emacs rather
than on its own merits (heuristic #1). A general `mapleader` remains a
separate future feature with vim-faithful defaults; if it lands, the
emacs-keys prefix can key off it then.

## Default leader-map

emacs convention: `<C-x>b` = *switch* buffer (picker), `<C-x><C-b>` =
*list* buffers — distinct from each other and from vim's `<C-w>` family,
which is untouched and coexists.

| Chord        | Emacs                | Action            | Target                            | Status |
|--------------|----------------------|-------------------|-----------------------------------|--------|
| `<C-x><C-f>` | find-file            | open file (picker)| `ex:files`                        | reuse  |
| `<C-x><C-s>` | save-buffer          | save              | `ex:write`                        | reuse  |
| `<C-x>b`     | switch-to-buffer     | switch buffer     | `ex:buffer-picker`                | reuse  |
| `<C-x><C-b>` | list-buffers         | list buffers      | `ex:buffers`                      | reuse  |
| `<C-x>k`     | kill-buffer          | delete buffer     | `ex:bdelete`                      | reuse  |
| `<C-x><C-c>` | save-buffers-kill     | quit all          | `ex:quit-all` (**new**)           | build  |
| `<C-x>2`     | split-window-below   | split horizontal  | `AppEffect::SplitPaneHorizontal`  | reuse  |
| `<C-x>3`     | split-window-right   | split vertical    | `AppEffect::SplitPaneVertical`    | reuse  |
| `<C-x>0`     | delete-window        | close pane        | `AppEffect::ClosePane`            | reuse  |
| `<C-x>1`     | delete-other-windows | close other panes | `AppEffect::OnlyPane` (**new**)   | build  |
| `<C-x>o`     | other-window         | focus next pane   | `AppEffect::NextPane`             | reuse  |

## New commands required

### `:qa` (quit-all) — distinct command, one effect with a `QuitScope`

`:q` is **already** pane-aware: `Editor::do_quit` closes the active pane
when more than one is open and only shuts the editor on the last pane
(running a dirty guard over *all* document buffers unless forced). That
is exactly vim's `:q`; no change needed there. The runtime path is
`ex:quit` → `apply_quit` → `Effect::QuitEditor { force }` →
(TUI + GPUI) `do_quit(force)`.

`:qa` is a **distinct command** — it ignores pane / tab count and shuts
the editor outright (subject to the same dirty guard). The S0 spike's
note that "`app_effect.rs` ships `Quit` only" pointed at the *wrong*
layer: that `AppEffect::Quit` → `Action::Quit` path is the brute `<C-c>`
quit (no dirty guard, no pane awareness). `:qa` must NOT route there —
it would either skip the dirty guard the user requires or duplicate it
in the wrong layer. Instead `:qa` mirrors `:q`'s grammar-`Effect` path.

The chosen model (heuristic #1, on merit): `:q` and `:qa` are the same
*verb* differing on one axis — **scope**. So they stay distinct commands
(separate `ex:quit` / `ex:quit-all` registrations, separate aliases
`qa` / `qall` / `quitall`, separate help) but share **one** effect and
**one** host method:

- `Effect::QuitEditor { force, scope: QuitScope }`, `QuitScope ∈ {Pane,
  All}` (`lattice-grammar::effect`). `apply_quit` → `Pane`,
  `apply_quit_all` → `All`, `apply_write_quit` → `Pane`.
- `Editor::do_quit(force, scope)`: the close-pane short-circuit is gated
  on `scope == Pane && pane_count > 1`; the dirty guard + shutdown below
  it are identical for both. Sibling variants (`Effect::QuitAll` + a
  second `do_quit_all`) were rejected: they duplicate the guard and
  double the TUI/GPUI parity arms for a one-branch difference
  (kept-separate-for-symmetry, which heuristic #1 forbids).

`<C-x><C-c>` (emacs `save-buffers-kill-emacs`) targets `ex:quit-all`, so
it inherits the dirty guard — the emacs-faithful behavior — rather than
the brute `<C-c>` quit. The universal `<C-c>` → quit hatch in
`input::translate` is gated to yield to a bound `[partial + <C-c>]`
leader continuation (the same digit-precedence rule as `<C-x>2`), so the
two-key emacs chord resolves while a bare `<C-c>` still hard-quits.

### `:only` (close other panes) — mirrors `:tabonly`

No close-other-panes op existed (emacs `C-x 1`, vim `<C-w>o`). `:only`
is a pane op exactly like the existing `:tabonly`, so it reuses that
shape: `AppEffect::OnlyPane` + `Action::OnlyPane` + `Editor::do_only_pane`
(backed by a new `PaneTree::collapse_to_active` that keeps the active
leaf and drops its siblings in one step, no-op on a single pane). The
chord `<C-x>1` targets `action:only-pane`; the ex-command `ex:only`
(aliases `only` / `on`) emits `Effect::AppAction(AppEffect::OnlyPane)` —
the same carrier `:tabonly` uses for `OnlyTab`.

## Mechanism

- **Keymap:** a `KeymapTrie` of multi-key chords pushed once at boot under
  `PushLayerKind::MinorMode(emacs_keys_mode_id())` — the `diff-mode`
  pattern (`crates/lattice-host/src/diff/mode.rs:196`). The trie's generic
  `Partial` match (`lattice-keymap/src/trie.rs`) resolves the two-key
  sequence; no new `BindingMode` variant is needed.
- **Default-on + enable / prefix toggles:** the mode is unconditionally
  `ActivationPolicy::Universal` (active on **every** buffer kind —
  documents *and* synthetic buffers: `*messages*`, help, file-tree, oil,
  terminal). This is deliberately broader than `Global` (which is
  document-only — the right scope for content modes like snippets):
  emacs-keys is a *universal* navigation leader, so `<C-x>b` / `<C-x>o` /
  `<C-x><C-c>` must work everywhere the user can focus, mirroring emacs'
  `C-x` map being live in `*Messages*`. The Normal-only layer means
  Terminal-Insert keystroke passthrough is unaffected, and the leader
  never shadows a synthetic buffer's own Normal-mode chords (Help's
  Esc / Enter / `-` are distinct keys). The *layer* is
  the gate, not mode activation: the `emacs-keys` enable flag and the
  `emacs-keys-prefix` value are applied by (re)building the layer's
  contents — **empty when disabled** — at boot and live on `:set` via the
  dispatcher's option-change handler (`push_layer` is idempotent on
  `mode_id`, so a re-push replaces rather than appends). K.1.c's
  per-keystroke filter scopes the chords to the (document) buffers where
  the mode is active. A policy-cell fold (deactivating the mode itself
  when disabled, à la snippet) is a possible future refinement; the
  layer-gate keeps the mode a pure marker and unifies enable + prefix into
  one re-push.
- **Prefix token:** the configured prefix string is parsed to a chord and
  prepended to each binding's suffix at layer-build time.
- **Digit suffixes vs. vim counts (S2):** the pane chords `<C-x>2` /
  `<C-x>3` / `<C-x>0` collide with vim's count parser. Slice 8.i.4.f
  hoists digit handling *ahead* of the partial-chord continuation so
  `d2w` / `2d3w` / `5gg` see the digit — which would swallow the `2` in
  `<C-x>2` as a count. The refinement (anticipated verbatim by 8.i.4.f's
  own comment): when a pending prefix actually **binds** `[prefix +
  digit]` as a chord, the digit is that chord's literal second key, not a
  count, so the trie wins; an unbound `[prefix + digit]` (e.g. `[d, 2]`)
  still falls through to the count accumulator. The check is
  mode-agnostic — any layer binding a `[prefix, digit]` chord benefits.
  Lives in `input::compute_normal_action`.
  - **Blast radius (verified safe for counts):** the rule also covers the
    `CharLiteral`-wildcard prefixes `"` / `m` / `` ` `` / `'` / `@` / `q`,
    whose next key matches the wildcard. There a digit is the prefix's
    *argument* (register / mark / macro name), never a count — so `"5p`
    now selects numbered register 5 (vim-correct) instead of being
    mis-counted, *correcting* a latent bug. No count flow regresses:
    counts after these prefixes always come before the prefix (`2"ap`)
    or after its argument (`"a2p`), never as the immediately-following
    key. Enumerated prefixes (`g`, `z`, `<C-w>`, operators) bind no
    `[prefix, 1-9]`, so `<C-w>5+`, `g5`, `d2w` count exactly as before;
    char-arg motions `f` / `t` / `r` use a separate `AfterFindChar`
    binding mode and never reach this code. Guarded by
    `tests/emacs_keys_dispatch.rs` (`window_prefix_then_digit_still_counts`,
    `g_prefix_then_digit_still_counts`,
    `register_prefix_then_digit_selects_register_not_count`).
- **Activation is per-tick, not at registration:** because the mode is
  `Global`, it enters `active_modes` when the buffer's `MajorEntered`
  event is drained by the generic resolver (`drain_minor_activation`,
  the same path that activates `snippet-mode`) — the running editor does
  this each tick. The keymap layer is pushed at boot, but the *chord only
  fires once the mode is active*; tests must drive a `MajorEntered` drain
  (trie-only unit tests bypass this — see `tests/emacs_keys_dispatch.rs`).
- **Introspection:** every binding carries `SourceLocation::builtin_file`,
  so `:describe-key` / `:keymap` render the tribute bindings with
  provenance (DESIGN.md §5.11.1).

## Paramount-goal alignment

- **#1 perf:** one additional minor-mode layer; chord resolution stays
  trie-O(depth). The existing keymap bench already exercises synthetic
  minor-mode layers; the tribute reuses that shape.
- **#2 extensibility:** configurable prefix + per-binding toggle via typed
  options; the mode owns its full surface.
- **#3 strict vim:** Builtin stays pure vim; the tribute is separable and
  toggleable.
- **UX (higher court):** emacs muscle memory works out of the box; no
  flicker, no hot-path cost; vim users reclaim `<C-x>` with one `:set`.

## Rejected alternatives

- **Builtin placement** — smudges `default_keymap()` ("the vim default
  keymap", with a drift test) with non-vim bindings, and isn't toggleable
  as a unit. Rejected on heuristic #1 + the mode-ownership standing rule.
- **General `mapleader` now** — introduces a foundational mechanism as a
  side effect of a tribute and bakes a non-vim leader default. Deferred as
  a separate, vim-faithful feature.

See the slice plan for sequencing:
`docs/dev/operations/slice-plans/archive/emacs-keys.md`.
