# Slice plan: motions in Visual (VM)

Design: [keymap-architecture.md §15](../../architecture/keymap-architecture.md).

## Why

Reported 2026-09-15: "Motions should work in visual mode, such as `[[` in org,
`C-d`, `C-b` … this should work for all motions even those registered from
plugins."

Diagnosis: nothing about Visual's *behaviour* was broken.
`Editor::write_through_caret` rebuilds the selection from `visual_anchor` +
`cursor` after every dispatch, so any reachable cursor-mover already extends
the selection. The chords simply resolved to nothing, because Visual's motion
surface was a re-registration of `keymap_normal::motion_rows` and several
motions never entered that table.

Enumerated against the Builtin layer at boot, the casualties were:

| Chord | Kind | Dead in |
| --- | --- | --- |
| `gg`, `<C-d>`, `<C-u>`, `<PageUp>`, `<PageDown>` | `Motion` | Visual, Select, operator-pending |
| `f` / `F` / `t` / `T` | `Motion` | Visual, Select |
| org `[[`, `]]`, `g{` | `Motion` (plugin) | Visual, Select |
| `<C-f>`, `<C-b>`, `<C-e>`, `<C-y>`, `%`, `;`, `,`, `H`, `M`, `L`, `n`, `N`, `*`, `#`, `` `x ``, `'x`, `gj`, `gk`, `g0`, `g$` | `Action` | Visual, Select, operator-pending |

The first three rows are one defect — a hand-kept list that drifted. The last
row is a different one: those commands are typed as actions, and most of them
are genuine motions in vim (`d%`, `dn`, `dH`, `d;` all work there).

## Slices

### VM.1 ✅ — the derivation

`keymap_normal::expand_plugin_mode_grammar_rows` already derived operator rows
from a mode layer's motions and a Visual row from its text objects. Generalised
to `expand_grammar_rows`: kind-driven, bind-if-absent, deriving **Visual +
Select + operator-pending** rows for `Motion` and for `TextObject`, and run at
boot over `KeymapLayer::Builtin` as well as over every mode layer.

- `crates/lattice-host/src/keymap_normal.rs` — the pass.
- `crates/lattice-host/src/editor_boot.rs` — run it last, after every builtin
  binder and after `translate_mode_keymaps`, so it fills gaps rather than
  overwriting deliberate bindings.
- `crates/lattice-protocol/src/chord.rs` — `ChordPattern: Hash`, so the
  bind-if-absent snapshot is a set rather than a linear scan.
- `crates/lattice-host/tests/a_motion_is_live_in_visual.rs` — 6 tests: the
  exhaustive kind-driven property over the Builtin layer, the four dead
  families by name, the operator half, a behavioural check that `<C-d>` in
  Visual moves the cursor *and* leaves the selection spanning what it crossed,
  the mode-layer (plugin-shaped) case, and bind-if-absent.

### VM.2 ✅ — retire the copies

With the derivation in place, `keymap_visual` and `keymap_select` no longer
list motions; `motion_rows` / `syntax_motion_rows` keep only their Normal and
operator-pending consumers, and their doc comments stop claiming to be a single
source of truth for surfaces they never reached. The Visual↔Select parity test
now asserts against the derivation's output rather than against two lists
happening to agree.

Touches the three test/bench harnesses that built a keymap by hand
(`lattice-ui-tui`'s `keymap_visual` tests, its `input.rs` `build_base_keymap`,
and `benches/keymap.rs`) — each now runs the derivation, because without it the
handle they build dispatches every motion chord to `Action::None`.

### VM.3a ✅ — `MotionSpec::jump` is read

`jump` had been declared on every motion since the grammar's first slice and
read by nobody. `run_document_invocation` decided what counted as a jump with a
hardcoded `inv.command == goto_first_line || inv.command == goto_last_line`, so
`gg` and `G` recorded a position-history entry and `}`, `{`, `(`, `)`, the
sixteen tree-sitter structural motions and every plugin motion did not — all of
them setting `jump: true` into a field that went nowhere. Org's headline motions
say so in their own source comment.

Now `CommandRegistry::motion_is_jump(id)`, and the same answer drives
fold-opening at the destination (vim's `foldopen` ships with
`block,mark,percent,search,tag,jump`). Only a bare motion reaches the check as
`inv.command` — an operator arrives as the operator's id — so `d}` records
nothing with no special-casing.

There were **two** copies of the hardcoded pair: `run_document_invocation` and
`run_read_only_motion`. Both fixed; fixing one would have meant `}` records a
jump in a file and not in `:help`.

### VM.3b ✅ — `%` is a motion, and inclusive means inclusive

`%` was `action:match-bracket`. An action takes no operator and VM.1's
derivation only mirrors motions into Visual, so `d%`, `y%` and `v%` were all
unbound. It is `motion:match-pair` now (`jump: true`, `exclusive: false`), and
the scan moved into `lattice-grammar`. `AppEffect::MatchBracket` stays — it
crosses the WIT boundary — and `do_match_bracket` delegates to the motion
rather than keeping a second copy of the scan.

**The part that was not about `%`.** `motion_to_range` implemented "inclusive"
as direction-dependent: forward covered the character at the target, backward
returned `[target, cursor)` and dropped the cursor's own character. Vim's rule
(`:h exclusive`) is about the buffer, not the direction of travel — "the last
character towards the end of the buffer" is included — so backward-inclusive is
`[target, cursor + 1)`.

Nothing caught it because its only users were `F` and `T`, which are exclusive
in vim and were registered here as inclusive: two errors cancelling, producing
the right range for the wrong reason. `%` is the first genuinely-inclusive
bidirectional motion in the tree and walked straight into it — `d%` from the
closing bracket deleted `(abc` and left the `)`.

Fixed as both halves, because either alone moves `dF` / `dT`:

- the backward branch now includes the cursor's character;
- `F` / `T` → `exclusive: true` (what vim says, and what they already did);
- `k` → `exclusive: true` too. It only ever travels backward, so that
  reproduces `dk` exactly. Its honest answer is **linewise**, but `MotionSpec`
  has no flag for that (`linewise` is decided per RESULT) and the engine has no
  linewise-target expansion — so `exclusive` is the only knob and this is the
  value that does not silently change what `dk` deletes.
- `gg` / `G` keep `exclusive: false`: genuinely bidirectional, so a backward
  `dgg` now includes the cursor's character — closer to vim's linewise answer,
  not further.

`dF`, `dT` and `dk` are pinned through real keystrokes so the reclassification
is provably behaviour-preserving rather than argued to be.

**Test-harness note.** `Editor::dispatch_chord` cannot express operator+motion:
the absorb effect pushes the operator prefix into `Editor::partial_chord` while
`dispatch_chord` resolves against the `&mut Vec` its caller threads. A host-side
harness fires the BARE motion, so `d%` "passed" while deleting nothing. The
composition tests live in `lattice-ui-tui/src/app/motion_composition.rs` over
`test_helpers::press_chars`. See `app/edit.rs`'s prose warning, which predates
this and says the same thing.

### VM.3c 📝 — `;` / `,`

Needs the last `f`/`F`/`t`/`T` (`Editor::last_find`) to reach the grammar.
`GrammarEnv` is the established seam for host state (it already carries
`selection`, `indent`, `textwidth`, `indent_resolver`, `native_format`), so
`FindKind` + `LastFind` move down into `lattice-grammar`, `DispatchEnv` and
`MotionContext` carry them, and the host re-exports. `do_find_repeat` already
synthesises a `CommandInvocation` against the find-char motion, so the body is
nearly a motion already.

### VM.3d 📝 — `n` / `N` / `*` / `#`

Needs the session search pattern + direction in the env. Same shape as VM.3c.

### VM.3e 📝 — `` `x `` / `'x ``

Needs the mark table. `'x` is linewise, which runs into the same missing
`MotionSpec` flag VM.3b documented — likely blocked on that.

### VM.3f 📝 — `H` / `M` / `L`

Viewport-relative, and fold- and line-height-aware
(`Editor::line_forward_by_budget`). Layout knowledge must NOT move into
`lattice-grammar`; the seam is a **`ViewportResolver`** trait in `GrammarEnv`,
exactly as `ScopeResolver` and `IndentResolver` already are — the host
implements it, the grammar calls it. Linewise motions in vim, so also gated on
the linewise gap for full fidelity.

### VM.3g 📝 — `gj` / `gk` / `g0` / `g$`

Display-line motions. Same `ViewportResolver` seam as VM.3f; the resolver
answers "the position N display lines from here". Hardest of the set.

### VM.3h 📝 — the scrollers, which are NOT motions

`<C-f>` / `<C-b>` / `<C-e>` / `<C-y>` are *scrolling* commands in vim, not
motions — `d<C-f>` is not a valid operator target there, and `<C-e>` / `<C-y>`
do not move the cursor at all unless it would leave the window. They still need
to be reachable in Visual and Select, so they get their mode-set declared at
their own registration site (three `handle.bind` calls, NOT `bind_modes`, which
rebuilds the merged trie per call and would reintroduce the O(N²) burst
`bind_bound`'s comment records).

Same treatment for the `z` viewport family (`zz` / `z.` / `zt` / `z<CR>` /
`zb` / `z-`, the horizontal `zl` / `zh` / `zL` / `zH` / `zs` / `ze`, and the
fold toggles). This also picks up **`zf`**, which is bound in Normal only
despite being `action:create-fold-from-visual` — "create fold from the most
recent Visual selection", unreachable from Visual.

One lattice deviation worth recording: `<C-d>` / `<C-u>` are typed here as
`motion:line-down` / `-up` with a baked `Count(10)`, where vim treats them as
scrolling commands. VM.1's derivation therefore makes `d<C-d>` bound, which vim
leaves unbound. A superset, and harmless — noted so a future reader does not
read it as a bug.
