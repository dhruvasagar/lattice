---
summary: "picker-recent: the files you have edited this session, newest first — `:recent`."
related: [recent, ex:recent]
---

# The `recent` picker

The files you have opened in this session, most recent first. `<CR>`
opens the one you pick.

Open it with `:recent` or `:picker recent`, or from the
[dashboard](help:dashboard-mode). It takes no arguments.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by path, fuzzily; space separates fragments |
| `<CR>` | Open the file (or switch to it if it is already open) |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Open it in a horizontal split / vertical split / new tab instead |
| **`<C-q>`** | Send every file still listed to the error list, then close |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close without opening anything |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here.

---

## What is listed

Absolute paths, newest first, with the same three columns as
[`files`](help:picker-files): permissions, size and last-modified time.
A file that has since been deleted is still listed, with blank columns.

A file joins the list whenever you open it, reload it or switch back to
it. Duplicates collapse to the newest position, and the list keeps the
last **50**.

The list lives for the session: it is **not** saved when you quit. For
"files I use a lot, across sessions", the `files` picker's recency
ranking is the thing that persists.

---

## Preview and ranking

Moving the selection previews the file in the active pane, without
opening it. On top of the list's own newest-first order, files you
pick from here often get a small frecency bonus — see
[MRU ranking](help:picker#mru-ranking).

---

## See also

- [`files`](help:picker-files) — every file in the project.
- [`buffers`](help:picker-buffers) — what is open *now*, rather than
  what you opened.
- [`picker`](help:picker) — keys and options shared by every picker.
