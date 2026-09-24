# The `project-buffers` picker

The buffers you have open **in the current project** — like `:b`, with
everything from other projects left out.

Open it with `:project-buffers`, `<leader>pb` or `<C-x>pb`, or from the
project menu's `b`.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by file name |
| `<CR>` | Switch to that buffer |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Show it in a new horizontal split / vertical split / tab instead |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-q>` has no location to send. `<C-l>` and `<C-d>` do nothing
(`:bd` closes a buffer).

---

## What is listed

Each row is the file name, with its directory beside it, `[+]` on a
buffer with unsaved changes and `(current)` on the one you are in —
which is listed last, so the first row is somewhere else. The prompt
names the project.

---

## See also

- [`buffers`](help:picker-buffers) — every open buffer, any project.
- [`projects`](help:project.picker-projects) — switch project first.
