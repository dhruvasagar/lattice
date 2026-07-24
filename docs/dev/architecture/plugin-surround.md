# surround-mode — vim-surround semantics as a native minor mode

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing is §12 and lives in
> [`../operations/slice-plans/plugin-surround.md`](../operations/slice-plans/plugin-surround.md).
> Companion fragments:
> [`design.md`](design.md) (§5.8.3 — bundled plugin candidates),
> [`plugin-auto-pair.md`](plugin-auto-pair.md) (the first bundled plugin, structural template).

## 1. Why

vim-surround (`tpope/vim-surround`) is the standard for surrounding-text operations —
`ds"`, `cs"'`, `ysiw(`, `S<h1>` in visual mode. It is one of the most-used vim
plugins across the ecosystem (100k+ stars, shipped in every vim distribution). The
design doc §5.8.3 lists "surround" as a bundled editing helper. Its purpose *here*
is to be a **native minor mode** in `lattice-mode` — not a WASM plugin (WASM is for
*extensions*; vim's default grammar stays native by design, CLAUDE.md §5.8.3).

The mode takes **complete ownership** of its grammar operators, keymap surface, and
handler bodies — no new `Editor::` methods, no new variants in the host's `Action`
enum. The mode contributes its operators to the shared `CommandRegistry` at boot and
its keymap at `KeymapLayer::MinorMode(surround-mode)`.

## 2. Grammar surface

Three operators registered in the shared `CommandRegistry` at boot:

| Operator | Name | Trigger | Args | Target / Range |
|----------|------|---------|------|----------------|
| **surround-delete** | `operator:surround-delete` | `ds{char}` | `Args::Char(target)` | None (cursor position) |
| **surround-change** | `operator:surround-change` | `cs{char1}{char2}` | `Args::List([Char(t), Char(r)])` | None (cursor position) |
| **surround-add** | `operator:surround-add` | `yss{char}` / `S{char}` | `Args::Char(wrapper)` | `Range::CurrentLine` (yss) or `Range::Selection` (visual S) |

All three are registered as `CommandKind::Operator` with `OperatorSpec`. They
participate in dot-repeat (`repeatable: true`). The operator `apply` closures
receive `&mut OperatorContext` with the resolved range + args.

For `surround-delete` and `surround-change`, the effective range is the cursor
position (empty range) — the operator reads the cursor, finds the surrounding pair
via the scan algorithm (§4), and produces edits covering the pair characters.

For `surround-add`, the range is the text to wrap. The wrapping character is
captured as `Args::Char(w)`.

## 3. Keymap surface

All bindings land at `KeymapLayer::MinorMode(surround-mode)`. Refer to
[`keymap-architecture.md`](keymap-architecture.md) for the layered keymap model.

### 3.1 Normal mode

| Path | Result |
|------|--------|
| `[d, s, CharLiteral]` | `Invoke(surround_delete, Args::Char(captured))` |
| `[c, s, CharLiteral, CharLiteral]` | `Invoke(surround_change, Args::List([Char(c1), Char(c2)]))` |
| `[y, s, s, CharLiteral]` | `Invoke(surround_add, Range::CurrentLine, Args::Char(captured))` |

### 3.2 Visual mode

| Path | Result |
|------|--------|
| `[S]` | `Partial` (overrides builtin `operator:change`; arms `[S, CharLiteral]`) |
| `[S, CharLiteral]` | `Invoke(surround_add, Range::Selection, Args::Char(captured))` |

### 3.3 How the multi-chord resolution works

The trie-based keymap resolves multi-key chords via `partial_chord` accumulation:

**`ds{char}`** (3 keystrokes):
1. `d` → trie returns `Bound(Invoke(absorb_operator_delete))` → `AbsorbOperatorPrefix(delete)` → `partial_chord = ['d']`
2. `s` → `lookup_normal_with_prefix(['d'], 's')` → `Partal` (intermediate node for `[d, s, CharLiteral]`) → `partial_chord = ['d', 's']`
3. `{char}` → `lookup_normal_with_prefix(['d', 's'], char)` → `Bound(Invoke(surround_delete, Args::Char(char)))`

**`cs{char1}{char2}`** (4 keystrokes):
1. `c` → `partial_chord = ['c']`
2. `s` → `Partal` → `partial_chord = ['c', 's']`
3. `{char1}` → wildcard captured, `Partal` (intermediate for `[c, s, CharLiteral, CharLiteral]`) → `partial_chord = ['c', 's', char1]`
4. `{char2}` → wildcard captured → `Bound(Invoke(surround_change, Args::List([Char(c1), Char(c2)])))`

**`yss{char}`** (4 keystrokes):
1. `y` → `partial_chord = ['y']`
2. `s` → `Partal` → `partial_chord = ['y', 's']`
3. `s` → `Partal` → `partial_chord = ['y', 's', 's']`
4. `{char}` → `Bound(Invoke(surround_add, Range::CurrentLine, Args::Char(char)))`

See `crates/lattice-host/src/keymap_normal.rs:1876` (`lookup_normal_with_prefix`)
and `crates/lattice-host/src/input.rs:534` (`compute_normal_action`) for the
partial-chord resolution flow.

## 4. Surround pair detection (`find_surround_pair`)

The core algorithm shared by `ds` and `cs`. Given a cursor position and a target
character, find the nearest enclosing pair bounds.

### 4.1 Pair mapping

Characters map to their matching pairs:

```
( ↔ )    [ ↔ ]    { ↔ }    < ↔ >
" ↔ "    ' ↔ '    ` ↔ `
```

The mapping is bidirectional: `(` → `)` and `)` → `(`. For symmetric pairs
(`"`, `'`, `` ` ``), the same character opens and closes.

### 4.2 Algorithm

Scan **outward** from the cursor in both directions simultaneously:

1. Scan **backward** from the cursor, tracking a stack of encountered closers.
   When the target's matching opener is found with an empty stack, record the
   position.
2. Scan **forward** from the cursor, tracking a stack of encountered openers.
   When the target's matching closer is found with an empty stack, record the
   position.

If both opener and closer are found, return `Some((opener_pos, closer_pos))`.

Special cases:
- **Cursor between pair**: If the cursor is between `(` and `)`, scanning
  backward finds `(`, scanning forward finds `)` — return the bounds.
- **Nested pairs**: Stacks handle nesting correctly — `((x))` with cursor on
  `x` → the innermost `()` pair is found.
- **No match**: Return `None`. The operator produces no effect (vim's "no
  surrounding" beep-equivalent is a silent no-op).
- **Symmetric pairs** (`"`, `'`): Treated as balanced pairs — consecutive
  same-char encounters alternate open/close semantics.

### 4.3 `open_close_pair(ch) → (open, close)`

Maps a user-provided character to its canonical (open, close) form:

```
( → ( , )      ) → ( , )      [ → [ , ]      ] → [ , ]
{ → { , }      } → { , }      < → < , >      > → < , >
" → " , "      ' → ' , '      ` → ` , `
```

Used by `surround-add` (the wrapping char determines what to insert) and
`surround-change` (the replacement char determines the new wrap).

## 5. Operator implementations

### 5.1 `operator:surround-delete` (`ds{char}`)

```
Input: OperatorContext { document, args: Args::Char(target) }
Output: Effect::Many([Edits(...), CursorMove(...)])

1. Get cursor position from document
2. Call find_surround_pair(document, cursor, target)
3. If None → return Effect::Nothing (no-op)
4. Compute edits: delete opener byte + closer byte
5. Adjust cursor position: if cursor was after opener, move back by 1
6. Return EditBatch with both deletions + cursor snap
```

### 5.2 `operator:surround-change` (`cs{char1}{char2}`)

```
Input: OperatorContext { document, args: Args::List([Char(target), Char(replacement)]) }
Output: Effect::Many([Edits(...), CursorMove(...)])

1. Get cursor position from document
2. Call find_surround_pair(document, cursor, target)
3. If None → return Effect::Nothing
4. Call open_close_pair(replacement) → (new_open, new_close)
5. Compute edits: replace opener byte with new_open, closer byte with new_close
6. Adjust cursor as needed
7. Return EditBatch
```

### 5.3 `operator:surround-add` (`yss{char}` + `S{char}`)

```
Input: OperatorContext { document, range, args: Args::Char(wrapper) }
Output: Effect::Many([Edits(...), EnterMode(Normal)])

1. Call open_close_pair(wrapper) → (open, close)
2. Get range text from document
3. Wrap: open + range_text + close
4. If range is CurrentLine (yss), wrap the whole line including the newline:
   open + line_content + close + "\n"
5. If range is Selection (visual S), wrap the selection:
   open + selection_text + close
6. Move cursor after the open char (inside the pair)
7. Return EditBatch
```

## 6. Multi-char capture infrastructure

Currently `action_from_bound_with_capture` (`keymap_normal.rs:2007`) only forwards
`captured.first()` as `Args::Char(c)`. For `surround-change`'s two-char binding
(`[c, s, CharLiteral, CharLiteral]` → captures two chars), the function must be
extended:

**Before:**
```rust
if let Some(&c) = captured.first() {
    inv = substitute_invocation_char_arg(inv, c);
}
```

**After:**
```rust
match captured.len() {
    0 => {}
    1 => {
        inv = substitute_invocation_char_arg(inv, captured[0]);
    }
    _ => {
        inv = inv.with_args(Args::List(
            captured.iter().map(|&c| ArgValue::Char(c)).collect(),
        ));
    }
}
```

This is a **one-function change** with no API surface impact — `captured` is
already a `&[char]` slice. The existing single-char callers (`f{char}`,
`m{char}`, `r{char}`, `q{char}`, `@{char}`, `"{char}`) are unaffected.

## 7. Mode lifecycle

### 7.1 Registration

```rust
pub struct SurroundMode;

impl SurroundMode {
    pub fn mode_id() -> ModeId { ModeId::new("surround-mode") }
}

impl Mode for SurroundMode {
    type Guard = ();
    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Minor }
    fn activation_policy(&self) -> ActivationPolicy { ActivationPolicy::Global }
    fn keymap(&self) -> Keymap { /* §3 bindings */ }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
```

A marker mode: `Guard = ()`, trivial `on_activate`. No event subscriptions, no
service handles, no buffer-local state. The mode's entire surface is its
operators (registered at boot) and its keymap (layered at
`MinorMode(surround-mode)`).

### 7.2 Boot sequence

```
editor_boot.rs:
  1. register_surround_operators(command_registry) → SurroundOperators { delete, change, add }
  2. register_surround_mode(mode_registry)         → registers SurroundMode
  3. translate_mode_keymaps(keymap_handle)         → picks up SurroundMode::keymap()
```

Operators must be registered before the mode's keymap is translated (keymap
entries reference `OperatorId`s by name).

### 7.3 Activation

`ActivationPolicy::Global` → auto-activates on every `BufferKind::Document`.
On deactivation (buffer close / mode toggle), the keymap layer is removed
automatically. No Guard resources to clean up.

No `:surround-mode` toggle is registered in v1 (the mode is always-on for
document buffers) — a v2 follow-up can add a typed-option gate
(`surround.enabled`).

## 8. Paramount-goal alignment

> **UX (higher court):** The mode's operators fire on the keystroke thread
> (grammar path, sync). Pair detection is a bounded scan (cursor-nearest pair,
> forward and backward) — O(line length), not O(file length). The edit is a
> single undo batch. Glyph latency stays within the one-frame budget.

> **Paramount goals:** protects #3 (extensible vim modal editing — surround
> IS vim grammar, first-class operators) and #2 (extensibility — the mode
> pattern validates that non-trivial grammar contributions land entirely
> mode-owned). Sacrifices nothing; the one infrastructure change
> (multi-char capture) is general, tiny, and idle on the hot path.

> **Heuristic #1 (long-term fit, on merit):** Native mode, not WASM. Surround
> IS vim grammar, and the default grammar stays native. No WASM overhead on the
> keystroke hot-path; no component packaging for built-in behavior.

> **Heuristic #2 (paramount, not other editors):** The operator model mirrors
> the design doc's grammar architecture (§5.2) — `CommandKind::Operator` with
> `OperatorContext` — and is motivated by the unified dispatch goal, not by
> "vim does it this way."

> **Heuristic #5 (four artefacts):** This design fragment, the slice plan,
> operator-level benchmarks (pair-find latency), and operator tests + edge-case
> tests ship together.

## 9. HTML/XML tag surround — deferred

vim-surround supports tag targets: `cst<div>` (change surrounding tag),
`dst` (delete surrounding tag), `ysiwt<div>` (wrap in tag). The `t` target
requires scanning for matching `<tag>` / `</tag>` pairs, which needs an XML/
HTML-aware parser or heuristic tag-name matching. Deferred to v2.

## 10. Full `ys{motion}{char}` — deferred to v2

The v1 design supports `yss{char}` (linewise) and `S{char}` (visual) but not
the full operator+motion form (`ysiw"` / `ys2w)` / `ys$}`). The obstacle is
the post-motion character capture: after the motion range is resolved,
the operator must capture one more character (the wrapper). This needs new
infrastructure — either a post-operator `AppEffect::WaitForSurroundChar` or a
two-phase operator that stores the range and re-invokes after the char arrives.

`register_operator_bindings` (the host-side cross-product that creates motion
bindings under an operator prefix) handles the motion-resolution half; the gap
is the second-char capture. v1 ships `yss{char}` (the most-common linewise form)
and `S{char}` (visual) as path-prefixed trie bindings that capture the wrapping
char inline.

## 11. Rejected alternatives

- **WASM plugin.** Rejected: surround is vim grammar, and the default grammar
  stays native. WASM overhead on every keystroke violates paramount goal #1
  (performance). The same rule that keeps built-in operators native applies
  here.
- **Action handlers instead of operators.** Rejected: operators participate in
  dot-repeat, register-based yank, count multiplication, and blockwise dispatching
  — action handlers don't. Treating surround as operators means `.ds(` re-deletes
  the last surround, `"ads"` yanks the deleted surround into register `a`, and
  `2ds"` deletes two levels of double quotes.
- **Separate crate** (`lattice-surround`). Rejected for v1: surround is a single
  file (~300 lines of operator + pair logic) with no crate-level dependencies
  beyond `lattice-grammar`. It sits in `lattice-mode/src/modes/surround.rs`
  alongside the other foundation modes. If it grows (v2 tag support, motion
  support), a crate split is the natural graduation path.

## 12. Slices

See [`../operations/slice-plans/plugin-surround.md`](../operations/slice-plans/plugin-surround.md).
Three slices:

- **SU.1** — Pair detection + surround operators
- **SU.2** — Multi-char capture + keymap + `SurroundMode`
- **SU.3** — Boot integration + tests + benchmarks
