# command-line-mode

Lattice's command line is a **rich editing surface** — a real
buffer with a major mode, not a single-line widget. It supports the
full vim grammar, syntax highlighting, live error indication, and
fuzzy history browsing. This is the **rich minibuffer**
(`docs/dev/architecture/rich-minibuffer.md`).

## The `:` line (tier 1)

Press `:` (Normal mode) to open the command line. It is a one-line
readline buffer (`*command-line*` with `command-line-mode` major)
edited through the universal Insert grammar:

| Chord | Action |
|---|---|
| `:` | Open command line (empty) |
| `<CR>` | Submit — parse + execute the command |
| `<Esc>` / `<C-c>` | Cancel — restore prior buffer + cursor |
| `<C-p>` / `<Up>` | Previous history entry |
| `<C-n>` / `<Down>` | Next history entry |
| `<Tab>` | Open completion popup / advance selection |
| `<S-Tab>` | Previous completion candidate |
| `<C-h>` | Describe command / arg under cursor |
| `<C-x><C-o>` | Open the picker for the argument under the cursor |
| `<C-x><C-e>` | Expand into full-modal mini-buffer band |

### Picking an argument (`<C-x><C-o>`)

`<Tab>` completes an argument inline. Some arguments can offer more than
a list of words — a git revision, for instance, is better chosen from a
log with its subject line and date beside it. Where a command declares
one, **`<C-x><C-o>` opens that argument's picker**, and what you choose
replaces what you had typed for that argument:

```
:magit-checkout ma<C-x><C-o>     → the revision picker, filtered work intact
:magit-checkout main             → after picking
```

The picker opens **already filtered by what you had typed**, so the chord
picks your typing up rather than throwing it away — unless nothing
matches it, in which case you get the full list instead of an empty one.

It is vim's omni-completion chord, and it means the same thing here: ask
whatever knows about this position. An argument can offer both — `<Tab>`
for the quick inline answer, `<C-x><C-o>` for the richer surface — and
if it offers no picker, the editor says so rather than doing nothing.

All universal Insert-mode readline chords work: `<C-a>` (start of
line), `<C-e>` (end of line), `<C-b>` (back one char), `<C-f>`
(forward one char), `<C-w>` (delete word backward), `<C-u>` (delete
to start of line), `<Left>` / `<Right>` cursor keys, `<Del>`.

The `:` line has **live syntax highlighting** (command word, range
prefix, bang, `:s///` pattern·replacement·flags), a **live error
indicator** (unknown command, bad args), and **parameter hints**
(from the resolved command's `ArgSpec`). `:s///` shows a
**substitution preview** on the target buffer as you type.

## Expanding the command line (`<C-x><C-e>`, tier 2)

`<C-x><C-e>` grows the one-row `:` line **in place** into a
full-width multi-row **mini-buffer band** at the bottom of the
frame, pushing the panes and mode-lines up. The band enables the
**full vim grammar** on the command text:

- When expanded, the `*command-line*` buffer switches its major mode
  to **`command-line-expand-mode`** (a dedicated tier-2 mode with
  isolated option overrides — line numbers, sign column, and wrap
  are all off, scoped to the band only, never leaking to the
  document pane). Collapsing reactivates `command-line-mode`.
- Normal / Insert / Visual modal editing
- Motions, operators, registers, undo, multi-line editing
- `<CR>` inserts a newline (multi-line commands)
- `<C-x><C-e>` again (or from Normal mode) collapses the band back
  to the one-row line **without executing** — review, then `<CR>` to
  run. `<C-c>` cancels (discards expanded edits).
- `:` is a no-op inside the band (already in the command line)

The expanded band height is controlled by `command-line.expand-height`
(`:set command-line.expand-height=full` or `=10` for 10 rows; default
is `half`).

## Command history

The `:` line keeps a history ring (`command_history`, in-memory,
oldest-first, cap 100, dedup on consecutive identical). Two surfaces:

- **Walk:** `<C-p>`/`<C-n>` in the `:` line. The first `<C-p>`
  saves your in-progress text; `<C-n>` walks back down to it.
- **Picker:** `q:` (Normal chord) or `:history` or `:history commands`
  opens a fuzzy picker over command history, newest first. `<CR>`
  loads the chosen command into the `:` line **without executing** —
  tweak it (or expand), then `<CR>` to run. From inside the expanded
  band, `q:` seeds the filter with whatever you've typed so far.

## The `/` `?` search line

The search line works the same way: a buffer-backed readline surface
(`*search-line*` with `search-line-mode` major).

| Chord | Action |
|---|---|
| `/` | Search forward (empty) |
| `?` | Search backward (empty) |
| `<CR>` | Submit — jump to first match |
| `<Esc>` / `<C-c>` | Cancel — restore cursor to pre-search position |
| `<C-p>` / `<Up>` | Previous search history entry |
| `<C-n>` / `<Down>` | Next search history entry |
| `<C-x><C-e>` | Expand into full-modal band |

Typing in the search line shows **live match highlighting**
(incsearch): matches are painted on the document as you type. `<CR>`
jumps to the first match; `<Esc>` cancels and returns you to where
you started.

## Search history

Search terms are stored in a separate `search_history` ring (same
dedup and cap as command history). Browse via:

- **Walk:** `<C-p>`/`<C-n>` in the `/` / `?` line.
- **Picker:** `q/` `q?` (Normal chords) or `:history searches`
  opens a fuzzy picker over search history. `<CR>` loads into the
  `/` line (Forward direction) for editing.

## Completions and live feedback

| Surface | `:` line (tier 1) | Expanded band (tier 2) |
|---|---|---|
| Completion popup | `<Tab>` / `<S-Tab>` | `<Tab>` / `<S-Tab>` |
| Live error indicator | Trailing echo text | (planned) |
| Parameter hints | Dim trailing text | (planned) |
| Syntax highlighting | Full command-line grammar | First line |
| `:s///` preview | Live on target buffer | (planned) |
| Registers / undo / Visual | — (insert-only) | Full modal grammar |

## Options

- `command-line.expand-height` — `half` (default), `full`, or a
  fixed row count. Controls how tall the expanded band grows.
  `:set command-line.expand-height=10`

## See also

- [`ex-commands`](help:ex-commands) — full ex-command reference.
- [`modal-editing`](help:modal-editing) — the keymap-side of the editor.
- [`options`](help:options) — `:set` + `:customize` deep dive.
