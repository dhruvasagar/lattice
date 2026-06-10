---
summary: "Modal editing: Normal / Insert / Visual / Command / Search / Replace, the vim grammar (operators + motions + text objects + counts), and registers / marks / macros."
related: [operator:, motion:, text-object:, register, mark, macro]
---

# Modal editing

Lattice is a **modal** editor. Different keystrokes do different
things depending on which mode you're in. The modes are a small
state machine; the transitions are deterministic; the grammar
is vim's, with a few principled extensions.

If you've never used vim: this is the page that gets you fluent.
If you have: skip to the grammar section — lattice is vim with
explicit unification of `:` and the keymap, and a few sharper
edges around modes that you'll find familiar but cleaner.

---

## The modes

| Mode               | Cursor look | What keys do                                    |
|--------------------|-------------|-------------------------------------------------|
| **Normal**         | Block       | Move + run commands; **default**                |
| **Insert**         | Bar (`|`)   | Type — keystrokes go into the buffer            |
| **Replace**        | Underline   | Type — overwrites instead of inserting          |
| **Visual** (3 sub) | Highlight   | Select a range; next operator acts on it        |
| **Operator-Pending** (transient) | Block | Awaiting motion / text-object after `d`/`y`/`c`/etc. |
| **Command**        | Bar at `:` line | Typing an ex-command (`:w`, `:set`, ...)     |
| **Search**         | Bar at `/` line | Typing a search pattern (`/foo`, `?bar`)     |

`<Esc>` always returns to Normal. From any text-input mode
(Insert, Replace, Command, Search), `<Esc>` cancels and goes
back. From Visual, `<Esc>` (or `v`/`V`/`<C-v>`) drops the
selection.

> The other axis — **major + minor modes** — is orthogonal to
> this. Modal state is your hands; major/minor modes are the
> editor's behaviour layers. See [`modes`](help:modes) for that
> axis.

---

## Quick reference: enter / leave each mode

| From Normal                             | To                                                  |
|-----------------------------------------|-----------------------------------------------------|
| `i`                                     | Insert at cursor                                    |
| `I`                                     | Insert at first non-blank of line                   |
| `a`                                     | Insert one column right (append)                    |
| `A`                                     | Insert at end-of-line                               |
| `o`                                     | Open a new line below + Insert                      |
| `O`                                     | Open a new line above + Insert                      |
| `R`                                     | Replace mode                                        |
| `v`                                     | Visual (charwise)                                   |
| `V`                                     | Visual (linewise)                                   |
| `<C-v>` (or `<C-q>`)                    | Visual (blockwise)                                  |
| `:`                                     | Command                                             |
| `/` / `?`                               | Search forward / backward                           |
| `gv`                                    | Re-select the previous Visual selection             |

| From Insert / Replace / Visual / Command / Search | To       |
|---------------------------------------------------|----------|
| `<Esc>`                                           | Normal   |

| From Visual | To                                                                              |
|-------------|---------------------------------------------------------------------------------|
| `v`/`V`/`<C-v>` | Switch among Visual sub-modes (or drop to Normal if same kind)              |
| `o`             | Swap selection anchor and cursor (extends to the *other* end)              |
| `<Esc>`         | Drop selection → Normal                                                     |

---

## The vim grammar

Every Normal-mode action is built from at most four pieces:

```
[register] [count] operator [count] motion-or-text-object
   |          |       |        |          |
   ▼          ▼       ▼        ▼          ▼
 "a         3       d         2        w
   |        |       |         |        |
 paste-     do-3-  delete    over-2  -words
 to / yank- of-                forward
 from
```

- `[register]` — `"a` ... `"z` / `"_` / `"+` ... — where to
  yank from / paste from. Optional; default `"`.
- `[count]` — leading number repeats the command. Optional;
  default 1.
- `operator` — `d` (delete), `y` (yank), `c` (change), `>`
  (indent), `<` (dedent), `gU` (uppercase), `gu` (lowercase),
  `g~` (toggle case), `=` (re-indent / format).
- `motion-or-text-object` — the *what* the operator acts on.

Examples:

| Key sequence | Meaning                                              |
|--------------|------------------------------------------------------|
| `dw`         | Delete word forward                                  |
| `d2w`        | Delete 2 words forward                               |
| `2dw`        | Same — count outside operator multiplies             |
| `"ayy`       | Yank current line into register `a`                  |
| `"ap`        | Paste from register `a`                              |
| `daw`        | Delete a word (with surrounding whitespace)          |
| `diw`        | Delete inner word (just the word chars)              |
| `>>`         | Indent current line                                  |
| `2>>`        | Indent 2 lines (current + 1 below)                   |
| `c$`         | Change to end-of-line                                |
| `gUw`        | Uppercase a word                                     |

In **Visual mode**, the selection IS the range — operators
take no further argument. `dw` outside Visual deletes a word;
inside Visual, `d` deletes the selection.

---

## Motions

A motion moves the cursor. After an operator (`d`, `y`, `c`,
...), the motion defines the operator's range.

### Cursor / character

| Key       | Motion                                        |
|-----------|-----------------------------------------------|
| `h` / `l`   | Left / right one character                       |
| `j` / `k`   | Down / up one **logical** line                   |
| `gj` / `gk` | Down / up one **display row** (wraps count)      |
| `0`         | Start of line (column 0)                         |
| `^`         | First non-blank of line                          |
| `$`         | End of line                                      |
| `g0`        | Start of current display row (under `:set wrap`) |
| `g$`        | End of current display row (under `:set wrap`)   |
| `g_`        | Last non-blank of line                           |
| `gg`        | First line                                       |
| `G`         | Last line                                        |
| `42G`       | Line 42                                          |
| `:42<CR>`   | Same as `42G` (jump-by-line)                     |

### Word / token

| Key     | Motion                                                            |
|---------|-------------------------------------------------------------------|
| `w`     | Next word start (word-class boundary)                             |
| `b`     | Previous word start                                               |
| `e`     | Next word end                                                     |
| `ge`    | Previous word end                                                 |
| `W`     | Next *WORD* (whitespace-separated; punctuation is part of WORD)   |
| `B`     | Previous WORD                                                     |
| `E`     | Next WORD end                                                     |
| `gE`    | Previous WORD end                                                 |

### Sentence / paragraph / section

| Key    | Motion                                  |
|--------|-----------------------------------------|
| `(`    | Previous sentence                       |
| `)`    | Next sentence                           |
| `{`    | Previous paragraph                      |
| `}`    | Next paragraph                          |
| `]]`   | Next section start                      |
| `[[`   | Previous section start                  |

### Find character on line

| Key  | Motion                                  |
|------|-----------------------------------------|
| `fX` | Jump to next `X` on this line (cursor lands on it) |
| `FX` | Same, backward                          |
| `tX` | Jump *till* next `X` (cursor lands on the char before) |
| `TX` | Same, backward                          |
| `;`  | Repeat last `f` / `F` / `t` / `T`       |
| `,`  | Reverse-repeat                          |
| `3fX`| Jump to the *3rd* `X` ahead             |

### Match / pair

| Key   | Motion                                              |
|-------|-----------------------------------------------------|
| `%`   | Jump to matching `(`/`)`, `[`/`]`, `{`/`}`, `<`/`>` |

### Viewport

| Key       | Motion                                             |
|-----------|----------------------------------------------------|
| `H`       | Top of visible viewport                            |
| `M`       | Middle of viewport                                 |
| `L`       | Bottom of viewport                                 |
| `<C-d>`   | Half-page down                                     |
| `<C-u>`   | Half-page up                                       |
| `<C-f>`   | Full page down                                     |
| `<C-b>`   | Full page up                                       |
| `<C-e>`   | Scroll one line down (cursor sticky to viewport)   |
| `<C-y>`   | Scroll one line up                                 |
| `zz`      | Centre cursor row in viewport                      |
| `zt`      | Cursor row at top of viewport                      |
| `zb`      | Cursor row at bottom of viewport                   |

### Search

| Key         | Motion                                          |
|-------------|-------------------------------------------------|
| `/pattern`  | Search forward for `pattern`                    |
| `?pattern`  | Search backward                                 |
| `n`         | Repeat last search (same direction)             |
| `N`         | Repeat backward                                 |
| `*`         | Search forward for word under cursor            |
| `#`         | Search backward for word under cursor           |

---

## Operators

An operator + a motion = a deletion / yank / etc. over the
motion's range.

| Operator | Action                                                              |
|----------|---------------------------------------------------------------------|
| `d`      | Delete (yanks into `"` register first)                              |
| `D`      | Same as `d$` (delete to end-of-line)                                |
| `y`      | Yank (copy)                                                         |
| `Y`      | Same as `y$`                                                        |
| `c`      | Change — delete + Insert                                            |
| `C`      | Same as `c$`                                                        |
| `s`      | Substitute char — `cl` (delete one + Insert)                        |
| `S`      | Substitute line — `cc`                                              |
| `>`      | Indent right                                                        |
| `<`      | Indent left                                                         |
| `gU`     | Uppercase                                                           |
| `gu`     | Lowercase                                                           |
| `g~`     | Toggle case                                                         |
| `~`      | Toggle case on cursor char (no motion needed)                       |
| `=`      | Re-indent / format (uses LSP if `lsp-format-mode` active)           |

### Doubled operators = whole-line

`dd` / `yy` / `cc` / `>>` / `<<` / `gUU` / `guu` / `g~~` all
operate on the cursor's whole line. Counts apply: `5dd`
deletes 5 lines.

### `.` repeat

`.` repeats the last *change* (operator + motion). Yank-only
invocations don't go on the dot register; only mutating ops.

---

## Text objects

Text objects are the *what* arguments that aren't motions —
they describe a structured chunk of buffer.

| Object         | Meaning                                              |
|----------------|------------------------------------------------------|
| `aw` / `iw`    | A word (with whitespace) / inner word                |
| `aW` / `iW`    | A WORD / inner WORD                                  |
| `as` / `is`    | A sentence / inner sentence                          |
| `ap` / `ip`    | A paragraph / inner paragraph                        |
| `a"` / `i"`    | Around / inside double-quoted string                 |
| `a'` / `i'`    | Around / inside single-quoted                        |
| `` a` `` / `` i` `` | Around / inside backtick-quoted                |
| `a(` / `i(`    | Around / inside parens (also `ab` / `ib`)            |
| `a{` / `i{`    | Around / inside braces (also `aB` / `iB`)            |
| `a[` / `i[`    | Around / inside brackets                             |
| `a<` / `i<`    | Around / inside angle brackets                       |
| `at` / `it`    | Around / inside an HTML/XML tag                      |

Combine with any operator: `daw` deletes a word + whitespace,
`ci"` changes the inside of a double-quoted string, `ya{`
yanks a brace block including the braces, `>>i{` indents
inside a brace block (without the braces).

In Visual mode, text objects extend the selection: `vi{`
selects everything inside the surrounding `{...}`.

### Tree-sitter text objects

> **Status: function / class / parameter / loop + comment objects landed
> 2026-06-10 (N.1.4–N.1.6); Visual-mode selection is the remaining
> follow-up.** The query substrate (`textobjects.scm` +
> `scope_at_cursor`) landed in N.1.0; the `zn` narrow operator in N.1.3.
> Design:
> [`../dev/architecture/tree-sitter-text-objects.md`](../dev/architecture/tree-sitter-text-objects.md).

The objects above are delimiter- and whitespace-based — they don't
know what a *function* or a *type* is. Tree-sitter text objects add
**structural** objects read from the syntax tree, registered as
first-class grammar objects: each composes with **every** operator,
exactly like `iw` or `ip`. `a` (outer) is the whole construct
(signature → closing brace); `i` (inner) is the body without the
delimiters.

| Object | outer / inner | Selects |
|--------|---------------|---------|
| **Function** | `af` / `if` | a function, method, or closure |
| **Class / type** | `ac` / `ic` | a `struct` / `enum` / `trait` / `impl` (Rust), `class` (Python/JS) |
| **Parameter / arg** | `aa` / `ia` | one argument in a parameter / argument list |
| **Loop** | `al` / `il` | a `for` / `while` / `loop` |
| **Comment** | `aC` / `iC` | `aC` the comment block incl. markers; `iC` the comment text (first line's leader stripped). Commentstring-driven — any language with a known line-comment leader, no parse tree needed |

Combine with any operator: `daf` deletes a function, `cic` changes
a class body, `daa` deletes an argument, `yif` yanks a function
body, `=af` reindents a function, and `znaf` narrows to a function
(the `zn` narrow operator). They resolve against the live syntax
tree, off the UI thread.

**v1 notes.** Resolution is byte-precise — `daa` deletes exactly
`x: i32`, not the whole signature line. Two rough edges for now:
`if` / `ic` / `il` include the body's braces in brace languages
(Python's brace-less suite is clean); and `aa` and `ia` resolve
identically (the trailing comma isn't captured yet). The comment
object scans line comments (`//`, `#`); block comments (`/* */`) and
trailing comments are a follow-up. The remaining cross-cutting
follow-up is Visual-mode text objects (`vaf` / `vaC` / …) — which will
apply to *every* object uniformly, not just these.

**Why these keys.** `t` (tag) is already a classic object, so
class/type takes `ac`/`ic` (the nvim-treesitter convention) rather
than Helix's `t`. Comment takes capital `aC`/`iC` — preserving the
inner-vs-outer distinction a single `gc` object can't express — and
leaves `gc` free for a future comment *operator* (`gcc`). `call` /
`conditional` / `block` have no dedicated key yet (their natural
letters clash with the classic paren/brace objects); reach them via
the classic `i(` / `i{` and via the narrow operator below.

**Innermost wins.** With the cursor inside a closure nested in a
function, `af` targets the **closure**; move to the function's
signature line to target the whole function.

**The narrow operator.** `zn` is a narrow operator that pairs with
any motion or text object: `znaf` narrows a function, `znic` an
inner class, `znip` a paragraph, `zni{` inside braces, `znG` to
end-of-file. Edits in the narrow view save back to the source file;
`:widen` (or `q`) closes it. See
[`../dev/architecture/narrow-mode.md`](../dev/architecture/narrow-mode.md).

**Language coverage today:** Rust, Python, JavaScript ship a
`textobjects.scm`. A language with no query simply has no
tree-sitter objects (the delimiter objects above still work).
Adding a language's objects is one query file — see
[languages.md](languages.md).

---

## Visual modes

Visual is "select-then-act." Three sub-modes:

| Mode      | Enter        | Selects                                         |
|-----------|--------------|-------------------------------------------------|
| Charwise  | `v`          | Char-by-char from anchor to cursor              |
| Linewise  | `V`          | Whole lines from anchor's line to cursor's line |
| Blockwise | `<C-v>`      | Rectangle anchored at anchor, extending to cursor |

Once in Visual:

- Any motion extends the selection.
- An operator (`d` / `y` / `c` / `>` / `<` / `gU` / ...) acts
  on the selection and exits Visual.
- `o` swaps anchor and cursor (extends to the other end).
- `gv` re-selects the previous Visual selection (Normal only).

### Blockwise specials

In blockwise Visual:

- `I` enters Insert at the **left edge** of the block; on
  `<Esc>` whatever you typed replicates on every row at that
  column.
- `A` enters Insert at the **right edge** + 1; same replicate
  behaviour.
- `>` / `<` indent / dedent the rectangle.
- `r` followed by a char fills the rectangle with that char.

The whole replicate session is one undo unit — `u` once
reverts the entire block edit.

---

## Registers + macros

Registers are named buffers for yank / paste.

- `"a` ... `"z` — named registers (lowercase appends, uppercase
  appends)
- `"0` ... `"9` — numbered (0 = last yank; 1-9 = recent deletes)
- `"+` — system clipboard (when the platform supports it)
- `"_` — black hole (write-only sink; `"_dd` deletes without
  affecting any register)
- `"%` — current buffer's path (read-only)

Use:

- `"ayy` — yank line into register `a`
- `"ap` — paste from register `a`
- `:reg<CR>` — list every populated register

Macros record key sequences:

- `qa` — start recording into register `a`
- `q` — stop recording
- `@a` — replay the macro
- `@@` — replay the *last-played* macro (not necessarily `@a`)
- `5@a` — replay macro `a` 5 times

Macros record `CommandInvocation` sequences, not raw
keystrokes — so a recorded macro survives keymap changes
(rebinding `j` doesn't break a macro that uses motion-down).

---

## Counts

A leading number repeats. Almost everywhere:

- `5j` — 5 lines down
- `3w` — 3 words forward
- `2dd` — delete 2 lines
- `c3l` — change 3 chars
- `5x` — delete 5 chars
- `4>>` — indent 4 lines

Counts can sit before *or after* an operator: `2dw` and `d2w`
both delete 2 words. The convention is "operator first when
the count is for the motion" but they're interchangeable.

---

## Edge cases

### `<Esc>` from Insert moves the cursor left

vim convention. Coming back to Normal, the cursor sits on the
char *before* where you were typing. Use `a` (append) to keep
the cursor where it was visually.

### `c` with a motion that doesn't change anything

Vim quirk: `cw` on a single-char word with no whitespace ahead
is well-defined; `c$` on an empty line just enters Insert. No
errors, just no-op edits.

### Counts with `f` / `t`

`3fX` jumps to the *third* `X` ahead, not "find `X` 3 times."
`f` always lands on the char; `t` always lands one before.
`;` repeats with the count it remembers.

### Repeating `.` after a count

`.` repeats the operator + motion + count. So if you ran
`3dd`, then `.` deletes 3 more lines. To repeat without the
count, prefix `.` with a different count: `5.` repeats with
count 5.

### Block-visual on short rows

When a block-visual selection's right edge extends past a
short row's content, that row's contribution is silently
skipped for `I` / `A` insertion (vim's behaviour). The other
rows still get the replicated text.

---

## See also

- [`modes`](help:modes) — major + minor modes (the *other*
  mode axis).
- [`buffers`](help:buffers) — buffers, panes, splits.
- [`folding`](help:folding) — fold-aware motions + operators
  (`zj`/`zk`, dd-on-fold semantics).
- [`completion`](help:completion) — insert-mode completion +
  ghost text.
- [`ex-commands`](help:ex-commands) — the `:` line.
- [`options`](help:options) — `:set` + `:customize`.
- [`lsp`](help:lsp) — LSP feature surface.
- [`tutor`](help:tutor) — interactive lesson sequence (when
  available).
- `docs/dev/architecture/keymap-architecture.md` — developer
  reference for the keymap registry + dispatch.
