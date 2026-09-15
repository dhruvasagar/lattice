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

### VM.3 📝 — the commands that are motions in vim but actions here

Splits by what each needs from the grammar context. `GrammarEnv` is the
established seam for host state reaching the grammar (it already carries
`selection`, `indent`, `textwidth`, `indent_resolver`, `native_format`), so
each family extends it and `MotionContext` borrows through.

- **VM.3a 📝 `%`** — needs nothing new; a pure text scan over `buffer` + `from`.
  The cheapest conversion and the proof the shape works.
- **VM.3b 📝 `;` / `,`** — needs the last `f`/`F`/`t`/`T` (`Editor::last_find`).
  Note the current `do_find_repeat` already synthesises a `CommandInvocation`
  against the find-char motion, so the body is nearly a motion already.
- **VM.3c 📝 `H` / `M` / `L`** — needs the viewport (`scroll`,
  `viewport_height`, and the fold-aware `line_forward_by_budget` walk).
  Linewise motions in vim.
- **VM.3d 📝 `n` / `N` / `*` / `#`** — needs the session search pattern +
  direction.
- **VM.3e 📝 `` `x `` / `'x`** — needs the mark table. `'x` is linewise.
- **VM.3f 📝 `gj` / `gk` / `g0` / `g$`** — needs display-line layout, which is
  renderer-side; the hardest of the set and the one most likely to stay an
  action.

**Not in VM.3, deliberately:** `<C-f>` / `<C-b>` / `<C-e>` / `<C-y>` are
*scrolling* commands in vim, not motions — `d<C-f>` is not a valid operator
target there, and `<C-e>` / `<C-y>` do not move the cursor at all unless it
would leave the window. They still need to be reachable in Visual and Select
(vim allows them there), so they get an explicit mirror rather than a re-type.
That is **VM.3g 📝**.

One lattice deviation worth recording: `<C-d>` / `<C-u>` are typed here as
`motion:line-down` / `-up` with a baked `Count(10)`, where vim treats them as
scrolling commands. VM.1's derivation therefore makes `d<C-d>` bound, which vim
leaves unbound. A superset, and harmless — noted so a future reader does not
read it as a bug.
