---
summary: "picker-buffers: switch to an open buffer by name — `:b`, `:buffers`, `<C-x>b`."
related: [buffers, ex:buffer-picker]
---

# The buffer picker

Every open buffer — files, the file tree, help pages, terminals,
`*messages*` — filtered as you type. `<CR>` switches the active pane
to the one you pick.

Open it with `:b`, `:buffers`, or `<C-x>b` with
[`emacs-keys-mode`](help:emacs-keys-mode) on. `:ls` is the other
thing: a static text listing, not a picker.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by name, fuzzily; space separates fragments |
| `<CR>` | Switch the active pane to that buffer |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Show it in a new horizontal split / vertical split / tab instead — even if it is the buffer you are on |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close; you stay where you were |
| `<C-h>` | This page |

`<C-q>` sends nothing — a buffer is not a location — and says so.
`<C-d>` does not close buffers; use `:bd`.

---

## What is listed

Each row is `#<id> <name>`, then what kind of buffer it is (`doc`,
`tree`, `help`, `oil`, `term`, `msg`, `mb`, `dash`), with `(current)`
on the buffer you are in. An unsaved, unnamed document carries `[+]`.
Rows are ordered by buffer id (oldest first), buffers you have hidden
(`nobuflisted`) after the others, and **the current buffer last** — so
the picker opens on a buffer other than the one you are in, already
previewed. It is not "the buffer you were just in"; for that, `<C-6>`
walks this pane back one step (see [`buffers`](help:buffers)).

---

## Preview

Moving the selection shows that buffer in the active pane without
switching to it. `<Esc>` puts back what you had.

---

## `:picker buffers`

`:picker buffers` opens the registry version of the same list. It
differs in three small ways: rows show the path (or title) with the
kind, `#id` and a status column (`•` current, `+` modified) on the
right; buffers you pick often float up (see
[MRU ranking](help:picker#mru-ranking)); and it takes no arguments.
Everything else on this page applies to both.

---

## See also

- [`buffers`](help:buffers) — the buffer model: `:bn`, `:bp`, `:bd`,
  listed and hidden buffers.
- [`recent`](help:picker-recent) — files you opened, including closed
  ones.
- [`picker`](help:picker) — keys and options shared by every picker.
