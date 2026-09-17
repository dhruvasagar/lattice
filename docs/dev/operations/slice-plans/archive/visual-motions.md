# Slice plan: motions in Visual (VM)

Design: [keymap-architecture.md §15](../../../architecture/keymap-architecture.md).

**Archived 2026-09-17** after an audit confirmed every VM slice in code and
tests. The vim-parity differences this plan records as "not done" (the `>` /
`<` / `=` landing, `H` / `L` under `scrolloff`, window-local `scroll`, `g*` /
`g#`, the read-only wrap echo) and the unpinned `zfk` case are carried in
`implementation.md` § "Carried over from archived slice plans".

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

### VM.3d-1 ✅ — the current-match highlight follows the cursor

Decided 2026-09-15 (user). `current_match` drives the strong current-match
highlight and was set only by `/` / `n` / `N` / `*` / `#`, so it stayed on a
match after a `j` or an edit moved the cursor off it. It's now derived after
every dispatch: `Editor::follow_current_match_to_cursor`, called at the end of
`dispatch_chord_with_outcome` right after `write_through_caret` and before the
publish, binary-searches the already-resolved `all_matches` for the match
containing the cursor. That's the Neovim `CurSearch` convention.

`:nohlsearch` empties `all_matches`, so it can't resurrect the highlight; the
`/`·`?` live preview owns both fields while that line is open. Renderers read
`current_match` from render state as before, so neither changed. Terminal
panes mirror search hits into their grid separately, so they keep the
old behaviour until VM.3d-2 routes `n` / `N` there through the same motion.

This is the first half of VM.3d on its own because it's UX-visible and
independent of making `n` a motion.

### VM.3d-2 ✅ — `n` / `N` / `*` / `#` are motions

Decided 2026-09-15 (user): motion feedback lives in the grammar. Checked in vim
9.2 (`vimcheck_n.vim`): `dn` from 1,1 deletes `alpha ` (charwise, exclusive),
`dn` across a wrap deletes back to the wrapped match, `yN` yanks back to the
previous match, `vny` includes the match start, `d*` deletes to the next
occurrence of the word, and a `dn` that finds nothing deletes nothing and says
`E486`.

**Motions.** `motion:search-next` / `-prev` / `-word-forward` /
`-word-backward` are exclusive jumps. `LastSearch` moved into
`lattice-grammar` (host re-export, no call site moved) and reaches the motion
through `GrammarEnv` → `MotionContext` (owned in `DispatchEnv` across the
actor, forwarded by the multibuffer), like `last_find`. The grammar compiles
the pattern with `fancy-regex` and searches with `lattice_core::search`.

**Feedback.** `MotionResult::notice` (a `Copy` enum) carries the wrap, which
the dispatcher echoes alongside the motion's effect, on the bare and operator
paths alike. `CommandError::User` carries `E486` / `E35`: no effect is
committed, so an operator fed by a failed search does nothing, and the host
echoes the message instead of dropping the error.

**`*` / `#`.** The word becomes the search before the motion runs:
`Editor::capture_search_word`, called by both motion runners for a bare `*`
and for an operator targeting it, records `last_search` and resolves
`all_matches`; the motions then repeat that search. One word rule
(`word_at_or_after_cursor`) serves this and the WIT action path.

**Jumps** are recorded only after the motion succeeds, so a failed `n` leaves
none. **Terminal panes** route the four ids to the host's search methods, which
mirror hits into the grid. A read-only buffer (`:help`) gets the search in its
motion env; its wrap echo is not shown (that runner returns a position only).

### VM.3d-3 ✅ — `*` / `#` match whole words

vim's `*` searches `\<word\>` (whole-word, when the word is keyword
characters); lattice's had always searched the plain escaped word. A vim-parity
fix of its own, not part of making the keys motions.

**`\<` / `\>` are vim regex and lattice searches with `fancy_regex`**, so the
landed form is `\b{escaped}\b`. That is an exact translation rather than an
approximation here: `word_at_or_after_cursor` only ever returns a run of
keyword characters, so both ends always sit on a word/non-word edge — which is
what `\b` asserts. The escape stays, as a no-op that keeps the helper correct
if the word source ever widens.

Measured in vim 9.2 (`vimcheck_star.vim`), and two rows were not in this
plan's original description:

```text
*  on `foo`      -> \<foo\>     #  on `foo` -> \<foo\>   (SAME pattern; only the direction differs)
g* on `foo`      -> foo         g# on `foo` -> foo       (no boundaries)
*  on `+++ x`    -> \<x\>       (skips non-keyword text to the next KEYWORD word on the line)
*  on `foo_bar2` -> \<foo_bar2\>
```

The third row is the one worth keeping: vim does not search for punctuation.
It looks for a keyword word at or after the cursor and only falls back to a
non-keyword run when the rest of the line has none — behaviour
`word_at_or_after_cursor` already had, which is why this slice was a
two-call-site change.

`g*` / `g#` are unbound; when they land they take the same word and skip the
`\b`s.

**"existing tests pin that pattern" turned out to be wrong** — no test asserted
the bare word. The one that looked like it (`search_state_is_session_wide.rs`)
drives `execute_search`, a `/` search, which this does not touch.

### VM.3e ✅ — `` `x `` / `'x `` are motions

Checked in vim 9.2 (`vimcheck_marks.vim`, `vimcheck_marks2.vim`): `d'a`
deletes whole lines to the mark, `` d`a `` stops before it (charwise,
exclusive), `y'a` works backward, `v'a` extends a charwise selection to the
first non-blank of the mark's line, `c'a` is a linewise change, a count is
ignored, and an unset mark is `E20` with nothing changed and no jump.

`motion:mark-line` (linewise) and `motion:mark-exact` (exclusive) jump and take
the mark name as `Args::Char`, bound by `register_mark_paths` beside `f` / `t`
for bare and operator forms; Visual gets them from the mirror. VM.3L's linewise
targets were the blocker and are in. The table reaches the grammar as a
`MarkResolver` (`GrammarEnv` → `MotionContext`, an `Arc` clone in `DispatchEnv`
only when a mark is set, forwarded by the multibuffer); the host's
`HashMap<char, Position>` is its own resolver, so a read-only buffer borrows
it. A mark past the end of a shrunk buffer clamps. Terminal panes route both
ids to `do_jump_mark`, which mirrors the jump into the grid. The action ids
stay registered for WIT.

### VM.3m ✅ — where an operator leaves the cursor

Found while gating VM.3L; older than it. Checked in vim 9.2
(`vimcheck_opcursor.vim`, buffer
`['  one a', '    two b', '', '  four d', '  five e', '  six f']`):

| Command | From | vim cursor |
|---|---|---|
| `dd` | 1,6 | 1,5 — first non-blank of the line that moved up |
| `dk` | 5,5 | 4,3 — first non-blank |
| `2dj` | 1,6 | 1,3 — first non-blank |
| `yy` / `2yy` / `yj` | 1,6 / 2,8 / 1,6 | unchanged |
| `yk` | 2,8 | 1,7 — start line, column kept (clamped to the line) |
| `yb` | 2,8 | 2,5 — start of the yanked text |
| `y{` | 5,5 | 3,1 |
| `yip` | 5,5 | 4,1 — start of the object |

Lattice landed a linewise delete on column 0 (the edit's start) and no yank
moved the cursor at all. The rule is vim's "the cursor is left at the start of
the text operated upon", with a linewise delete then going to the first
non-blank.

`OperatorContext::origin` carries the operated region's start BEFORE linewise
expansion — `min(cursor, motion target)` for a motion, the object's or
selection's start otherwise, and the cursor for a count / current-line / ex
range. That distinction is the whole point: `yk` keeps its column because `k`'s
target does, `yy` doesn't move at all, and `yip` goes to its object's line
start, though all three expand to whole lines. `resolve_target` returns the
pre-expansion target (one caller), so nothing else had to change.
`operator_yank` appends the cursor move; `operator_delete` appends one only
when linewise. Not done: `>` / `<` / `=` also go to the first non-blank in vim.

### VM.3f ✅ — `H` / `M` / `L` are motions, and `startofline`

Decided 2026-09-15 (user): a typed `startofline` option, **default on** as in
vim 9.2 — `H` / `M` / `L` land on the first non-blank; off keeps the column.

Checked in vim 9.2 on a real screen (`vimcheck_hml4.vim`,
`vimcheck_m_even.vim`): `3H` / `3L` count from the edges and a count past the
window clamps to the far one; `M` is top + (lines shown − 1) / 2, so a short
buffer's middle; `dL` / `yH` are linewise, `vL` charwise.

`motion:viewport-top` / `-middle` / `-bottom` are linewise jumps. The layout
stays on the host: `Editor::shown_lines` walks the window's lines and their
heights (the IM.1b budget walk), built only when the invocation is `H` / `M` /
`L`, and hands the grammar a `ViewportResolver`. The grammar's `ShownLines`
owns vim's rule over those lines, and the host's `JumpViewport` action path
answers from the same type, so the two can't disagree. `nostartofline` rides
`GrammarEnv` (named so `Default` is vim's default). Fixed on the way: `M` used
to spend `height / 2`, one line past vim on every even height, and ignored
short buffers; the IM.1b test pinned that and now pins vim's number.

Not done: vim adjusts `H` / `L` for `scrolloff` (lattice's default is 0).

### VM.3j-1 ✅ — `gg` / `G` / `<C-f>` / `<C-b>` follow `startofline`

Split from VM.3j on 2026-09-16 (user): the column rule costs nothing, while
`<C-d>` / `<C-u>` need a WIT-bearing change (VM.3j-2), and the two do not belong
in one commit.

vim 9.2 (`vimcheck_sol_pages.vim`, 80 lines of `"  line N"`): with
`startofline` — the default — `gg` → 1,3, `G` → 40,3, `5G` → 5,3, `<C-f>` from
3,6 → 21,3, `<C-b>` from 60,6 → 51,3; with `nostartofline` the same lines at
column 6. Lattice landed `gg` / `G` at column 0, matching neither.

One rule, two peers: `startofline_target` in the grammar (`gg` / `G` / `H` /
`M` / `L`) and `Editor::startofline_byte` in the host (the page scrolls and the
`JumpViewport` action). `H` / `M` / `L` had an inline copy from VM.3f and now
call the helper. Only the COLUMN changes; the line each command walks to is
untouched.

### VM.3j-2 ✅ — `<C-d>` / `<C-u>` are half-window scrolls, and `scroll`

`<C-d>` / `<C-u>` are bound to the `j` / `k` MOTIONS with a baked `Count(10)`,
so they move a fixed ten lines regardless of window size, and — because a
motion composes — `d<C-d>` deletes eleven lines where vim deletes nothing.
They also cannot honour `startofline` while they are `j` / `k`, since plain `j`
must keep its column.

vim 9.2 (`vimcheck_ctrl_d.vim`): `<C-d>` scrolls the view and the cursor
together by half a window; `{count}<C-d>` sets the `scroll` option and persists
(`3<C-d>` then `<C-d>` moves 3 again); `d<C-d>` and `y<C-d>` do nothing;
`v<C-d>` extends the selection; on the last line it beeps.

So they become scroll COMMANDS like `<C-f>` / `<C-b>` (Normal, Visual and
Select; no operator rows), with a new `scroll` option (`#[aliases("scr")]`,
default 0 = half the window). That needs a new `AppEffect` variant, which means
`wit/types.wit`, both directions of `boundary_app_effect.rs` plus its
round-trip list, and rebuilding the 30 guest components (27 fixture crates +
3 in `plugins/`). Those do get verified: `lattice-plugin-host`'s `build.rs`
builds every fixture guest as its own standalone workspace, `wasm32-wasip2` is
installed, and the gate's plugin-host run prints no SKIP lines — so the cost is
rebuild TIME on a slice that already takes the longest gate in the workspace,
not a hole in the checking.

**Landed without one measured row**, which VM.3j-3 carries.

### VM.3j-3 ✅ — `{count}<C-d>` sets `scroll` and persists

`do_half_page` took no count and `Action::HalfPageDown` carried none, so
`3<C-d>` moved half a window and a following bare `<C-d>` had nothing to
remember. vim treats the count as an assignment to the `scroll` OPTION.

Measured in vim 9.2 (`vimcheck_scroll_curswant.vim`), 23-row window:

```text
default                &scroll=11          (half of 23)
3<C-d>                 &scroll=3
then bare <C-d>        cursor +3, top +3   (persists)
2<C-u>                 &scroll=2           (<C-u> writes the SAME option)
99<C-d>                &scroll=23          (CLAMPED to the window height)
:split  (height 11)    &scroll=5           (reset, half the new window)
:only   (height 23)    &scroll=11          (reset again)
```

Three rows were not in this plan's original description, and two of them cost
real code: `<C-u>` writes the option too; a count is clamped to the window
height rather than scrolling that far; and **`scroll` is reset whenever the
window is resized**, so a counted value never outlives the geometry it was
typed in. The last one is why this could not be "thread a count through" — it
needs a hook on `Action::SetViewportHeight`.

**No WIT change, unlike VM.3j-2.** The count comes from `Editor::pending_count`,
the slot the `<C-w>` pane-resize arms already consume, so `AppEffect` stays a
unit variant and nothing crosses the boundary.

The reset stores `0` rather than `height / 2`: `0` is already this option's
"half the window, computed at use" sentinel, so storing it stays correct through
later resizes instead of freezing a number that was right once. The only
user-visible difference from vim is what `:set scroll?` reports — `0` here, the
concrete half there.

**Known divergence: vim's `scroll` is window-local, lattice's is not.** vim
keeps a separate value per window, so `3<C-d>` in one split leaves the other
alone; lattice has no window-local option mechanism, so the write is global and
both splits move by three. Narrow (it needs a count typed with a split open),
recorded rather than silently accepted, and it wants a window-local option
substrate rather than a patch here.

### VM.3g-1 ✅ — `j` and `k` remember the column they aim for

vim's `curswant`. `CurswantEffect` on `MotionSpec` (`SetFromTarget` by default,
so every other motion and every plugin motion is already right); the host reads
it back through `CommandRegistry::motion_curswant`, so nothing new crosses
`Effect` or WIT. `Editor::curswant` holds it, and the rule at the end of every
dispatch is vim's: unless this dispatch was `j` / `k` / `$`, the column the
cursor ended on becomes the goal — which covers edits, Insert and yanks without
each knowing the rule exists.

### VM.3g-2 ✅ — `gj` / `gk` / `g0` / `g$` are motions

Ported from `Editor::do_display_line_*` so the landing is unchanged, with two
gains: a count walks (vim's `2gj` moves two rows; the actions ignored counts),
and there is now ONE goal column. `Editor::goal_col` is gone — it was
maintained by an allow-list ("reset unless the action is DisplayLineDown/Up"),
which is the shape that breaks silently when someone adds a vertical motion and
forgets the list.

The seam is a `DisplayResolver` (wrap width + rows per line) built only for
those four motions, over a window of `count + 1` lines either side of the
cursor — never the whole buffer, which matters on the keystroke path.

**Measured, not assumed** (`vimcheck_gdollar.vim`, `vimcheck_gj_curswant.vim`):
`g$` does NOT pin the goal the way `$` does — it records the landing column
(160), while `$` records MAXCOL. And `gj` / `gk` SET the goal from where they
land (`gj` from 2,5 records 85; crossing to the next line records the column
there), unlike `j`, which keeps its goal across a short line. A first
implementation had `gj` keeping the goal, which matches vim for a single `gj`
and diverges the moment a plain `j` follows.

### VM.3g-3 ✅ — `gj`'s goal when the aim is CLAMPED

One measured row was unmet: `g$` then `gj` on a 200-column line lands at 200 but
vim records `curswant = 240` — the UNCLAMPED aim, not the landing. So a
following `j` onto a longer line reached 240 in vim and 200 here. Narrow — it
needs a clamped `gj` followed by a vertical motion onto a longer line — but a
real divergence, and written down rather than rounded off.

Re-measured on the way in (`vimcheck_scroll_curswant.vim`, wrap 80 over a
240-char line then a 100-char line), which shows the whole rule rather than the
one row:

```text
g$  (row 1 of the 240 line)     col=80   curswant=80
gj  -> row 2                    col=160  curswant=160
gj  -> row 3                    col=240  curswant=240
gj  -> line 2 row 1             col=80   curswant=80
gj  -> line 2 row 2 (CLAMPED)   col=100  curswant=160   <- the aim
$   on the 240 line             col=240  curswant=2147483647 (MAXCOL)
```

`gj` / `gk` record **the column they aimed at**. Unclamped, the aim IS the
landing — which is exactly why VM.3g-2's simpler "set from where you landed"
rule passed every test it had.

**This plan's stated mechanism was wrong, and the correction is the slice.**
It said to mirror `MotionResult::exclusive`. `exclusive` is consumed INSIDE the
grammar dispatcher during range resolution and never leaves it; `curswant` has
to reach the host. The host's normal-document path runs through
`DispatchEnv` → `Document::dispatch_with_cancel`, which returns
`Pending<Effect>` — and `DispatchEnv` deliberately has **no lifetime**, because
it owns `Arc` handles so it can cross that trait into an async future. A
borrowed reporting slot cannot ride it. (The `GrammarEnv` path where a borrow
*would* work, `run_read_only_motion`, serves read-only buffers only.)

So the landed shape is a reporting slot, owned where it must cross the async
boundary and borrowed where it need not:

```text
motion   MotionResult { curswant: Some(Col(aim)) }      // only gj / gk
   |     GrammarEnv::curswant_out: Option<&Mutex<..>>   // Copy preserved
   |     DispatchEnv::curswant_out: Option<Arc<Mutex<..>>>  // owned, crosses async
host     Editor::curswant_report, take()n by the dispatch tail
```

`Editor::curswant_report` is a long-lived slot rather than a per-dispatch one
because `dispatch_blocking` takes `&self` — it can clone the `Arc` in but
cannot hand anything back. The tail `take()`s rather than reads: a motion
writes the slot on every motion dispatch (`None` for all but two), but an
operator or action never touches it, so a left-behind value would let one
`gj`'s aim resurface after an unrelated command.

The override is applied AFTER the `CurswantEffect` match, on purpose: `gj`'s
spec says `SetFromTarget`, whose whole meaning is "let the dispatch tail take
the goal from where the cursor landed" — the clamped column this slice exists
to discard. Claiming it is what suppresses that tail rule.

Not on the WIT boundary, like `exclusive` and `notice`: a plugin declares its
`CurswantEffect` on its `MotionSpec`. `from_wit` decodes `None` deliberately,
and that line says so.

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

Known gap at the time, pinned rather than fixed: `zfk` from column 0 created
no fold, because `k` was exclusive and lattice had no linewise operator
targets. VM.3L later added those and made `j` / `k` linewise, which should
resolve it. No test pins `zfk` yet (carried to the ledger).

A deviation recorded here, since removed: `<C-d>` / `<C-u>` were typed as
`motion:line-down` / `-up` with a baked `Count(10)`, so `d<C-d>` was bound. VM.3j-2
made them the half-page scroll commands vim has, with no operator row.

Tests: `crates/lattice-host/tests/visual_fold_commands_match_vim.rs` (one per
vim row, Visual and Normal), `lattice-ui-tui`'s `folds.rs` (`zf` as an operator
and the scrolls, over real keystrokes), grammar tests for the operator's line
span, and a WIT round-trip for `CreateFold` with non-default values.

### VM.3i ✅ — `zj` / `zk` are motions

Vim's `zj` / `zk` move to the start of the next fold / the end of the previous
one and "can be used after an operator". They were host actions, so `dzj`,
`yzk` and `zj` in Visual were all unbound.

Checked in vim 9.2 (headless, folds on 4–6 and 9–10): both are charwise and
exclusive, land on column 1, repeat with a count, count a closed fold as one,
and leave the cursor where it is when there's no fold that way. `dzk` from
12,3 deletes `line 10\nline 11\nli`.

**Mechanism:** a `FoldResolver` seam, the `IndentResolver` pattern. Folds and
their visibility are host state, so the grammar asks the host for the next
edge (`GrammarEnv::fold_resolver` → `MotionContext`, owned as
`FoldResolverHandle` in `DispatchEnv` across the actor) and the motions
(`motion:goto-next-fold` / `-prev-fold`) walk the answers. The host snapshots
the fold spans only when the buffer has folds, and builds the fold index only
when `zj` / `zk` ask. `folds::visible_fold_edge` is the one edge rule, shared
with `do_goto_fold`, which stays for the WIT `goto-next-fold` / `goto-prev-fold`
effects. The multibuffer forwards the resolver like `last_find`. Not on WIT:
a plugin motion has no fold table to ask.

**Known gap, closed by VM.3L:** `dzj` from 1,3 in vim keeps line 3's newline,
by `:h exclusive-linewise`. When VM.3i landed, lattice's engine didn't
implement that rule for any motion; VM.3L added it, and `dzj` is now pinned
against vim's exact result.

UX note: `zj` with no fold ahead used to echo "no more folds"; as a motion it
is silent, which is what vim does.

### VM.3L ✅ — linewise operator targets, and `:h exclusive-linewise`

Decided 2026-09-15 (user): implement linewise operator targets before VM.3e
(`'x`) and VM.3f (`H` / `M` / `L`), which are linewise motions. A motion's
`linewise` flag never reached the operator, so `dj`, `yj`, `dG` and `dgg` all
acted charwise — a documented deviation from vim.

Checked in vim 9.2 (`vimcheck_linewise.vim`, `vimcheck_linewise_edges.vim`,
`vimcheck_marks.vim`) and pinned row by row in `motion_composition.rs`:
`dj` / `dk` / `dG` / `dgg` delete whole lines into a `V` register, `yj` yanks
them linewise, `2dj` and `5dj` count and clamp, `cj` replaces lines, `dj` on
the last line does nothing at all, and `yk` lands on the start line keeping
the column. Marks behave the same way (`d'a` linewise, `` d`a `` charwise) and
come with VM.3e.

**Mechanism.** `motion_to_range` returns `(range, linewise)`. A linewise
motion's range is whole lines, the shape `Range::CurrentLine` resolves to,
and `resolve_target` carries the flag into `OperatorContext::linewise`, so
`d` / `y` / `c` needed no change. `j` / `k` declare `linewise: true` and fail
with the new `CommandError::MotionFailed` at the buffer edge: with linewise
ranges, returning the cursor there would have made `dj` on the last line
delete it. The host already drops a failed dispatch silently, and counts are
cleared before dispatch, so a failed `j` is a plain no-op.

**`:h exclusive-linewise`**, both halves, in `motion_to_range` for every
exclusive motion: ending in column 1 of a later line after starting at or
before the first non-blank makes the motion linewise (`d}` from a line start);
otherwise the end moves to the end of the previous line (`d}` from mid-line,
`dzj`).

Behaviour changes, deliberate: `dj` / `dk` / `dG` / `dgg` / `yj` act on whole
lines; `d}` from a line start deletes lines; `d}` / `dzj` from mid-line keep
the last line's newline; `j` on the last line and `k` on the first fail
(silently, as before, since nothing visible moved).

