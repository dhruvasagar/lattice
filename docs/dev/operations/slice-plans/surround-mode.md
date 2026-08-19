# surround-mode — slice plan

> **Status: 🚧 ACTIVE (2026-08-19).** SU.1–SU.3g complete (49 tests).
> SU.5–SU.6 deferred to v2.
> Sequencing companion to the design fragment
> [`../../architecture/surround-mode.md`](../../architecture/surround-mode.md).
>
> **SU.4 is done, not deferred** (corrected 2026-08-19). The ⛔ below stood
> while `editor_boot.rs` was already calling `register_operator_bindings`
> with `post_motion_char: true`, and `ysiw"` works — verified in source and
> pinned by `ysiw_wraps_the_word`. An icon is not evidence.

Native minor mode in `lattice-mode` (SL.0). Three operators registered in
`CommandRegistry` at boot; keymap at `MinorMode(surround-mode)`; one infrastructure
change (multi-char capture in `action_from_bound_with_capture`). Land each slice
green; ship doc + bench + test + graceful-error together.

## Slice SU.1 — Pair detection + surround operators

- **SU.1a — `open_close_pair` mapping.** ✅
  - Function in `lattice-mode/src/modes/surround.rs`: `fn open_close_pair(ch: char) -> Option<(char, char)>`
  - Maps bracket/quotes to canonical (open, close) form.
  - Unit tests for every bracket type + symmetric pairs.

- **SU.1b — `find_surround_pair` algorithm.** ✅
  - `fn find_surround_pair(document: &Buffer, cursor: Position, target: char) -> Option<(usize, usize)>`
  - Backward scan with closer-stack; forward scan with opener-stack.
  - Returns byte-offset pair or `None`.
  - Unit tests: nested `((x))` → inner; `"hello"` → bounds; cursor between `()`; no match.
  - Bench test: 10k-char line completes in <1ms.

- **SU.1c — Register operators in `CommandRegistry`.** ✅
  - `pub fn register_surround_operators(registry: &mut CommandRegistry) -> SurroundOperators`
  - `SurroundOperators { delete: OperatorId, change: OperatorId, add: OperatorId }`
  - All three: `repeatable: true`, `blockwise_per_row: false`.
  - Operator closures call `find_surround_pair` / `open_close_pair`.
  - 11 operator dispatcher tests: happy-path `ds"`, `cs"'`, `S{char}`; no-match no-op; undo-batch.

## Slice SU.2 — Multi-char capture + keymap + `SurroundMode`

- **SU.2a — Multi-char capture in `action_from_bound_with_capture`.** ✅
  - `crates/lattice-host/src/keymap_normal.rs:2007`: extended to handle `captured.len() > 1`.
  - When len > 1, produces `Args::List([ArgValue::Char(c) for c in captured])`.
  - Existing single-char callers unaffected (len == 1 path unchanged).

- **SU.2b — `SurroundMode` struct + `Mode` impl + keymap.** ✅
  - `pub struct SurroundMode` holds `SurroundOperators` for keymap construction.
  - `Mode::kind() = Minor`, `activation_policy() = Global`, `Guard = ()`.
  - `Mode::keymap()` returns chain-form bindings with `ChordPattern::CharLiteral`
    for wildcard character capture + table-form entries for catalog.

- **SU.2c — Declining bindings for `S` in Visual mode.** ✅
  - Bind `[S, CharLiteral]` in surround-mode Visual layer → surround-add on selection.
  - Surround mode's minor-mode layer takes precedence over builtin at lookup time.

## Slice SU.3 — Boot integration + tests + benchmarks

- **SU.3a — Boot integration.** ✅
  - `register_surround_operators(registry)` called from grammar bootstrap in `editor_boot.rs`.
  - `register_surround_modes(registry, operators)` called after `register_foundation_modes`.
  - `translate_mode_keymaps` picks up surround-mode's chain-form bindings at boot.

- **SU.3b — Integration tests.** ✅
  - 11 operator dispatcher tests in `crates/lattice-mode/src/modes/surround.rs`:
    - `ds"` on `"hello"` → `hello`
    - `ds(` on `fn foo(x: i32)` → `fn foox: i32`
    - `ds)` on `(hello world)` → `hello world`
    - `cs"'` on `"hello"` → `'hello'`
    - `cs(]` on `(hello)` → `[hello]`
    - `yss"` on `hello` → `"hello"`
    - `yss(` on `hello` → `( hello )` (was `(hello)` until SU.3g)
    - Visual `S"` on selection → wrap
    - No-op on unmatched `ds"` / `cs"'`
    - No-op on unknown wrapper char
  - Each test drives `grammar_execute()` through the real dispatcher.

- **SU.3c — Performance benchmarks.** ✅
  - Latency bench test: `find_surround_pair` on 10k-char line < 1ms.

- **SU.3d — Update implementation ledger.** ✅
  - Surround entry added to `docs/dev/operations/implementation.md`.

## Slice SU.3e — the chord path actually reaches the operators ✅

Everything above was verified through `grammar_execute()`. Nothing tested
that a *keypress* arrived, and three of the four chords did not. Three
defects, all shadowing at the trie level — a node's own binding is returned
before its children, so any binding at a shorter prefix makes the
wildcard-bearing path unreachable:

- `register_operator_bindings`' doubled-operator block ignored its own
  `post_motion_char` flag, binding `[y, s, s]` at the Builtin layer. Design
  fragment §3.3 step 3 says that must resolve `Partial`.
- surround-mode's own table-form catalog row `chord: "S"` bound `[S]` at one
  chord and shadowed `[S, CharLiteral]` — the mode shadowed itself. §3.2 says
  `[S]` must resolve `Partial`. The rows were believed inert; they are not.
  `push_mode_keymap` resolves entries into dispatch bindings in the same trie.
- the three Normal catalog rows wrote chords space-separated (`"d s"`), which
  `parse_chord_sequence` reads as a literal Space chord — binding
  `d<Space>s`, `c<Space>s`, `y<Space>s<Space>s`. They were the only
  space-separated `keymap_entry!` chords in the workspace.

All four table-form rows deleted. Nothing was lost: no production code reads
the static catalog, and `:describe-key` resolves against the trie, so the
chain bindings' `with_doc` strings are what it already displayed.

**Tests.** `surround_bindings.rs` drives real keys through
`test_helpers::press`: `yss`, `ds`, `cs`, visual `S`, `ysiw`, plus a guard
that surround-mode is active at all (else the suite is vacuous) and a pin
that `d<Space>s` is unbound.

## Slice SU.3f — cursor on the delimiter ✅

The two scans are half-open around the cursor (backward `byte < cursor`,
forward `byte >= cursor`), so a *closer* under the cursor was already found
by the forward scan while an *opener* under it was skipped by both — `ds"`
with the caret on the opening quote did nothing, where vim deletes the pair.
The effective cursor is now nudged one character right when it sits on an
opener, which puts it just inside its own pair: the position the scans
already handle.

A symmetric target is its own closer, so which end the cursor is on is
decided by the count of that character preceding it **on the line** (even ⇒
opens). Design fragment §4.2 records the rule and why it is not optional:
without it, `"a" "b"` with the caret on the second pair's opening quote
resolves to the gap between the pairs, which is a real enclosing pair and
the wrong one.

**Tests.** Eight finder-level cases (opener/closer × quote/bracket, the two
`"a" "b"` parity cases, an inner delimiter in a nested pair, and a lone
unmatched delimiter that must still find nothing) plus `ds` / `cs` at the
chord level, since landing on a quote and typing `ds"` is the shape a user
actually produces.

## Slice SU.3g — `(` vs `)` spacing ✅

vim-surround distinguishes the opening form from the closing one: `ysiw(`
yields `( hello )`, `ysiw)` yields `(hello)` (likewise `[`/`]`, `{`/`}`,
`<`/`>`). `open_close_pair` canonicalises both halves to the same pair,
which is right for *matching* a target and wrong for *inserting* one, so
the distinction is now carried by a separate `pads_inside(ch)` predicate.

Two-sided, and that is the part worth stating: insertion pads (add's
wrapper, change's replacement) and removal absorbs (delete's target,
change's target). A one-sided rule would leave `ds(` unable to undo
`ysiw(`, and the padding would accumulate on every round trip. `ds)` still
deletes the delimiters alone, so `( hello )` → ` hello `.

Design fragment gains §4.4. Two existing tests changed expectation
deliberately — `surround_add_linewise_wraps_line_with_brackets` and
`surround_change_parens_to_brackets` both used the opening form and
therefore now pad.

**Tests.** All four bracket pairs on both sides of the rule, symmetric
wrappers unaffected, `cs` in both directions (padded → unpadded and back),
the `ds)`-leaves-the-padding pair to the `ds(`-takes-it case, and a
round-trip property over all four pairs. Plus `ysiw(` / `ysiw)` and the
`ysiw(` → `ds(` round trip at the chord level.

## Deferred to v2

- **SU.5 — HTML/XML tag targets** (`t` — `cst<div>`, `dst`, `ysiwt`). ⛔ Needs tag-scanning parser.
- **SU.6 — Config gate** (`surround.enabled` typed option + `:surround-mode` toggle). ⛔

## Cross-renderer note

Surround operators are renderer-agnostic (grammar layer). No renderer files touched.
No TUI/GPUI parity concern for this feature.
