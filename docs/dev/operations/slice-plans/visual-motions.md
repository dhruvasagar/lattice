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

### VM.3c ✅ — `;` / `,`, and the exclusivity axis they forced

`FindKind` and `LastFind` moved down into `lattice-grammar` (the host
re-exports both, so no call site moved), and `last_find` now travels
`DispatchEnv` → `GrammarEnv` → `MotionContext`. `motion:find-repeat` and
`motion:find-repeat-reverse` delegate to the existing find-char bodies by
rebuilding the context with the remembered character in `args`, so there is
still exactly one implementation of "find a char on this line".

`do_find_repeat` survives for `AppEffect::FindRepeat` (the WIT boundary) and
delegates, like `do_match_bracket`.

**`MotionResult::exclusive: Option<bool>`.** `linewise` has always travelled
with the RESULT and `exclusive` with the SPEC, and nothing needed the
asymmetry resolved until a motion existed whose exclusivity is not knowable
until it runs. `;` is that motion: it repeats whatever `f` / `F` / `t` / `T`
came last, and vim gives it that motion's exclusivity — `f` and `t` are
inclusive, `F` and `T` are not. One flag on the `;` spec has to be wrong half
the time, and it was: `d,` after an `f` deleted one character too many.

`None` — every motion but these two — means "read the spec", so nothing else
changed. NOT on the WIT boundary: a plugin declares exclusivity on its
`MotionSpec`, which is right for any motion that knows its own answer, so
`from_wit` decodes `None` deliberately rather than for want of a field.

Two behaviour notes:

- **`,` no longer rewrites the memory.** The old action re-dispatched a
  *find-char* invocation, which re-captured `last_find`, so `,` flipped the
  remembered direction and a following `;` went backwards. Vim reverses the
  travel, not the memory. Pinned.
- **The multibuffer forwards `last_find`.** Its `dispatch_with_env` discards
  every other env field, and for `selection` / `syntax` / `indent_resolver`
  that is honest — they need a composed→source mapping that does not exist.
  `last_find` needs none: `;` searches the line in front of the user, and in a
  composed view that line IS the composed one. `dispatch_composed` was widened
  to carry it rather than let a new field silently kill `;` there.

### VM.4 ✅ — the guarantee: a motion is live in Visual by construction

Asked for directly: "any new motions that are registered by plugins / builtin
will be available for visual mode without any change."

As implemented in VM.1, that was a convention with two trigger points, not a
guarantee. `expand_grammar_rows` ran at boot and on `PluginLoaded`, and three
things got past it:

- **A re-pushed mode layer.** `push_layer` replaces a mode layer's tries
  wholesale (K.1.b), which dropped its derived Visual rows until the pass ran
  again.
- **`init.rs` and plugin `register-binding`.** Both bind through
  `try_bind_chord_string` at any time, and neither re-ran the pass.
- **A binder added to boot after the pass.** It was missed without any error.

The drift test only enumerated `KeymapLayer::Builtin`, so it would have caught
none of these.

**Mechanism** (design: keymap-architecture.md §15): mirror at the four
writes into a layer's per-mode tries in `lattice-keymap`, which are
`bind_bound`, `bind_modes`, `push_layer` and `unbind`. Every binding API
reaches one of them: `bind` and `try_bind` go through `bind_bound`, and
`try_bind_chord_string` goes through `try_bind`. The mirror asks
`KeymapHandle::set_command_registry`'s live handle whether a command is a
motion. It writes into the layer's own tries, so `:describe-key`, which-key
and the reverse cache all stay accurate. It tells its own rows apart by
`Arc` identity: a Visual or Select binding shares the Normal row's `Arc` only
when it is the mirror, and every explicit multi-mode write gets its own copy.

**What stays in the host:** the operator-pending expansion, which needs
`Builtins`, plus the text-object rows, since that pass also removes the Normal
row and that removal is host policy.

**Accepted tradeoff:** a user can't permanently remove a motion from Visual by
unbinding it there. The next write to its Normal path, a re-push, or a rescan
restores it. Shadowing it means binding something else in Visual, and an
explicit binding like that is never overwritten.

**Accepted UX cost (decision (A), 2026-09-15):** printable motions no longer
extend a selection from inside Select mode. That means `w`, `e`, `f{char}`,
`%`, `;`, `[[` and every other motion whose first key types a character; in
Select they overtype, which is what select-mode.md §1 and §4 always
specified. Arrows, Home/End, PageUp/PageDown and `<C-d>` / `<C-u>` still
extend, and `<C-g>` flips to Visual, where every motion works. The payoff is
that text typed over a snippet placeholder is never taken by a motion.

**Gate scope, and why it's narrow.** VM.4 changes what lands in every
layer's Visual and Select tries, so every test that inspects keymap contents
was checked before choosing the gates (2026-09-15):

- `binding_count()` sums every trie in every layer and mode, so mirror rows
  WOULD change an exact count. But every exact-count test (plugin-host
  `keymap_source.rs`, `keymap_host.rs`, `teardown.rs`; plugin-loader
  `keymap_drain.rs`, `init_config.rs`) builds a bare `KeymapHandle::new()`
  and never boots an `Editor`. With no command registry, the mirror never
  runs there. Production is still covered: the loader shares boot's handle,
  and the registry lives on the shared `Arc<KeymapRegistry>`.
- Magit's Visual-chord tests (`every_visual_chord_is_classified` and the
  tests next to it, `lib.rs`'s s/u/x pairing) and surround's Visual `S` read
  the modes' DECLARED `KeymapEntry` lists, not the registry. Magit registers
  no motions.
- `project_plugin_modes.rs` reads a mode layer's Normal trie, and VM.4 never
  writes to Normal.

So the gates are the three touched crates: `lattice-keymap`, `lattice-host`,
`lattice-ui-tui`. The plugin crates aren't skipped; their tests can't observe
the change.

Touches:

- `lattice-keymap`: `trie.rs` gets `KeymapTrie::get`; `registry.rs` gets the
  mirror, `overtypes_in_select`, `set_command_registry`, `layers()`, and 14
  registry tests plus a `get` test.
- `lattice-host`: `editor_boot.rs` sets the registry; `expand_grammar_rows`
  keeps the Visual branch for text objects only; `keymap_normal.rs` and
  `keymap_select.rs` test harnesses are updated;
  `a_motion_is_live_in_visual.rs` now walks every layer, gets a rewritten
  mode-layer test, and gains a new end-to-end re-push test. `keymap_select.rs`
  replaces `visual_and_select_share_every_motion`, which asserted the bug, and
  adds a sweep of every printable against the populated table.
  `do_select_overtype` gives `<CR>` its auto-indent.
- `lattice-ui-tui`: `input.rs`, the `keymap_visual.rs` harness, and the
  `production_keymap()` bench, plus the new
  `keymap_register_production_catalog` bench with and without the mirror.
  `completion.rs` types `foo`, `work` and `<CR>` over a snippet placeholder
  through real keystrokes.
- Docs: keymap-architecture.md §15 rewritten; a benchmarks.md row.

**`<CR>` and `<NL>` overtype too.** Vim's Select rule is "Printable
characters, <NL> and <CR> cause the selection to be deleted, and Vim enters
Insert mode". Lattice overtyped printables only, so `<CR>` over a snippet
placeholder did nothing. `overtypes_in_select` now admits `<CR>` and Ctrl-J
(how a terminal sends `<NL>`), and both type a newline. Because vim *types*
the key after entering Insert, the newline carries its auto-indent (IN.1).
`do_select_overtype` computes the indent from the line split at the
selection's edges, and lands newline and indent in the same replace edit, so
one `u` still undoes the overtype (select-mode.md §3).

### VM.5 ✅ — Select binds no bare printables

VM.4 removed the printable motions, which came from the mirror.
`register_select_bindings` still bound `o` (swap ends) and the `i` / `a`
text-object prefixes explicitly, so those three letters still couldn't start
text typed over a selection. VM.5 deletes the function and its boot call, so
Select has no binder of its own. It replaces the two tests that asserted those
bindings (`visual_and_select_share_swap_ends`,
`visual_and_select_share_text_objects`) with tests that `o`, `i` and `a`
overtype, drops the sweep's `NOT_YET` list so all of `' '..='~'` is checked,
and adds `aim` / `info` / `owl` to the real-keystroke snippet test.

Checked in vim 9.2 before landing (headless, `ve<C-g>` over `beta`): `o`, `iw`
and `aw` each replace the selection and enter Insert. UX cost: inside Select,
`o` and `iw` type; `<C-g>` flips to Visual for both, as in vim.

### VM.3d 📝 — `n` / `N` / `*` / `#`, and where `current_match` belongs

**Not the same shape as `%` and `;`, and worth reading before starting.**
`repeat_search` does far more than compute a position: it sets
`current_match` (which drives hlsearch painting), emits vim's wrap echoes
("search hit BOTTOM, continuing at TOP"), emits `E486: Pattern not found`, and
refreshes the terminal search mirror. A motion returns only a `Position`, so
converting `n` naively drops all of it silently.

The way out is that `current_match` should not be `n`'s job at all — it is
"the match the cursor is sitting on", so the host can recompute it after ANY
dispatch from `last_search` + cursor. That is kind-free, and strictly more
correct than today: `current_match` currently updates only on `n` / `N` / `/`
and goes stale after a `j` or an edit. It also touches hlsearch painting,
which is UX-visible, so it is the FIRST step of this slice rather than a
side effect of it.

The pattern itself reaches the grammar the same way `last_find` does.

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

### VM.3h ✅ — fold and scroll commands in Visual, matching vim

`<C-f>` / `<C-b>` / `<C-e>` / `<C-y>` and the `z` family are scrolling and
fold commands in vim, not motions, so the motion mirror can't reach them and
they were Normal-only. The first draft of this slice bound the cursor-only fold
handlers in Visual and called it acceptable; it wasn't vim. Every rule below
was then checked in vim 9.2, run headless (`-u NONE`, `foldmethod=manual`),
rather than read off the help text, which gets two of them wrong.

- **`zf` is an operator**, as in vim: `zf{motion}`, `zfip`, `zff{char}` and
  `{Visual}zf`. `operator:create-fold` emits a new `AppEffect::CreateFold`
  (and WIT `create-fold`, reusing `narrow-lines-payload`). The old
  `action:create-fold-from-visual` required Visual but was bound in Normal
  only, so it could never succeed; it stays registered for the WIT boundary.
  `register_operator_bindings`' doubled form is now optional: `zff` would
  have shadowed `zff{char}`. The host exits Visual after a fold itself,
  because it leaves Visual after an operator only when the effect edits or
  yanks. Whole lines come from `lattice_grammar::range::span_to_whole_lines`,
  moved out of the narrow provider so `zn` and `zf` share it.
- **Visual `zo` `zc` `zd` act on every selected line** and end Visual. `zo`
  opens one level (inner fold stays closed); `zc` closes the innermost.
- **`zO` `zC` `zD` are new** (Normal and Visual), and not symmetric:
  - `zO`: folds containing the target lines plus every fold nested inside them
    (the help says nested-but-not-containing folds are unchanged; vim opens
    them);
  - `zC`: only folds containing the target lines, so in Visual it closes an
    enclosing, partly selected fold;
  - `zD`: Normal deletes the innermost fold plus its nested folds; Visual
    deletes folds inside the selection and keeps an enclosing one.
- **Visual `za` acts at the cursor and keeps Visual** (vim checked).
- **The rest of the `z` family** (scrolls, `zR`/`zM`/`zi`, `zj`/`zk`, and the
  org-cycle `z<Space>` / `z<Tab>` / `zp`) works in Visual at the cursor. None of
  it is bound in Select, where `z` is typed text.
- **`<C-f>` `<C-b>` `<C-e>` `<C-y>`** scroll in Visual and Select; a Ctrl chord
  never overtypes.

Known gap, pinned rather than fixed: `zfk` from column 0 creates no fold.
`k` is exclusive and lattice has no linewise operator targets yet, so the span
ends at byte 0 of the cursor's own line; `dk` and narrow's `znk` share the
limitation.

One lattice deviation worth recording: `<C-d>` / `<C-u>` are typed here as
`motion:line-down` / `-up` with a baked `Count(10)`, where vim treats them as
scrolling commands, so `d<C-d>` is bound here and not in vim. A superset, and
harmless.

Tests: `crates/lattice-host/tests/visual_fold_commands_match_vim.rs` (one per
vim row, Visual and Normal), `lattice-ui-tui`'s `folds.rs` (`zf` as an operator
and the scrolls, over real keystrokes), grammar tests for the operator's line
span, and a WIT round-trip for `CreateFold` with non-default values.

### VM.3i 📝 — `zj` / `zk` are motions

Vim's `zj` / `zk` move to the start of the next fold / the end of the previous
one and compose with an operator (`dzj`). They're typed as actions here, so
they're dead in Visual and after an operator. Re-type them as motions, like
`%` and `;` / `,`.
