---
summary: "picker-snippets: browse the snippets for this buffer's language and expand one at the cursor."
related: [snippets]
---

# The `snippets` picker

The snippets available for the buffer you are in, by trigger prefix.
`<CR>` expands the one you pick at the cursor, and you land in its first
tab stop.

Open it with `:picker snippets`. It takes no arguments. (Expanding a
snippet by its prefix while typing is `<C-x><C-s>` — see
[`snippet-mode`](help:snippet-mode).)

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by trigger prefix |
| `<CR>` | Expand the snippet at the cursor |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close; nothing is inserted |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>`. `<C-q>` has no location
to send. `<C-l>` and `<C-d>` do nothing.

---

## What is listed

Snippets for the buffer's language (or the `plain` set, for a buffer
with none), sorted by prefix. Each row is the prefix; the snippet's
name and description are on the right.

Snippets you expand often rank a little higher next time. There is no
preview.

---

## See also

- [`snippet-mode`](help:snippet-mode) — writing snippets, and
  `:reload-snippets`.
- [`active-snippet-mode`](help:active-snippet-mode) — moving between tab
  stops once expanded.
- [`picker`](help:picker) — keys and options shared by every picker.
