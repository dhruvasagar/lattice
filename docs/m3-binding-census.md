# M3 / Slice 8 -- `input.rs` binding census

Read-only inventory of every binding currently encoded in
`crates/lattice-ui-tui/src/input.rs` (4365 lines, of which ~1165 are
production code; the remainder are tests). Used to plan the
trie-driven dispatcher migration: every row below has to round-trip
through the new registry without loss.

Notation:

- Chord uses `keymap.rs` notation (`<C-d>`, `gg`, `<Esc>`, `dw`).
- "Pre-condition" lists `Pending::*` state, modifier-state, or
  context flags (`completion_open`, `recording_macro`,
  `picker_open`, `insert_completion_open`, `snippet_active`,
  `chord_capture`, `active_buffer`).
- "Resolves to" is the `Action` variant. For structurally
  interesting arms (operators with `Range::Selection`, `Target::Motion`,
  `Target::TextObject`, `Args::Char`) the shape is noted.
- "Source" is the `input.rs:line` of the arm.
- `(?)` marks a guess where the source comment was sparse.
- `(no descriptor)` flags an Action that has no row in
  `keymap.rs::default_keymap()` today.

## Summary

| Mode | Binding count | Notes |
|---|---|---|
| **Top-level dispatch** (`translate`) | 5 | Picker / completion-popup / snippet / chord-capture overlays + universal `<C-c>`; help-buffer-local intercepts |
| **Replace** (`translate_replace`) | 4 | Smallest mode -- start migration here |
| **Visual** (`translate_visual`) | 24 | Includes 2 blockwise-only (`I`, `A`); operators reuse Normal builtins with `Range::Selection` |
| **Search** (`translate_search`) | 4 | Trivial cmdline-style |
| **Command** (`translate_command`) | 14 | Plus a 4-binding completion-popup sub-layer |
| **Command chord-capture** (`translate_command_chord_capture`) | 4 (3 reserved + catch-all) | Overlay; every other key becomes a chord token via `format_chord` |
| **Picker overlay** (`translate_picker`) | 11 | Wins over modal handlers entirely; `<C-c>` dismisses picker not app |
| **Insert completion popup** (`translate_insert_completion_popup`) | 13 | Returns `Option<Action>`; non-claimed keys fall through to Insert |
| **Active snippet overlay** (`translate_active_snippet`) | 4 | `<Tab>`, `<S-Tab>`, `BackTab`, `<Esc>` only |
| **Insert** (`translate_insert`) | 8 + 2-key `<C-x>` family | Includes `<C-Space>`, `<C-x>`-pending, `<C-x><C-o>`, `<C-x><C-s>` |
| **Normal: top-level** (`translate_normal` direct match arms) | ~70 | Motions, viewport, ops, paste, `Y`/`x`/`D`/`C`/`S`, `J`, find-prefixes, marks, regs, macros, undo/dot, search keys, mode entries, `K` |
| **Normal: Ctrl-modified** | 14 | `<C-d/u/f/b/e/y/r/o/i/t/l/v/q/w>` |
| **Normal: count parser** | 1 (synthetic) | `1-9` always, `0` only if `pending_count > 0` -- guard, not a row in keymap |
| **After-`<C-w>`** (`resolve_after_ctrl_w`) | 11 + Esc | Bare and Ctrl-modified second key both accepted |
| **After-`g`** (`resolve_after_g`) | 12 | `gg`, case operators, `gv`, `gJ`, `g;`/`g,`, LSP go-tos |
| **After-`z`** (`resolve_after_z`) | 13 + Esc | Scroll positions + folds |
| **After-operator (op-pending)** (`resolve_after_operator`) | 27 + Esc | Motions / linewise doubled / find-prefixes / text-object prefixes |
| **After-text-object** (`resolve_after_text_object`) | 11 pairs (×2 for inner/around) + Esc | `iw`/`aw`, `iW`/`aW`, `ip/ap`, `is/as`, `it/at`, quotes, brackets, braces, angles |
| **After-find-char** (`resolve_after_find_char`) | 1 catch-all (any printable char) | + Esc; routes through 4 motion ids |
| **After-set-mark** (`resolve_after_set_mark`) | 1 catch-all (`a-zA-Z0-9`) | + Esc |
| **After-jump-mark line / exact** (`resolve_after_jump_mark`) | 1 catch-all (`a-zA-Z0-9`) | + Esc; `exact` is a bool |
| **After-register** (`resolve_after_register`) | ~5 ranges | Named, Numbered, Unnamed, BlackHole, System, with Esc fallback |
| **After-macro-start** (`resolve_after_macro_start`) | 1 catch-all (`a-zA-Z0-9`) | + Esc; `q` while recording stops via Normal direct arm, not here |
| **After-macro-play** (`resolve_after_macro_play`) | 2 (`@@` + `a-zA-Z0-9`) | + Esc |
| **Help-buffer local** (top-level `translate`) | 3 | `<Esc>`, `q`, `<CR>` -- only when `active_buffer` is `Help` or `FileTree` and `Pending::None` and Normal mode |

Approximate total of distinct bindings (excluding count-digit guard
and the chord-capture catch-all): **~280**.

---

## Top-level dispatch (`translate`, lines 81-169)

These run before the modal handlers and decide which overlay or mode
sees the key. They are not "bindings" in the keymap sense but every
slice has to model them.

| Order | Pre-condition | Action | Source |
|---|---|---|---|
| 1 | `picker_open` | Delegate to `translate_picker` | input.rs:87-89 |
| 2 | `insert_completion_open` (returns `Some`) | Delegate to `translate_insert_completion_popup` | input.rs:97-101 |
| 3 | `snippet_active && Insert && !insert_completion_open` (returns `Some`) | Delegate to `translate_active_snippet` | input.rs:111-117 |
| 4 | `Command && chord_capture` | Delegate to `translate_command_chord_capture` | input.rs:123-125 |
| 5 | `<C-c>` regardless of mode | `Action::Quit` | input.rs:128-130 |
| 6 | `Help`/`FileTree` + Normal + `Pending::None`: `Esc` | `Action::HelpDismiss` | input.rs:145 |
| 7 | same: `q` (only when `!recording_macro`) | `Action::HelpDismiss` | input.rs:146 |
| 8 | same: `<CR>` | `Action::FollowLink` | input.rs:147 |

### Flags

- `<C-c>` is a universal escape hatch -- only the chord-capture overlay
  preempts it, deliberately (so `:describe-key <C-c>` works).
- The help-buffer `q`-while-not-recording arm is the only place the
  `recording_macro` flag is consulted in top-level dispatch.
- `Help`/`FileTree` buffer-local bindings only fire on `Pending::None`;
  inside an `AfterG` or `AfterCtrlW` the chord falls through to
  `translate_normal` (intentional -- motions / `gg` / `<C-d>` work
  inside help).

---

## Replace mode (`translate_replace`, lines 171-182)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `EnterMode(Normal)` | input.rs:176 |
| `<BS>` | -- | `ReplaceUndoLast` | input.rs:177 |
| `<CR>` | -- | `Insert("\n")` | input.rs:178 |
| `<Char>` | no Ctrl | `OverwriteChar(c)` | input.rs:179 |

Any Ctrl-modified key is dropped (returns `Action::None`,
input.rs:172-174). The Ctrl-block is a guard, not a binding -- the
trie has to encode "any Ctrl-prefixed chord here is a no-op" rather
than each specific Ctrl-letter.

`<Char>` is a single catch-all that fires on every printable; the
trie node has to have a wildcard leaf, not 95 separate rows. Same
pattern in Insert / Search / Command.

---

## Visual mode (`translate_visual`, lines 184-253)

VisualKind passed in (`Charwise`/`Linewise`/`Blockwise`); first two
arms branch on it.

### Blockwise-only

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `I` | `kind == Blockwise` | `EnterBlockVisualInsert` | input.rs:194 |
| `A` | `kind == Blockwise` | `EnterBlockVisualAppend` | input.rs:195 |

### All visual kinds

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `ExitVisual` | input.rs:200 |
| `v` | -- | `ExitVisual` (toggle) | input.rs:202 |
| `V` | -- | `ExitVisual` (toggle) | input.rs:203 |
| `h` / `<Left>` | -- | `Invoke(char_left)` | input.rs:208 |
| `j` / `<Down>` | -- | `Invoke(line_down)` | input.rs:209 |
| `k` / `<Up>` | -- | `Invoke(line_up)` | input.rs:210 |
| `l` / `<Right>` | -- | `Invoke(char_right)` | input.rs:211 |
| `0` / `<Home>` | -- | `Invoke(line_start)` | input.rs:212 |
| `$` / `<End>` | -- | `Invoke(line_end)` | input.rs:213 |
| `^` | -- | `Invoke(first_non_blank)` | input.rs:214 |
| `w` | -- | `Invoke(word_forward)` | input.rs:215 |
| `b` | -- | `Invoke(word_backward)` | input.rs:216 |
| `e` | -- | `Invoke(word_end)` | input.rs:217 |
| `W` | -- | `Invoke(big_word_forward)` | input.rs:218 |
| `B` | -- | `Invoke(big_word_backward)` | input.rs:219 |
| `E` | -- | `Invoke(big_word_end)` | input.rs:220 |
| `}` | -- | `Invoke(paragraph_forward)` | input.rs:221 |
| `{` | -- | `Invoke(paragraph_backward)` | input.rs:222 |
| `)` | -- | `Invoke(sentence_forward)` | input.rs:223 |
| `(` | -- | `Invoke(sentence_backward)` | input.rs:224 |
| `G` | -- | `Invoke(goto_last_line)` | input.rs:225 |
| `d` / `x` | -- | `Invoke(delete).with_range(Selection)` | input.rs:229-231 |
| `c` / `s` | -- | `Invoke(change).with_range(Selection)` | input.rs:232-234 |
| `y` | -- | `Invoke(yank).with_range(Selection)` | input.rs:235-237 |
| `>` | -- | `Invoke(indent_right).with_range(Selection)` | input.rs:242-245 |
| `<` | -- | `Invoke(indent_left).with_range(Selection)` | input.rs:246-249 |

### Flags

- All Ctrl-modified keys are dropped (input.rs:185-187 guard).
- `kind` is a state input but not part of the chord -- only `I`/`A`
  are gated. The trie can model these as "Visual(Blockwise)"-mode
  bindings.
- Operators emit `Range::Selection` rather than a typed motion target.
  Distinct shape from Normal-mode operators -- migration must encode
  this on the operator descriptor, not on the key.
- `keymap.rs` is **missing descriptors for**: `I`, `A` (blockwise),
  `<Left>`/`<Down>`/`<Up>`/`<Right>`, `<Home>`/`<End>`, `W`/`B`/`E`,
  `}`/`{`/`)`/`(`, `>`/`<` in Visual. Only the headline motions and
  the four operators (`d`/`x`, `c`/`s`, `y`) are listed today.

---

## Search mode (`translate_search`, lines 255-265)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `SearchCancel` | input.rs:257 |
| `<CR>` | -- | `SearchSubmit` | input.rs:258 |
| `<BS>` | -- | `SearchBackspace` | input.rs:259 |
| `<Char>` | no Ctrl | `SearchAppend(c)` | input.rs:260-262 |

Ctrl-modified keys silently drop. No descriptors in `keymap.rs` for
the catch-all (which is fine -- it's a wildcard).

---

## Command mode (`translate_command`, lines 267-311)

Two sub-layers: completion-popup-claims (when `completion_open` is
true) and the regular cmdline grammar.

### Completion-popup-claims (when `completion_open == true`)

| Chord | Action | Source |
|---|---|---|
| `<Tab>` | `CommandLineCompleteOrAdvance` | input.rs:281 |
| `<S-Tab>` (BackTab) | `CommandLineCompletePrev` | input.rs:282 |
| `<CR>` | `CommandLineAcceptCompletion` | input.rs:283 |
| `<Esc>` | `CommandLineDismissCompletion` (two-stage Esc; second Esc cancels cmdline) | input.rs:284 |

If none of these match, the popup branch falls through to the
regular dispatch below.

### Ctrl-modified

| Chord | Action | Source |
|---|---|---|
| `<C-h>` | `CommandLineDescribeUnderCursor` | input.rs:291 |
| `<C-u>` | `CommandLineClear` | input.rs:292 |
| `<C-w>` | `CommandLineDeleteWordBackward` | input.rs:293 |
| any other Ctrl | `Action::None` | input.rs:294 |

### Regular cmdline keys

| Chord | Action | Source |
|---|---|---|
| `<Esc>` | `CommandLineCancel` | input.rs:299 |
| `<CR>` | `CommandLineSubmit` | input.rs:300 |
| `<BS>` | `CommandLineBackspace` | input.rs:301 |
| `<Tab>` | `CommandLineCompleteOrAdvance` | input.rs:302 |
| `<S-Tab>` (BackTab) | `CommandLineCompletePrev` | input.rs:303 |
| `<Up>` | `CommandLineHistoryPrev` | input.rs:304 |
| `<Down>` | `CommandLineHistoryNext` | input.rs:305 |
| `<Char>` | `CommandLineAppend(c)` | input.rs:306-308 |

### Flags

- `keymap.rs` lists `<S-Tab>` for cmdline indirectly via "advance to
  next candidate" -- the BackTab arm is **missing a descriptor**.
- `_chord_capture` is in the function signature but never read; it's
  routed at the top level. Migration should drop the unused param.
- Two-stage Esc and the popup-completion paths require the dispatcher
  to consult `completion_open` as a binding-mode discriminator, not
  just a context flag. Consider modelling "Command-with-popup" as a
  layered minor mode, similar to CompletionPopup for Insert.

---

## Command chord-capture overlay (`translate_command_chord_capture`, lines 316-334)

Active when `Command && chord_capture`. Reserves three keys; every
other event flows through `chord::format_chord` and lands as a token.

| Chord | Action | Source |
|---|---|---|
| `<Esc>` | `CommandLineCancel` | input.rs:323 |
| `<CR>` | `CommandLineSubmit` | input.rs:324 |
| `<BS>` | `CommandLineDeleteChord` (one full chord-token, not one byte) | input.rs:325 |
| any chord | `CommandLineAppendChord(token)` (or `None` if `format_chord` returned `None`) | input.rs:328-333 |

`keymap.rs` has no descriptors for any of these (none of the Actions
are listed there either).

---

## Picker overlay (`translate_picker`, lines 340-365)

Active when `picker_open`. Wins over every modal handler.

### Ctrl-modified

| Chord | Action | Source |
|---|---|---|
| `<C-c>` | `PickerDismiss` (escape-hatch override; not Quit) | input.rs:345 |
| `<C-n>` | `PickerSelectNext` | input.rs:346 |
| `<C-p>` | `PickerSelectPrev` | input.rs:347 |
| `<C-u>` | `PickerBackspace` (approximate; per-char today, comment flags this) | input.rs:350 |
| any other Ctrl | `Action::None` | input.rs:351 |

### Bare keys

| Chord | Action | Source |
|---|---|---|
| `<Esc>` | `PickerDismiss` | input.rs:355 |
| `<CR>` | `PickerAccept` | input.rs:356 |
| `<BS>` | `PickerBackspace` | input.rs:357 |
| `<Up>` | `PickerSelectPrev` | input.rs:358 |
| `<Down>` | `PickerSelectNext` | input.rs:359 |
| `<Tab>` | `PickerSelectNext` | input.rs:360 |
| `<S-Tab>` | `PickerSelectPrev` | input.rs:361 |
| `<Char>` | `PickerAppend(c)` | input.rs:362 |

`keymap.rs` has **no descriptors for any picker bindings**. The picker
overlay is documented in `DESIGN.md §5.9.7` but has no rows in the
catalog. Audit deliverable: every Action above is a "fires Action with
no descriptor" hit.

---

## Insert mode (`translate_insert`, lines 367-415)

Pending-state branch first; then one-key Ctrl bindings; then the
default match.

### Pending: `AfterCtrlX` (lines 372-387)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<C-o>` | Ctrl held | `CompletionTrigger` | input.rs:375 |
| `<C-s>` | Ctrl held | `SnippetExpand` | input.rs:380 |
| any other Ctrl | -- | `SetPending(None)` (drop the prefix) | input.rs:383 |
| any non-Ctrl | -- | `SetPending(None)` | input.rs:386 |

### One-key bindings

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<C-Space>` | -- | `CompletionTrigger` | input.rs:391-394 |
| `<C-x>` | -- | `SetPending(AfterCtrlX)` | input.rs:400-403 |

### Default match

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `EnterMode(Normal)` | input.rs:406 |
| `<BS>` | -- | `DeleteCharBackward` | input.rs:407 |
| `<CR>` | -- | `Insert("\n")` | input.rs:408 |
| `<Tab>` | -- | `Insert("\t")` | input.rs:409 |
| `<Char>` | no Ctrl | `Insert(c.to_string())` | input.rs:410-412 |

### Flags

- `keymap.rs` lists `<C-x><C-o>` and `<C-x><C-s>` as
  `BindingMode::AfterCtrlX` rows; the `<C-x>` prefix itself has a row.
- The "fall-through" arm at input.rs:386 (`<C-x>` followed by a
  non-Ctrl key drops pending and returns None) is not separately
  rowed in keymap; trivially modelled as the default trie-leaf.

---

## Insert completion-popup minor mode (`translate_insert_completion_popup`, lines 425-469)

Returns `Option<Action>`. Layer is consulted **before**
`translate_insert`; non-claimed keys (returning `None`) fall through.

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<C-n>` | Ctrl | `CompletionNext` | input.rs:429 |
| `<Down>` | -- | `CompletionNext` | input.rs:429 |
| `<C-p>` | Ctrl | `CompletionPrev` | input.rs:430 |
| `<Up>` | -- | `CompletionPrev` | input.rs:430 |
| `<C-y>` | Ctrl | `CompletionAccept` | input.rs:432 |
| `<Tab>` | -- | `CompletionAccept` | input.rs:433 |
| `<CR>` | -- | `CompletionAccept` | input.rs:434 |
| `<C-e>` | Ctrl | `CompletionCancel` | input.rs:436 |
| `<Esc>` | -- | `CompletionCancelAndExitInsert` | input.rs:438 |
| `<C-Space>` | Ctrl + Space | `CompletionTrigger` | input.rs:441 |
| `<C-d>` | Ctrl | `CompletionToggleDocs` | input.rs:443 |
| `<C-f>` | Ctrl | `CompletionDocsScrollDown` | input.rs:448 |
| `<C-b>` | Ctrl | `CompletionDocsScrollUp` | input.rs:449 |
| any printable `c` | not Ctrl, not control char | `CompletionAcceptThenInsert(c)` | input.rs:459-461 |

`keymap.rs` `mode: CompletionPopup` covers most; `<Down>` and `<Up>`
aliases for `<C-n>`/`<C-p>` are **missing descriptors**.

The `CompletionAcceptThenInsert(c)` catch-all is conceptually a
wildcard binding. The migration has to decide: leaf wildcard, or each
typed character routes back to Insert and the App layer tracks
commit-char state. Today it's a wildcard arm.

---

## Active-snippet minor mode (`translate_active_snippet`, lines 481-493)

Returns `Option<Action>`. Active when `snippet_active && Insert &&
!insert_completion_open`. Non-claims fall through to Insert.

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Tab>` | not Shift | `SnippetNextPlaceholder` | input.rs:483-485 |
| `<S-Tab>` | -- | `SnippetPrevPlaceholder` | input.rs:486 |
| `<Tab>` | Shift held | `SnippetPrevPlaceholder` | input.rs:487-489 |
| `<Esc>` | -- | `SnippetLeave` | input.rs:490 |

Note the duplicate `<Tab>` arm: the BackTab path (input.rs:486)
catches terminals that emit a real BackTab; the Shift+Tab path
(input.rs:487-489) catches terminals that emit `Tab` with the SHIFT
modifier. In a trie this collapses to "S-Tab equals BackTab equals
Tab+SHIFT".

---

## Normal mode (`translate_normal`, lines 495-732)

The big one. Pending-state branch first (delegates to nine resolvers
below); then Ctrl-modified; then count-digit guard; then a 70-arm
default match.

### Pending-state delegations (lines 503-525)

| Pre-condition | Delegate | Source |
|---|---|---|
| `Pending::AfterCtrlW` | `resolve_after_ctrl_w` | input.rs:504 |
| `Pending::AfterG` | `resolve_after_g` | input.rs:505 |
| `Pending::AfterOperator(op)` | `resolve_after_operator` | input.rs:506 |
| `Pending::AfterFindChar { kind, operator }` | `resolve_after_find_char` | input.rs:507 |
| `Pending::AfterTextObject { operator, around }` | `resolve_after_text_object` | input.rs:510 |
| `Pending::AfterZ` | `resolve_after_z` | input.rs:513 |
| `Pending::AfterSetMark` | `resolve_after_set_mark` | input.rs:514 |
| `Pending::AfterJumpMarkLine` | `resolve_after_jump_mark(.., false)` | input.rs:515 |
| `Pending::AfterJumpMarkExact` | `resolve_after_jump_mark(.., true)` | input.rs:516 |
| `Pending::AfterRegister` | `resolve_after_register` | input.rs:517 |
| `Pending::AfterMacroStart` | `resolve_after_macro_start` | input.rs:518 |
| `Pending::AfterMacroPlay` | `resolve_after_macro_play` | input.rs:519 |
| `Pending::AfterCtrlX` | `SetPending(None)` (Insert-only state; drop) | input.rs:524 |

### Ctrl-modified Normal (lines 528-565)

| Chord | Action | Source |
|---|---|---|
| `<C-d>` | `Invoke(line_down).with_count(10)` | input.rs:530 |
| `<C-u>` | `Invoke(line_up).with_count(10)` | input.rs:531 |
| `<C-f>` | `PageDown` | input.rs:532 |
| `<C-b>` | `PageUp` | input.rs:533 |
| `<C-e>` | `ScrollLineDown` | input.rs:534 |
| `<C-y>` | `ScrollLineUp` | input.rs:535 |
| `<C-r>` | `Redo` | input.rs:536 |
| `<C-o>` | `JumpHistoryBack` | input.rs:537 |
| `<C-i>` | `JumpHistoryForward` | input.rs:538 |
| `<C-t>` | `TagStackPop` | input.rs:544 |
| `<C-l>` | `RedrawScreen` | input.rs:550 |
| `<C-v>` | `EnterVisual(Blockwise)` | input.rs:558 |
| `<C-q>` | `EnterVisual(Blockwise)` (alias) | input.rs:558 |
| `<C-w>` | `SetPending(AfterCtrlW)` | input.rs:562 |
| any other Ctrl | `Action::None` | input.rs:563 |

### Bare `<Tab>` (lines 568-570)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Tab>` | empty modifiers | `JumpHistoryForward` (vim's `<C-i>` alias) | input.rs:568-569 |

### Count parser (lines 575-580)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<digit>` (0-9) | `digit > 0 \|\| pending_count > 0` | `PushDigit(d)` | input.rs:575-580 |

`0` only fires `PushDigit` if a count is in progress; otherwise it
falls through to `motion:line-start` below. **This is one of the
trickier dispatch features** -- the trie node for `0` has to know
the current `pending_count`.

### Default match (lines 582-731)

#### Macro keys

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `q` | `recording_macro` | `StopMacroRecord` | input.rs:585 |
| `q` | otherwise | `SetPending(AfterMacroStart)` | input.rs:586 |
| `@` | -- | `SetPending(AfterMacroPlay)` | input.rs:589 |

The `q` arm is **the only Normal-mode binding whose target depends on
recording state**. `keymap.rs` documents this in the `q` doc string
("press q again to stop") but doesn't model the state split.

#### Motions

| Chord | Action | Source |
|---|---|---|
| `h` / `<Left>` | `Invoke(char_left)` | input.rs:592 |
| `j` / `<Down>` | `Invoke(line_down)` | input.rs:593 |
| `k` / `<Up>` | `Invoke(line_up)` | input.rs:594 |
| `l` / `<Right>` | `Invoke(char_right)` | input.rs:595 |
| `0` / `<Home>` | `Invoke(line_start)` (only when count is 0) | input.rs:596 |
| `$` / `<End>` | `Invoke(line_end)` | input.rs:597 |
| `^` | `Invoke(first_non_blank)` | input.rs:598 |
| `w` | `Invoke(word_forward)` | input.rs:599 |
| `b` | `Invoke(word_backward)` | input.rs:600 |
| `e` | `Invoke(word_end)` | input.rs:601 |
| `W` | `Invoke(big_word_forward)` | input.rs:602 |
| `B` | `Invoke(big_word_backward)` | input.rs:603 |
| `E` | `Invoke(big_word_end)` | input.rs:604 |
| `}` | `Invoke(paragraph_forward)` | input.rs:605 |
| `{` | `Invoke(paragraph_backward)` | input.rs:606 |
| `)` | `Invoke(sentence_forward)` | input.rs:607 |
| `(` | `Invoke(sentence_backward)` | input.rs:608 |
| `G` | `Invoke(goto_last_line)` | input.rs:609 |

#### Viewport jumps

| Chord | Action | Source |
|---|---|---|
| `H` | `JumpViewport(Top)` | input.rs:612 |
| `M` | `JumpViewport(Middle)` | input.rs:613 |
| `L` | `JumpViewport(Bottom)` | input.rs:614 |

#### Pending prefixes

| Chord | Action | Source |
|---|---|---|
| `g` | `SetPending(AfterG)` | input.rs:617 |
| `z` | `SetPending(AfterZ)` | input.rs:618 |

#### Operator-leading keys

| Chord | Action | Source |
|---|---|---|
| `d` | `SetPending(AfterOperator(delete))` | input.rs:621 |
| `c` | `SetPending(AfterOperator(change))` | input.rs:622 |
| `y` | `SetPending(AfterOperator(yank))` | input.rs:623 |
| `>` | `SetPending(AfterOperator(indent_right))` | input.rs:624 |
| `<` | `SetPending(AfterOperator(indent_left))` | input.rs:625 |

#### Paste, line ops, find-repeat, mode entry, case

| Chord | Action | Source |
|---|---|---|
| `p` | `PasteAfter` | input.rs:628 |
| `P` | `PasteBefore` | input.rs:629 |
| `Y` | `Invoke(yank).with_range(CurrentLine)` | input.rs:632-634 |
| `x` | `Invoke(delete).with_target(Motion(char_right, None))` | input.rs:637-640 |
| `D` | `Invoke(delete).with_target(Motion(line_end, None))` | input.rs:643-646 |
| `C` | `Invoke(change).with_target(Motion(line_end, None))` | input.rs:647-650 |
| `S` | `Invoke(change).with_range(CurrentLine)` | input.rs:651-654 |
| `J` | `JoinLines { with_space: true }` | input.rs:657 |
| `;` | `FindRepeat { reverse: false }` | input.rs:660 |
| `,` | `FindRepeat { reverse: true }` | input.rs:661 |
| `i` | `EnterMode(Insert)` | input.rs:664 |
| `a` | `EnterAppend` | input.rs:665 |
| `o` | `OpenLineBelow` | input.rs:666 |
| `O` | `OpenLineAbove` | input.rs:667 |
| `:` | `EnterCommandLine` | input.rs:668 |
| `v` | `EnterVisual(Charwise)` | input.rs:669 |
| `V` | `EnterVisual(Linewise)` | input.rs:670 |
| `R` | `EnterMode(Replace)` | input.rs:671 |
| `~` | `ToggleCaseAtCursor` | input.rs:674 |
| `K` | `LspHoverRequest` | input.rs:682 |

#### Search / find-prefixes

| Chord | Action | Source |
|---|---|---|
| `/` | `EnterSearch(Forward)` | input.rs:685 |
| `?` | `EnterSearch(Backward)` | input.rs:686 |
| `n` | `SearchNext` | input.rs:687 |
| `N` | `SearchPrevious` | input.rs:688 |
| `*` | `SearchWordUnderCursor(Forward)` | input.rs:689 |
| `#` | `SearchWordUnderCursor(Backward)` | input.rs:690 |
| `%` | `MatchBracket` | input.rs:691 |
| `f` | `SetPending(AfterFindChar { Forward, None })` | input.rs:694-697 |
| `F` | `SetPending(AfterFindChar { Backward, None })` | input.rs:698-701 |
| `t` | `SetPending(AfterFindChar { TillForward, None })` | input.rs:702-705 |
| `T` | `SetPending(AfterFindChar { TillBackward, None })` | input.rs:706-709 |

#### Undo, dot, regs, marks, paging

| Chord | Action | Source |
|---|---|---|
| `u` | `Undo` | input.rs:712 |
| `.` | `RepeatLastChange` | input.rs:715 |
| `"` | `SetPending(AfterRegister)` | input.rs:719 |
| `m` | `SetPending(AfterSetMark)` | input.rs:722 |
| `'` | `SetPending(AfterJumpMarkLine)` | input.rs:723 |
| `` ` `` | `SetPending(AfterJumpMarkExact)` | input.rs:724 |
| `<PageDown>` | `Invoke(line_down).with_count(10)` | input.rs:727 |
| `<PageUp>` | `Invoke(line_up).with_count(10)` | input.rs:728 |

### Flags

- `q` is the only top-level Normal binding split by `recording_macro`.
- `0` is the only top-level Normal binding split by `pending_count`
  state (motion:line-start vs. count-digit accumulator). Both are
  state-coupled bindings the trie has to model with care.
- `<Tab>` arm is conditionally `JumpHistoryForward` only when the
  modifiers field is empty (`event.modifiers.is_empty()`); a Tab
  with any other modifier (e.g. terminals that emit Shift+Tab as
  Tab+SHIFT) falls through to the catch-all `_ => None`. The
  separate Ctrl-modified branch above doesn't claim `<C-i>` /
  `<Tab>` -- they're aliased deliberately.
- `<C-d>` / `<C-u>` mint a `Count(10)` rather than calling a
  half-page-down command; this is a count-injection at translate
  time. `keymap.rs` documents these but they're synthesised, not
  registered as their own command.

---

## After-`<C-w>` (`resolve_after_ctrl_w`, lines 743-783)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `SetPending(None)` | input.rs:744 |
| `<C-w>w` | second key Ctrl-w | `NextPane` | input.rs:758 |
| `<C-w><C-h>` | second key Ctrl-h | `NavigatePane(Left)` | input.rs:759 |
| `<C-w><C-j>` | second key Ctrl-j | `NavigatePane(Down)` | input.rs:760 |
| `<C-w><C-k>` | second key Ctrl-k | `NavigatePane(Up)` | input.rs:761 |
| `<C-w><C-l>` | second key Ctrl-l | `NavigatePane(Right)` | input.rs:762 |
| `<C-w><C-s>` | second key Ctrl-s | `SplitPaneHorizontal` | input.rs:763 |
| `<C-w><C-v>` | second key Ctrl-v | `SplitPaneVertical` | input.rs:764 |
| `<C-w><C-c>` / `<C-w><C-q>` | second key Ctrl-c / Ctrl-q | `ClosePane` | input.rs:765 |
| any other Ctrl | -- | `SetPending(None)` | input.rs:766 |
| `<C-w>s` / `<C-w>S` | -- | `SplitPaneHorizontal` | input.rs:770 |
| `<C-w>v` | -- | `SplitPaneVertical` | input.rs:771 |
| `<C-w>c` / `<C-w>q` | -- | `ClosePane` | input.rs:772 |
| `<C-w>h` / `<C-w><Left>` / `<C-w><BS>` | -- | `NavigatePane(Left)` | input.rs:773-775 |
| `<C-w>j` / `<C-w><Down>` | -- | `NavigatePane(Down)` | input.rs:776 |
| `<C-w>k` / `<C-w><Up>` | -- | `NavigatePane(Up)` | input.rs:777 |
| `<C-w>l` / `<C-w><Right>` | -- | `NavigatePane(Right)` | input.rs:778 |
| `<C-w>w` / `<C-w><Tab>` | -- | `NextPane` | input.rs:779 |
| `<C-w>W` / `<C-w><S-Tab>` | -- | `PrevPane` | input.rs:780 |
| anything else | -- | `SetPending(None)` | input.rs:781 |

### Flags

- The Ctrl-still-held vs. Ctrl-released paths are separate match arms
  but cover the same surface (`<C-w><C-h>` and `<C-w>h` both navigate
  left). `keymap.rs` only catalogs the Ctrl-released form; the
  Ctrl-held variants are **missing descriptors** (input.rs comment at
  747-756 explains why they're allowed).
- Backspace as left-pane-navigation (input.rs:773) and Tab/BackTab as
  cycle (input.rs:779-780) are **both missing descriptors** in
  `keymap.rs`.
- `<C-w>S` (capital S) aliases `<C-w>s` -- not in `keymap.rs`.

---

## After-`g` (`resolve_after_g`, lines 785-825)

| Chord | Action | Source |
|---|---|---|
| `gg` | `Invoke(goto_first_line)` | input.rs:788 |
| `gU` | `SetPending(AfterOperator(upper))` | input.rs:793 |
| `gu` | `SetPending(AfterOperator(lower))` | input.rs:794 |
| `g~` | `SetPending(AfterOperator(toggle_case))` | input.rs:795 |
| `gv` | `ReselectLastVisual` | input.rs:797 |
| `gJ` | `JoinLines { with_space: false }` | input.rs:799 |
| `g;` | `WalkMarkHistoryBack` | input.rs:801 |
| `g,` | `WalkMarkHistoryForward` | input.rs:802 |
| `gd` | `LspDefinitionRequest` | input.rs:807 |
| `gD` | `LspDeclarationRequest` | input.rs:810 |
| `gy` | `LspTypeDefinitionRequest` | input.rs:813 |
| `gI` | `LspImplementationRequest` | input.rs:817 |
| `gr` | `LspReferencesRequest` | input.rs:822 |
| anything else | `SetPending(None)` | input.rs:823 |

No `<Esc>` arm -- the catch-all `SetPending(None)` covers it. All
chords have `keymap.rs` rows.

---

## After-`z` (`resolve_after_z`, lines 979-1000)

| Chord | Action | Source |
|---|---|---|
| `<Esc>` | `SetPending(None)` | input.rs:980 |
| `zz` / `z.` | `ScrollCursorTo(Center)` | input.rs:984 |
| `zt` / `z<CR>` | `ScrollCursorTo(Top)` | input.rs:985 |
| `zb` / `z-` | `ScrollCursorTo(Bottom)` | input.rs:986 |
| `zf` | `CreateFoldFromVisual` | input.rs:988 |
| `zo` | `OpenFoldAtCursor` | input.rs:989 |
| `zc` | `CloseFoldAtCursor` | input.rs:990 |
| `za` | `ToggleFoldAtCursor` | input.rs:991 |
| `zR` | `OpenAllFolds` | input.rs:992 |
| `zM` | `CloseAllFolds` | input.rs:993 |
| `zd` | `DeleteFoldAtCursor` | input.rs:994 |
| `zj` | `GotoNextFold` | input.rs:995 |
| `zk` | `GotoPrevFold` | input.rs:996 |
| `zi` | `ToggleFoldEnable` | input.rs:997 |
| anything else | `SetPending(None)` | input.rs:998 |

All chords have `keymap.rs` rows.

---

## After-operator (`resolve_after_operator`, lines 827-937)

Operator-pending state. `op` is one of `delete`/`change`/`yank`/
`indent_right`/`indent_left`/`upper`/`lower`/`toggle_case`. The match
returns either an `Action::Invoke` (current-line variants and the
prefix-cases `f`/`F`/`t`/`T`/`i`/`a`) or a `Target::Motion` that's
wrapped at the end.

### Motions (each becomes `Target::Motion(.., Args::None)`)

| Chord | Resolves to motion | Source |
|---|---|---|
| `w` | `word_forward` | input.rs:834 |
| `b` | `word_backward` | input.rs:835 |
| `e` | `word_end` | input.rs:836 |
| `W` | `big_word_forward` | input.rs:837 |
| `B` | `big_word_backward` | input.rs:838 |
| `E` | `big_word_end` | input.rs:839 |
| `}` | `paragraph_forward` | input.rs:840 |
| `{` | `paragraph_backward` | input.rs:841 |
| `)` | `sentence_forward` | input.rs:842 |
| `(` | `sentence_backward` | input.rs:843 |
| `h` / `<Left>` | `char_left` | input.rs:844 |
| `l` / `<Right>` | `char_right` | input.rs:845 |
| `j` / `<Down>` | `line_down` | input.rs:846 |
| `k` / `<Up>` | `line_up` | input.rs:847 |
| `0` / `<Home>` | `line_start` | input.rs:848 |
| `$` / `<End>` | `line_end` | input.rs:849 |
| `^` | `first_non_blank` | input.rs:850 |

(After the match closes, the `target` is wrapped in
`Action::Invoke(CommandInvocation::of(op.0).with_target(target))` at
input.rs:936.)

### Doubled-operator current-line variants

| Chord | Pre-condition | Action (`with_range(CurrentLine)`) | Source |
|---|---|---|---|
| `dd` | `op == delete` | `Invoke(delete)` | input.rs:851-859 |
| `cc` | `op == change` | `Invoke(change)` | input.rs:860-866 |
| `yy` | `op == yank` | `Invoke(yank)` | input.rs:867-872 |
| `>>` | `op == indent_right` | `Invoke(indent_right)` | input.rs:873-877 |
| `<<` | `op == indent_left` | `Invoke(indent_left)` | input.rs:878-882 |
| `gUU` | `op == upper` | `Invoke(upper)` | input.rs:883-887 |
| `guu` | `op == lower` | `Invoke(lower)` | input.rs:888-892 |
| `g~~` | `op == toggle_case` | `Invoke(toggle_case)` | input.rs:893-897 |

These are state-coupled: the second key only doubles if it matches
the operator that is pending. Migration must encode "doubled-operator
=> current-line range" as a per-operator descriptor, not a per-key
binding.

### Find-prefix forwarders

| Chord | Action | Source |
|---|---|---|
| `f` | `SetPending(AfterFindChar { Forward, Some(op) })` | input.rs:898-903 |
| `F` | `SetPending(AfterFindChar { Backward, Some(op) })` | input.rs:904-909 |
| `t` | `SetPending(AfterFindChar { TillForward, Some(op) })` | input.rs:910-915 |
| `T` | `SetPending(AfterFindChar { TillBackward, Some(op) })` | input.rs:916-921 |

### Text-object prefix forwarders

| Chord | Action | Source |
|---|---|---|
| `i` | `SetPending(AfterTextObject { op, around: false })` | input.rs:922-927 |
| `a` | `SetPending(AfterTextObject { op, around: true })` | input.rs:928-933 |

### Esc / fallback

| Chord | Action | Source |
|---|---|---|
| `<Esc>` | `SetPending(None)` | input.rs:828-830 |
| anything else | `SetPending(None)` | input.rs:934 |

### Flags

- All operator-pending bindings are op-conditioned -- the same key
  produces a different `Action` depending on which operator is
  latched. `keymap.rs` lists `OperatorPending` as a binding mode but
  has zero rows under it (the actual motion / text-object table is
  the Normal-mode motions, reused). Migration has to decide whether
  to dup the rows or compose at lookup time.
- The doubled-operator arms are the "operator-pending => motion or
  doubled-operator" chord-prefix branching that the user-facing prompt
  flags as not mapping cleanly to a trie node.

---

## After-text-object (`resolve_after_text_object`, lines 1028-1127)

`around: bool` selects inner vs. around variant for each text-object
key. The op is whatever was pending; the text-object id is selected
from the builtins set.

| Chord | Around | Resolves to text-object | Source |
|---|---|---|---|
| `iw` / `aw` | false / true | `inner_word` / `around_word` | input.rs:1038-1044 |
| `iW` / `aW` | false / true | `inner_big_word` / `around_big_word` | input.rs:1045-1051 |
| `ip` / `ap` | false / true | `inner_paragraph` / `around_paragraph` | input.rs:1052-1058 |
| `is` / `as` | false / true | `inner_sentence` / `around_sentence` | input.rs:1059-1065 |
| `it` / `at` | false / true | `inner_tag` / `around_tag` | input.rs:1066-1072 |
| `i"` / `a"` | false / true | `inner_quote_double` / `around_quote_double` | input.rs:1073-1079 |
| `i'` / `a'` | false / true | `inner_quote_single` / `around_quote_single` | input.rs:1080-1086 |
| `` i` `` / `` a` `` | false / true | `inner_quote_backtick` / `around_quote_backtick` | input.rs:1087-1093 |
| `i(` / `i)` / `ib` / `a(` / `a)` / `ab` | false / true | `inner_paren` / `around_paren` | input.rs:1094-1100 |
| `i[` / `i]` / `a[` / `a]` | false / true | `inner_bracket` / `around_bracket` | input.rs:1101-1107 |
| `i{` / `i}` / `iB` / `a{` / `a}` / `aB` | false / true | `inner_brace` / `around_brace` | input.rs:1108-1114 |
| `i<` / `i>` / `a<` / `a>` | false / true | `inner_angle` / `around_angle` | input.rs:1115-1121 |
| `<Esc>` | -- | `SetPending(None)` | input.rs:1034-1036 |
| anything else | -- | `SetPending(None)` | input.rs:1122 |

The match is wrapped at input.rs:1124-1126 in
`Action::Invoke(CommandInvocation::of(operator.0)
.with_target(Target::TextObject(tobj, Args::None)))`.

### Flags

- `keymap.rs` has **zero text-object rows**. `mode: AfterTextObject`
  is declared but unpopulated. Every `iw`, `aw`, `i(`, `i"`, etc. is
  missing a descriptor.
- Many keys collapse to the same text-object via aliases:
  - `(` / `)` / `b` -> paren
  - `{` / `}` / `B` -> brace
  - `[` / `]` -> bracket
  - `<` / `>` -> angle

---

## After-find-char (`resolve_after_find_char`, lines 1129-1154)

Routes through one of four motion ids based on `kind`.

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `SetPending(None)` | input.rs:1135-1137 |
| any printable `<c>` | `operator == None` | `Invoke(motion_id).with_args(Char(c))` | input.rs:1148-1149 |
| any printable `<c>` | `operator == Some(op)` | `Invoke(op).with_target(Motion(motion_id, Char(c)))` | input.rs:1150-1153 |
| anything else (not `Char`) | -- | `SetPending(None)` | input.rs:1138-1141 |

`motion_id` selection (input.rs:1142-1147):

- `FindKind::Forward` -> `find_char_forward`
- `FindKind::Backward` -> `find_char_backward`
- `FindKind::TillForward` -> `till_char_forward`
- `FindKind::TillBackward` -> `till_char_backward`

`keymap.rs` rows exist for `f`/`F`/`t`/`T` as motions but the
`<char>` follow-up is not (and arguably can't) be enumerated.

---

## After-set-mark / After-jump-mark (lines 1002-1026)

| Chord | Pre-condition | Action | Source |
|---|---|---|---|
| `<Esc>` | -- | `SetPending(None)` | input.rs:1003-1005 / 1013-1015 |
| `<a-zA-Z0-9>` | (set) | `SetMark(c)` | input.rs:1007 |
| `<a-zA-Z0-9>` | (jump line) | `JumpToMarkLine(c)` | input.rs:1021 |
| `<a-zA-Z0-9>` | (jump exact) | `JumpToMarkExact(c)` | input.rs:1019 |
| anything else | -- | `SetPending(None)` | input.rs:1008 / 1024 |

No descriptors for the per-mark-letter rows in `keymap.rs` (consistent
with vim's design -- mark-letter is data, not a binding).

---

## After-register (`resolve_after_register`, lines 960-977)

| Pattern | Action | Source |
|---|---|---|
| `<Esc>` | `SetPending(None)` | input.rs:961-963 |
| `<a-zA-Z>` | `SelectRegister(Named(c))` | input.rs:969 |
| `<0-9>` | `SelectRegister(Numbered(c-'0'))` | input.rs:970 |
| `"` | `SelectRegister(Unnamed)` | input.rs:971 |
| `_` | `SelectRegister(BlackHole)` | input.rs:972 |
| `+` / `*` | `SelectRegister(System)` | input.rs:973 |
| anything else | `SetPending(None)` | input.rs:974 |

No descriptors in `keymap.rs` for register letters. Same rationale
as marks.

---

## After-macro-start (`resolve_after_macro_start`, lines 939-947)

| Pattern | Action | Source |
|---|---|---|
| `<Esc>` | `SetPending(None)` | input.rs:940-942 |
| `<a-zA-Z0-9>` | `StartMacroRecord(c)` | input.rs:944 |
| anything else | `SetPending(None)` | input.rs:945 |

---

## After-macro-play (`resolve_after_macro_play`, lines 949-958)

| Pattern | Action | Source |
|---|---|---|
| `<Esc>` | `SetPending(None)` | input.rs:950-952 |
| `@` | `PlayLastMacro` | input.rs:954 |
| `<a-zA-Z0-9>` | `PlayMacro(c)` | input.rs:955 |
| anything else | `SetPending(None)` | input.rs:956 |

---

# Cross-cutting findings

## Bindings that depend on more than chord + mode

1. **`q` in Normal** -- splits on `recording_macro`. (input.rs:585-586)
2. **`0` in Normal** -- splits on `pending_count > 0`. (input.rs:575-580 + 596)
3. **`I` / `A` in Visual** -- only fire when `kind == Blockwise`. (input.rs:192-198)
4. **All overlays** -- `picker_open` / `insert_completion_open` /
   `snippet_active && Insert` / `chord_capture && Command` precede
   normal modal dispatch.
5. **Help-buffer-local `<Esc>`/`q`/`<CR>`** -- only when `active_buffer`
   is `Help` or `FileTree`, and `Pending::None`, and Normal mode. The
   `q` arm additionally requires `!recording_macro`. (input.rs:140-150)
6. **Doubled-operator arms** in `resolve_after_operator` -- second
   key's meaning depends on which operator is pending (`dd` only fires
   if `op == delete`). (input.rs:851-897)
7. **`<Tab>` in Normal** -- only fires `JumpHistoryForward` if
   modifiers are empty; with any modifier it falls through. The
   Ctrl-modified branch above doesn't claim `<C-i>` because the
   terminal already collapses `<C-i>` to `<Tab>` for us. (input.rs:568-570)
8. **Two-stage `<Esc>` in Command + completion popup** -- first Esc
   dismisses the popup; second cancels the cmdline. State carried in
   `completion_open`, not the chord. (input.rs:279-287)

## Multi-key sequences with non-trivial pending branching

- **Operator-pending -> motion** (canonical case; flagged in prompt).
  The `Pending::AfterOperator(op)` carries the operator id, and the
  resolver dispatches one of: motion, current-line doubled, find-char
  forwarder, text-object forwarder. Three different chord-prefix
  branches converge on different Actions. (input.rs:827-937)
- **Operator-pending -> find-char -> char** is a 3-deep chord
  (`df<x>`) where stage 2 stashes the operator into the
  `AfterFindChar { operator: Some(op) }` payload. Stage 3 checks
  `operator.is_some()` and emits `with_target(Motion(.., Char(c)))`
  vs. plain `with_args(Char(c))`. (input.rs:898-921 + 1148-1153)
- **Operator-pending -> text-object -> object-char** (`ciw`) -- same
  shape, three deep, with `AfterTextObject { operator, around }`
  payload. (input.rs:922-933 + 1028-1127)
- **`<C-x><C-o>` / `<C-x><C-s>`** -- two-key Insert chord with
  `Pending::AfterCtrlX`. Trivial (just two arms) but encoded as
  pending state, not a trie node. (input.rs:372-387)
- **`<C-w><X>` window chord** -- second key has both Ctrl-held and
  Ctrl-released forms accepted. The trie has to alias both forms.
  (input.rs:743-783)
- **`zz` / `zt` / `zb` aliases** -- `zz`/`z.`, `zt`/`z<CR>`,
  `zb`/`z-` -- key aliases at the second-key level. Trivially modelled
  as multiple leaves on the same node. (input.rs:984-986)
- **`<C-w>w` / `<C-w><Tab>` / `<C-w><C-w>` aliases** for `NextPane`,
  same for `<C-w>W` / `<C-w><S-Tab>` for `PrevPane`. (input.rs:779-780,
  758)

## Actions that are not in `keymap.rs::default_keymap()`

(High-confidence list, walked against the keymap in this session.)

### Picker overlay (whole layer missing)

- `PickerAppend`, `PickerBackspace`, `PickerSelectNext`,
  `PickerSelectPrev`, `PickerAccept`, `PickerDismiss`.

### Command chord-capture overlay (whole layer missing)

- `CommandLineAppendChord`, `CommandLineDeleteChord`.
- `CommandLineCancel` and `CommandLineSubmit` are listed for
  `mode: Command` -- the chord-capture overlay's reuse of those is
  arguably already covered.

### Visual mode bindings missing descriptors

- `<Left>` / `<Down>` / `<Up>` / `<Right>` / `<Home>` / `<End>` aliases.
- `W`, `B`, `E`, `}`, `{`, `)`, `(` (motions).
- `>`, `<` (operators with `Range::Selection`).
- `x` (alias of `d`), `s` (alias of `c`).
- `I`, `A` (blockwise-only).

### Insert completion popup -- aliases missing

- `<Down>` (alias of `<C-n>`).
- `<Up>` (alias of `<C-p>`).
- `CompletionAcceptThenInsert(_)` wildcard binding has no row (and
  arguably can't have one -- it's a wildcard, see the open question
  below).

### After-`<C-w>` aliases missing

- All `<C-w><C-x>` Ctrl-still-held forms (8 chords): `<C-w><C-w>`,
  `<C-w><C-h>`, `<C-w><C-j>`, `<C-w><C-k>`, `<C-w><C-l>`,
  `<C-w><C-s>`, `<C-w><C-v>`, `<C-w><C-c>`, `<C-w><C-q>`.
- Bare-key non-letter aliases: `<C-w><Left>`, `<C-w><Down>`,
  `<C-w><Up>`, `<C-w><Right>`, `<C-w><BS>`, `<C-w><Tab>`,
  `<C-w><S-Tab>`, `<C-w>S` (capital).

### After-text-object (whole layer missing)

- `iw`, `aw`, `iW`, `aW`, `ip`, `ap`, `is`, `as`, `it`, `at`, `i"`,
  `a"`, `i'`, `a'`, `` i` ``, `` a` ``, `i(`, `i)`, `ib`, `a(`, `a)`,
  `ab`, `i[`, `i]`, `a[`, `a]`, `i{`, `i}`, `iB`, `a{`, `a}`, `aB`,
  `i<`, `i>`, `a<`, `a>`. ~36 chords.

### Marks / registers / macros (data-keyed, no rows)

- `m<X>`, `'<X>`, `` `<X> ``, `"<X>`, `q<X>`, `@<X>`, `@@`. Vim
  doesn't catalog these per-letter either; documenting the prefix
  itself probably suffices.

### Synthesized count-injection bindings

- `<C-d>` / `<C-u>` / `<PageDown>` / `<PageUp>` are listed in
  `keymap.rs` as motion-bound, but the actual translate emits
  `Invoke(motion).with_count(10)`. The `count: 10` synthesis is not
  documented in the descriptor.

### Help-buffer-local

- `<Esc>` / `q` / `<CR>` for help/file-tree are listed under
  `mode: Help` (3 rows); FileTree shares them by routing.

### LSP request bindings

- `K` -> `LspHoverRequest` is listed.
- `gd` / `gD` / `gy` / `gI` / `gr` are listed (as `mode: AfterG`).
- `<C-t>` -> `TagStackPop` is listed.

## Special-case escape hatches

- **Universal `<C-c>` => Quit** (input.rs:128-130). Preempted only by
  the chord-capture overlay (so `:describe-key <C-c>` works).
- **Picker `<C-c>` => PickerDismiss** (input.rs:345). Picker overrides
  the universal hatch deliberately.
- **`<C-x>` AfterCtrlX fallback drops pending on any non-Ctrl key**
  (input.rs:386). Soft-fail rather than committing the user to the
  prefix.
- **`<C-w>` second-key fallback drops pending on Esc and any
  unrecognised key** (input.rs:744-746, 766, 781).
- **All `resolve_after_*` resolvers drop pending on `<Esc>`**
  (consistent across all 9 helpers).

## Open questions for the migration

1. **Wildcard leaves.** `<Char>` catch-alls (Insert, Visual?, Search,
   Command, Picker, completion-popup commit-char, find-char target,
   mark name, register name, macro name) are nine distinct wildcard
   shapes. The trie needs an "any-char" leaf concept. Same for the
   chord-capture `format_chord` catch-all.
2. **State-coupled bindings.** `q` (recording state), `0` (count
   state), doubled-operator (operator state), Visual-`I`/`A`
   (visual-kind state), help/file-tree-buffer `<Esc>`/`q`/`<CR>`
   (active-buffer state) are five distinct guard styles. A clean
   registry probably wants a single "guard predicate on the binding"
   concept.
3. **Operator-pending re-use of Normal motions.** The same motion
   chords (`w`, `b`, `e`, `0`, `$`, `^`, `h/j/k/l`, ...) appear in
   Normal-default, Visual, and operator-pending. Three modes, one
   table -- the trie should reference the descriptor rather than
   dup the rows.
4. **`<Tab>` aliasing.** `<Tab>` aliases `<C-i>` (Normal), aliases
   `<C-n>` (picker, completion popup, after-`<C-w>` cycle), means
   "literal tab" (Insert default), means "next placeholder"
   (snippet), means "advance completion" (cmdline). The migration
   should pin "what `<Tab>` resolves to per layer" explicitly.
5. **Count-injection on `<C-d>` / `<C-u>` / `<PageDown>` / `<PageUp>`.**
   Either model "scroll-half-page" as its own command, or document
   the count synthesis on the keymap descriptor. Today neither is true.
6. **`recording_macro` plumbing.** The `q`-while-recording arm is one
   line; if every state-coupled split lands in keymap descriptors,
   recording state has to be queryable at lookup time.

