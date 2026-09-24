---
summary: "picker-marks: every mark you have set, by name; <CR> jumps to it like a backtick."
related: [marks]
---

# The `marks` picker

The marks you have set with `m{a-z}`, `m{A-Z}` and friends, in mark
order. `<CR>` jumps to the exact position — the same as pressing
`` ` `` and the mark's name.

Open it with `:picker marks`. `:marks` is not this: it prints the list
as text.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by mark name |
| `<CR>` | Jump to the mark's exact line and column (`` `a ``) |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>` — a mark is a position,
not a file to open. `<C-q>` has no location to send and says so.
`<C-l>` and `<C-d>` do nothing.

---

## What is listed

`'a`, `'b`, … sorted by name, with the mark's `line:col` on the right.
There is no preview: moving the selection does not move the view.

Jumping records where you were, so `<C-o>` brings you back. Marks you
jump to often rank a little higher next time.

---

## See also

- [`jumps`](help:picker-jumps) — every position you jumped from, not
  just the named ones.
- [`modal-editing`](help:modal-editing) — setting and using marks.
- [`picker`](help:picker) — keys and options shared by every picker.
