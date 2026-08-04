---
summary: "First steps: the modal loop, opening and saving files, the command line, splits, and where to go next."
related: [edit, write, quit, help, tutor]
---

# Getting started

Lattice is a modal editor: the keys you press mean different things
depending on the **mode** you're in. If you've used vim, everything
here is familiar. If you haven't, this page is the ten-minute version
that gets you editing; [`modal-editing`](help:modal-editing) is the
full treatment, and [`:tutor`](help:tutor-mode) teaches it hands-on.

> **New here?** The fastest way to learn the grammar is the
> interactive tutor: type `:tutor` and press `<CR>`. This page is the
> quick orientation; the tutor is the practice.

---

## Quick reference

| You want to…            | Do this                                  |
|-------------------------|------------------------------------------|
| Type text               | `i` (enter Insert mode), then type       |
| Stop typing             | `<Esc>` (back to Normal mode)            |
| Move around             | `h` `j` `k` `l` — left, down, up, right  |
| Open a file             | `:e <path>` `<CR>`                        |
| Find a file by name     | `:files` `<CR>`, then type to filter     |
| Save                    | `:w` `<CR>`                              |
| Save and quit           | `:x` `<CR>`  (or `:wq`)                   |
| Quit (discard)          | `:q!` `<CR>`                             |
| Run a command           | `:` opens the command line               |
| Get help                | `:help` `<CR>`, or `:help <topic>`       |
| Learn interactively     | `:tutor` `<CR>`                          |

---

## The modal loop

You spend most of your time in **Normal mode**. Normal mode is not
for typing text — it's for *navigating and operating on* text. Keys
are commands: `w` jumps a word forward, `dd` deletes a line, `gg`
goes to the top of the file.

To insert text, switch to **Insert mode**:

- `i` — insert before the cursor
- `a` — insert after the cursor
- `o` — open a new line below and insert

Type what you want, then press `<Esc>` to return to Normal mode. The
modeline (bottom of the pane) shows a tag for the current mode so you
always know where you are.

This is the whole rhythm: **Normal to move and operate, Insert to
type, `<Esc>` to come back.** The power comes from Normal mode's
grammar — operators (`d`elete, `c`hange, `y`ank) compose with motions
(`w`ord, `$` end-of-line) and text objects (`i`nner `p`aragraph) so
`daw` means "delete a word" and `ci"` means "change inside quotes."
That grammar is the subject of [`modal-editing`](help:modal-editing).

---

## Opening and saving files

The command line is how you talk to the editor beyond single-key
grammar. Press `:` — the cursor drops to a prompt at the bottom of the
screen — type a command, and press `<CR>`.

```text
:e src/main.rs      open (edit) a file by path
:w                  write the current buffer to its file
:w other.txt        write to a different path
:x                  write, then quit (":wq" is the same)
:q                  quit — refuses if there are unsaved changes
:q!                 quit and throw away unsaved changes
```

`<Tab>` completes paths and command names while you type, so
`:e src/ma<Tab>` fills in the rest. When you don't know the file's
exact path, the **fuzzy file picker** is faster: `:files` opens it,
then you type a few letters of the name and press `<CR>` on the match.
The picker does much more (grep, open buffers, symbols) — see
[`picker`](help:picker).

Every open file is a **buffer**. `:ls` lists them, `:bn` / `:bp`
cycle between them, and `:b <name>` jumps to one by name. Buffers,
splits, and window management are covered in [`buffers`](help:buffers).

---

## Working in more than one pane

Split the screen to see two things at once:

- `:sp` (or `<C-w>s`) — split horizontally (new pane below)
- `:vsp` (or `<C-w>v`) — split vertically (new pane beside)
- `<C-w>h` `<C-w>j` `<C-w>k` `<C-w>l` — move focus between panes
- `:only` (or `<C-w>o`) — close every pane but the focused one

Panes, the file tree, and the `<C-w>` family are all in
[`buffers`](help:buffers).

---

## Making it yours

Options are set with `:set`, using vim's names where they exist:

```text
:set number         show line numbers
:set wrap           soft-wrap long lines
:set tabstop=2      a tab is two columns wide
```

`:set <Tab>` completes option names, and `:options` opens a live,
searchable reference of every option with its current value. The full
model — layered defaults, TOML config, per-language overrides — is in
[`options`](help:options).

To change the colour theme, `:colorscheme` with no argument opens a
live-preview picker (arrow through themes and watch the editor
recolour); `:colorscheme <name>` sets one directly. See
[`themes`](help:themes).

---

## Finding your way around

Lattice is self-documenting — you rarely need to leave the editor to
learn it:

| Command             | Answers                                    |
|---------------------|--------------------------------------------|
| `:help`             | Opens this help system's topic index       |
| `:help <topic>`     | Opens a specific topic (e.g. `:help folding`) |
| `:describe-key`     | "What does *this* key do?" — then press it |
| `:describe-command` | What a `:command` does, with cross-links   |
| `:describe-bindings` | Every key binding that works *here* (`<C-h>K`) |
| `:keymap`           | Every default binding in every mode        |
| `:describe-active-modes` | What modes are live in this buffer (`<C-h>m`) |
| `:apropos <word>`   | Search commands, options, and topics by keyword |

`:help <Tab>` lists every registered topic. When in doubt,
`:apropos` searches everything.

---

## Where to go next

- [`:tutor`](help:tutor-mode) — the interactive lessons. Start here.
- [`modal-editing`](help:modal-editing) — the vim grammar in full:
  motions, operators, text objects, registers, marks, macros.
- [`modes`](help:modes) — major and minor modes, and how
  `:<mode-name>` toggles work.
- [`ex-commands`](help:ex-commands) — the `:` command language:
  ranges, `:s`ubstitute, `:g`lobal, argument schemas.
- [`buffers`](help:buffers) — buffers, splits, panes, the file tree.
- [`options`](help:options) — configuration and the `:set` model.
- [`help`](help:help) — the introspection commands (`:describe-*`,
  `:apropos`, `:keymap`) in depth.
