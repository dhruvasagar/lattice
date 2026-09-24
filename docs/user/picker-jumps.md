---
summary: "picker-jumps: the position history — jump list and mark ring in one — newest first; <CR> jumps back to any point."
related: [jumps]
---

# The `jumps` picker

Everywhere the cursor has jumped from, newest first — the ring
`<C-o>` and `<C-i>` walk one step at a time, laid out so you can go
straight to any entry.

Open it with `:picker jumps`. It takes no arguments.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by file name or buffer title, fuzzily |
| `<CR>` | Jump there — switching buffer if needed |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Jump there in a new horizontal split / vertical split / tab |
| **`<C-q>`** | Send every entry still listed to the error list, then close. Entries in buffers with no file on disk are left out |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection — each entry is previewed in place |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close; the cursor stays where it was |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here.

---

## What is listed

One row per recorded position: the file (or the buffer's title, or
`#<id>` for a buffer since closed), then two columns — **how it got
there** and `line:col`:

| Source column | Recorded by |
|---|---|
| `auto` | a big motion — `gg`, `G`, a search, `*`, `#`, `%`, a mark jump |
| `plugin` | a plugin, or a jump made from this picker |
| `'a` … | setting a named mark with `m` |

The ring keeps the last **100** positions. Jumping from here records
where you were, so `<C-o>` still brings you back.

---

## Preview

Moving the selection shows the entry's buffer in the active pane,
centred on the line, without switching to it.

---

## See also

- [`modal-editing`](help:modal-editing) — the jump list and `<C-o>` /
  `<C-i>`.
- [`marks`](help:picker-marks) — just the named marks.
- [`picker`](help:picker) — keys and options shared by every picker.
