---
summary: "picker-grep: live project-wide text search — hits re-run as you type, <CR> jumps to one, <C-q> sends them all to the error list."
related: [grep]
---

# The `grep` picker

Search file contents across the project, **live**: the search re-runs
as you type (after a 150 ms pause) and the hits replace the list.
`<CR>` jumps to the one you pick.

Open it with `:picker grep`, or `:picker grep <pattern>` to start with
a pattern already typed (only its first word — the argument is split on
spaces; type the rest into the prompt). The prompt shows the project
root being searched.

For a persistent, editable buffer of *every* match rather than one
jump, use [`:search`](help:project-search-mode).

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | The search pattern. Each pause re-runs the search; an empty pattern clears the list |
| `<CR>` | Jump to the hit — file, line and column |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Open the hit in a horizontal split / vertical split / new tab instead |
| **`<C-q>`** | Send **every hit still listed** to the error list, then close — then `:cn` / `:cp` walk them. The fastest way from "search" to "fix each one" |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection (and the preview) |
| `<BS>` | Delete a character of the pattern (the search re-runs) |
| **`<C-w>`** | Delete the previous **word** of the pattern. Never "up a directory": a pattern containing `/` loses one word, not everything back to the slash |
| `<C-r>` | Append something from your yank history to the pattern — paste the identifier you just yanked |
| `<Esc>` / `<C-c>` | Close; you stay where you were |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here.

---

## The pattern is the backend's

Unlike every other picker, the query is **not** fuzzy-matched by the
editor. It is handed to the search program as its pattern, and the rows
are exactly what the program prints. So:

- It is a **regular expression** in that program's dialect.
- Case sensitivity, and whether `.gitignore` and hidden files are
  respected, are the program's defaults. `rg` and `ag` respect ignore
  files and skip hidden files; plain `grep -r` does neither.
- A pattern that **starts with `-`** is read by the program as a flag.
  Wrap the dash in a class — `[-]foo` — which every backend reads as a
  literal `-`.

Which program runs is `picker.grep.backend`:

| Value | Runs |
|---|---|
| `auto` *(default)* | the first of `rg`, `ag`, `grep` found on your `PATH` |
| `rg` / `ag` / `grep` / *any name* | that program; it must be on your `PATH` or the picker says so, naming the option |

Only `rg` and `ag` report a column, so with `grep` every hit jumps to
the start of its line.

---

## What is listed

Each row is the matching line (leading whitespace trimmed,
syntax-highlighted), with `path:line:col` on the right. At most
`picker.grep.max-hits` hits are kept (default **2000**) — narrow the
pattern if you hit it.

The search runs off the UI thread, so typing never waits on it; a
slower search just means the list catches up a moment later.

---

## Preview

Moving the selection shows the hit's file in the active pane, centred
on the matching line, without opening it. `<Esc>` puts back what you
had.

---

## Options

| Option | Default | Meaning |
|---|---|---|
| `picker.grep.backend` | `auto` | Which search program to run (above) |
| `picker.grep.max-hits` | `2000` | Most hits kept per search |

Both are read on every search, so `:set picker.grep.backend=grep`
takes effect on the next keystroke.

---

## See also

- [`:search`](help:project-search-mode) — all matches, as an editable
  multibuffer.
- [`lines`](help:picker-lines) — search just the current buffer.
- [`error-list`](help:error-list) — where `<C-q>` sends the hits.
- [`picker`](help:picker) — keys and options shared by every picker.
