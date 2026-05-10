# Folding

Folds collapse a contiguous range of buffer lines into a single
visual line so you can navigate large files by structure instead
of scrolling. lattice's folds feel **physical** — the heading row
stays visible with full syntax highlighting, the body disappears
out of the visual flow, a small gutter glyph and a dim summary
suffix mark the affordance, and motions / operators treat the
closed fold as one logical line. The model is the same as
emacs's `outline-mode` and org-mode TAB-cycling, applied
universally across languages via tree-sitter / indent / markdown
detection.

> **Status:** computed folds (tree-sitter + markdown + indent
> cascade), the syntax-highlighted heading render, and the
> operator semantics described here are the **planned** v1
> behavior tracked under task C.2 in
> [`../dev/operations/implementation.md`](../dev/operations/implementation.md). Manual folds
> (`zf` / `zo` / `zc` / `za` / `zR` / `zM` / `zd`) are
> shipped today; the rest of this doc is the contract for
> the rollout.

---

## Quick reference

| Keystroke / command | Meaning                                                                    |
|---------------------|----------------------------------------------------------------------------|
| `zf` (visual)       | Create a manual fold over the visual selection                             |
| `za`                | Toggle the fold under cursor (open ↔ closed)                               |
| `zo`                | Open the fold under cursor                                                 |
| `zc`                | Close the fold under cursor                                                |
| `zR`                | Open every fold in the buffer                                              |
| `zM`                | Close every fold in the buffer                                             |
| `zd`                | Delete the fold under cursor (manual folds only; computed folds re-emerge) |
| `zj` / `zk`         | Jump to the next / previous fold start                                     |
| `:set foldmethod=X` | Pick the fold provider: `manual` / `indent` / `markdown` / `syntax`        |
| `:set nofoldenable` | Hide all fold affordances (folds still exist, all lines render)            |

---

## Concepts

### The two states of a fold

A fold is a contiguous line range with an `open` / `closed` flag.
Closed = body hidden, heading visible with a `▸` glyph in the
gutter. Open = body visible, heading shows a `▾` glyph as a guide.
Toggling is the primary user gesture (`za`, or TAB on a heading
in a doc-mode buffer).

### Where folds come from

Folds are produced by a **fold provider** that walks the buffer.
There are four built-in providers:

1. **`manual`** — only the folds the user creates with `zf`. Stable
   line ranges; not affected by buffer content.
2. **`indent`** — folds wherever the indent level rises. The fold
   ends when the indent returns to that level or below. Works
   across every language; trades precision for universal coverage.
3. **`markdown`** — `^#+ ` headings define the fold tree. A `# H1`
   folds until the next `# H1`; a `## H2` folds until the next
   same-or-higher heading. Nests cleanly. Triggers automatically
   on `*.md` buffers.
4. **`syntax`** — tree-sitter scope queries (functions, classes,
   blocks, etc.). Per-language; ships with each grammar's
   `folds.scm`. The most precise method when available.

The active provider is governed by `foldmethod` (the option
explained below). The default is `syntax` with a cascading
fallback: tree-sitter → markdown (for `.md`) → indent.

### Identity, not position

When the buffer changes, folds get recomputed. To keep your
open / closed choices stable across edits, each computed fold
carries an **identity**: a hash of its start-line text plus
indent depth. After recompute, lattice matches new folds to old
ones by identity and transfers their open / closed state. So:

- Adding a line to fold A doesn't reopen the closed fold B
  elsewhere.
- Renaming a heading from `# Configuration` to `# Config` creates
  a new identity; the old one's closed state doesn't transfer.
  This matches vim and emacs (and is rare in practice).
- Two headings with identical text at the same depth share the
  same identity and the same open / closed state. v1 documented
  limitation; same as vim.

---

## Visual presentation

### TUI rendering

Take a markdown buffer with two top-level headings and `#
Configuration`'s `## Editor settings` subsection collapsed:

```text
   1 │▾ # Configuration
   2 │  This document explains...
   3 │
   4 │▸ ## Editor settings  ┄ 4 lines folded
   5 │
   6 │▾ ## Plugin paths
   7 │  - foo
   8 │  - bar
   9 │
  10 │▸ # Keymap            ┄ 87 lines folded
  11 │ ...
```

Notice three things:

1. **The heading line keeps its full syntax highlighting.** Line
   4 is still rendered through the markdown highlight pipeline —
   `## Editor settings` reads as a heading, the same color and
   weight as if the fold were open. Vim before patch 9.x lost this
   (everything became one `Folded` highlight group); we preserve
   the original look. This is why the affordance reads as
   "section is collapsed" rather than "this is a fold marker".
2. **The gutter glyph column** is one cell wide, between the line
   number and the content. `▸` (U+25B8) on closed-fold heading
   rows; `▾` (U+25BE) on open-fold heading rows; blank elsewhere.
   For terminals that fall back to box-drawing, a config option
   selects an ASCII fallback (`>` / `v`).
3. **The closed-fold summary suffix** is dim, italic, and
   right-padded so it doesn't crowd the heading. Format:
   `┄ N lines folded`. The `┄` (U+2504) is a faint horizontal
   that reads as "metadata" not "content".

The body lines never reach the renderer; `compose_visible_lines`
skips them.

### Cursor placement

The cursor never sits inside the body of a closed fold. If the
buffer is edited such that the cursor would land on a hidden
line, it's moved to the fold's heading line.

### GPU rendering (planned)

The same data drives the GPU compositor with these additions:

- **Animated open / close** — height interpolation (~150ms
  ease-out-cubic) when the user toggles. Skipped for mass
  toggles like `zM` / `zR` to keep the UX snappy.
- **Sprite glyphs** — the `▸` / `▾` come from the `§5.6.7`
  icon atlas, antialiased and theme-customizable.
- **Hover preview** — hovering a closed fold pops a small
  floating buffer showing the first ~5 body lines, faded.
- **Indent guides** — vertical 1px lines through the gutter
  showing nesting depth (post-1.0).
- **Subtle separator** — a 1px hairline beneath a closed fold's
  heading row so the section reads as a complete card.

The TUI already passes the data; the GPU work is purely visual
polish on the same fold set.

---

## Operator semantics

This is the section that determines whether folds feel right or
feel like a hack. The principle:

> A closed fold is **one visual line**. Operators that take a
> "current line" and motions that step "by line" treat the fold
> as a single unit. An open fold is N lines and behaves like
> regular text.

### Operators on a closed fold's heading

The cursor is on the heading row of a closed fold. The fold
extends from `start_line` to `end_line`.

| Keystroke | Behavior                                                     | Range affected           |
|-----------|--------------------------------------------------------------|--------------------------|
| `dd`      | Delete the entire fold                                       | `[start..=end]`          |
| `yy`      | Yank the entire fold's content (with newlines)               | `[start..=end]`          |
| `cc`      | Delete the fold's content, enter Insert at `(start, 0)`      | `[start..=end]`          |
| `>>`      | Indent every line in the fold                                | `[start..=end]`, batched |
| `<<`      | Dedent every line in the fold                                | `[start..=end]`, batched |
| `gUU`     | Uppercase every byte in the fold                             | `[start..=end]`          |
| `guu`     | Lowercase every byte in the fold                             | `[start..=end]`          |
| `g~~`     | Toggle case in the fold                                      | `[start..=end]`          |
| `2dd`     | Delete the fold + 1 more **visible** line                    | fold + next visible      |
| `3yy`     | Yank the fold + 2 more visible lines (each fold counts as 1) | fold + next 2 visible    |

The whole operation lands as **one undo unit** (same guarantee
as `2>>` / `2dd` over multiple regular lines).

### Operators inside an open fold

When the fold is open, every operator behaves as if the fold
weren't there. `dd` on a body line deletes just that line.

### Text objects

Text objects (`iw` / `aw` / `ip` / `ap` / `i{` / `a{` etc.)
operate on buffer characters and lines as if folds weren't there
— they don't know about visual lines. This matches vim. So
`dap` on a heading inside an open fold deletes the paragraph
from buffer line K to the next blank line, regardless of fold
boundaries.

### Cursor placement after operations

| Operation             | Where the cursor lands                                 |
|-----------------------|--------------------------------------------------------|
| `dd` on a fold        | The line that was after the fold (now at `start_line`) |
| `cc` on a fold        | `(start_line, 0)`, then Insert mode                    |
| `yy` on a fold        | Stays on the fold's heading                            |
| `>>` / `<<`           | Stays on the fold's heading                            |
| `gUU` / `guu` / `g~~` | Stays on the fold's heading                            |

---

## Visual selection

Visual selection's `anchor` and `head` are **buffer positions**
(line + column), not visible-row counts. So selections naturally
include folded body lines once the head moves past a fold.

### Linewise (`V`)

```
4│  > Press V here
5│▸ ## Editor settings  ┄ 4 lines folded   ← cursor j's into this row
6│
7│▾ ## Plugin paths                        ← second j lands here
```

1. `V` enters Linewise visual at line 4. Anchor = `(4, 0)`.
2. `j` moves head **one visible line down**. Line 5 starts a
   closed fold ending at line 8 (the `## Editor settings` heading
   plus 3 body lines). Head jumps to `(8, EOL)`. Selection
   highlights all of lines 4-8.
3. Second `j` moves head to `(10, EOL)` (the next visible line
   after line 9's blank).
4. `d` reads `selections().primary()` — anchor `(4, 0)`, head
   `(10, EOL)` — and deletes buffer lines 4-10, fold body
   included.

### Charwise (`v`)

Same mechanism. The fold's body lines fall between anchor and
head and get included in the operator's range.

### Blockwise (`Ctrl-V`)

Block-visual operates on the column slice of every buffer line
in the visual line range, including folded ones. This matches
vim. The dispatcher's per-row blockwise walk visits every line
regardless of fold state.

---

## Motions

| Motion                | Behavior with closed folds                                                               |
|-----------------------|------------------------------------------------------------------------------------------|
| `j` / `k`             | Step to next / previous **visible** line. Skip closed-fold body.                         |
| `h` / `l`             | Same line, ignore folds.                                                                 |
| `0` / `$` / `^`       | Same line, ignore folds.                                                                 |
| `w` / `b` / `e`       | Word motions on the current line. Cross-line word motion lands on the next visible line. |
| `gg`                  | Buffer line 1. **Opens** the containing fold if line 1 is hidden.                        |
| `G`                   | Last line. **Opens** the containing fold if needed.                                      |
| `5G` / `:5`           | Buffer line 5. **Opens** the containing fold if needed.                                  |
| `H` / `M` / `L`       | Viewport top / middle / bottom — counted in visible rows. Never lands inside a fold.     |
| `Ctrl-D` / `Ctrl-U`   | Half-page; counts visible rows.                                                          |
| `n` / `N`             | Search next / prev. **Opens** the containing fold on a match.                            |
| `*` / `#`             | Word search. **Opens** on a match.                                                       |
| `f` / `F` / `t` / `T` | Find-char on the current line; folds irrelevant.                                         |
| `%`                   | Match-bracket. **Opens** the containing fold if the match is hidden.                     |
| `Ctrl-O` / `Ctrl-I`   | Position history. **Opens** the containing fold on jump.                                 |
| `gd` / `gD`           | Goto-definition (LSP, planned). Will **open** the containing fold.                       |

### Auto-open semantics

Five jump categories trigger an auto-open of the destination's
containing fold:

1. Search (`n`, `N`, `*`, `#`, `/`, `?`)
2. Vertical jumps (`gg`, `G`, numeric `5G`)
3. Mark jumps (`'a`, `` `a ``)
4. Position-history hops (`Ctrl-O`, `Ctrl-I`)
5. Bracket-match (`%`)

Categories vim has but lattice does not auto-open:

- `block` — operator-touched ranges (vim auto-opens for visibility
  during the op; lattice's range expansion already handles this
  by including the fold).
- `hor` — horizontal motion across a fold boundary (rare; vim
  opens; we don't, since `j`/`k` step visible lines and never
  cross into a fold body).
- `quickfix` / `tag` — features lattice doesn't have yet.
- `undo` — auto-open after an undo that crosses a fold (vim
  opens; we don't, since the cursor lands on the heading).

When the typed-options registry ships, `:set foldopen=...` will
let users tune the set; v1 ships the five-category default
hardcoded.

---

## Edge cases

### `o` / `O` from a fold-start

`o` opens a new line below. Vim's behavior on a closed fold:
the new line lands **after the fold's end**, not inside it. So
on a closed fold from line 4 to line 16, `o` creates a new line
at 17 (pushing 17+ down). `O` similarly lands above the fold's
start.

### `p` / `P` from a fold-start

Paste-after lands **after the fold's end_line**. Linewise yank
content gets pasted starting on the line after the fold;
charwise content gets pasted at the heading's end-of-line, then
flows past the fold.

### Visible-line counts

`5G` is buffer-line-5 (vim's behavior). `5j` is **5 visible lines
down** from the current line — a closed fold counts as 1. So
`5j` from a heading might step over multiple folds before
landing.

`H`, `M`, `L`, `Ctrl-D`, `Ctrl-U`, `Ctrl-E`, `Ctrl-Y` all count
visible rows.

### Search match inside a closed fold

The fold opens, the cursor lands on the match. The fold's open
state is now part of the merged state; if the buffer is
re-folded later (`zM` then `zR`), this fold's open state will
follow the new computation.

### Repeated `dd` over folds

Three closed folds in a row, cursor on the first heading: `3dd`
deletes all three folds and their bodies, as a single undo unit.
Visual count = 3, matched by walking visible lines forward.

### Edits that change a fold's identity

If you rename `# Configuration` to `# Config`, the heading text
hash changes, so the new computed fold has a different identity.
Its open / closed state defaults to **open** (the recompute
doesn't find a match in the old folds). Same as vim.

### Empty / single-line ranges

`zf` over a single-line visual selection is silently skipped.
Computed folds are emitted only when the candidate range spans
at least 2 lines.

### Mixing manual and computed folds

When `foldmethod` is one of the computed methods (`indent` /
`markdown` / `syntax`), manually-created folds (`zf`) coexist
with the computed set. The provider runs over the buffer and
emits computed ranges; the manual folds are merged in, identity-
matched against any prior manual entries, and the closed state
preserved. `zd` removes a manual fold; for computed folds,
`zd` is a no-op (the next recompute would re-emerge the fold).

---

## Configuration

### `foldmethod` — pick the provider

```vim
:set foldmethod=manual    " only zf-created folds
:set foldmethod=indent    " indent-rise folds
:set foldmethod=markdown  " heading-based folds for *.md
:set foldmethod=syntax    " tree-sitter scopes (cascades to markdown / indent)
```

Default: `syntax` per buffer, with the cascade described above.
Per-extension defaults (e.g. `markdown` for `*.md`) ship with
the major-mode registry (Phase 8). Until then, lattice infers
the right method from the file extension at buffer-open time.

### `foldenable` — disable the affordance globally

```vim
:set foldenable      " (default) folds render and operate
:set nofoldenable    " all lines render; fold data is preserved
```

`nofoldenable` doesn't delete folds — just suppresses the
visual collapse. Useful when grepping or skimming.

### `foldopen` (planned, ships with typed-options)

```vim
:set foldopen=search,jump,mark,history,bracket   " v1 default
```

The five categories listed under "Auto-open semantics" above.

---

## Implementation notes (for contributors)

The fold provider trait in `lattice-grammar` (or `lattice-syntax`):

```rust
pub trait FoldProvider: Send + Sync {
    fn id(&self) -> FoldSource;
    /// Pure function of the snapshot. Cheap to recompute on
    /// text_version bump.
    fn compute(&self, snap: &DocumentSnapshot) -> Vec<ComputedFold>;
}

pub enum FoldSource { Manual, Indent, MarkdownHeading, OrgHeading, Syntax }

pub struct ComputedFold {
    pub start_line: u32,
    pub end_line: u32,
    /// Hash of (trimmed start-line text, indent depth).
    pub identity: u64,
    pub source: FoldSource,
}
```

The App owns fold state and pre-expands `Range::CurrentLine`
into a `Range::Span` covering the containing fold before
dispatch. The grammar dispatcher stays fold-agnostic; folds are
App state, not grammar state.

```rust
// In App::run_invocation, before dispatch_blocking:
fn expand_range_for_folds(&self, mut inv: CommandInvocation) -> CommandInvocation {
    if let Some(Range::CurrentLine) = inv.range
        && let Some(fold) = self.fold_at(self.cursor.line)
        && fold.closed
    {
        let span = (Position::new(fold.start_line, 0),
                    Position::new(fold.end_line, line_byte_len(buffer, fold.end_line)));
        inv = inv.with_range(Range::Span(span));
    }
    inv
}
```

The cursor-step helpers `next_visible_line` / `prev_visible_line`
on the App walk past closed folds; `j` / `k` use them.

The renderer `render_styled_line` for a closed-fold heading runs
the normal syntax-highlight pipeline, then the fold-aware
wrapper appends a dim `┄ N lines folded` span. The gutter writer
chooses the glyph based on `app.fold_state_at(line)`:
`Open` → `▾`, `Closed` → `▸`, `None` → blank.

---

## See also

- [`../dev/operations/implementation.md`](../dev/operations/implementation.md) — current build
  status; check the Phase 1 row + the "Up next" list for fold
  rollout progress.
- [`../dev/architecture/design.md`](../dev/architecture/design.md) §15:18 — fold storage / interaction
  open question (resolved by the design here).
- vim's `:help folding` — the reference we model strict semantics
  on (with the syntax-highlight-preserving improvement noted in
  this doc).
- emacs's `outline-mode` and `org-mode` TAB-cycling — the UX
  inspiration.
