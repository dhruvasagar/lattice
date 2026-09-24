---
summary: "picker-yank-ring: pick anything you recently yanked or deleted and insert it where you are — the buffer, the `:` or `/` line, or another picker's query."
related: [yank-ring]
---

# The `yank-ring` picker

Everything you have recently yanked or deleted, newest first, followed
by your registers. `<CR>` **inserts** the pick where you were when you
opened the picker — which is the point of it: the same key gives you
something you copied, wherever you happen to be typing.

Open it with:

| From | Key | The pick goes |
|---|---|---|
| Insert mode | `<C-r><C-r>` | into the buffer, at the cursor |
| inside any picker | `<C-r>` | onto the end of that picker's query (the picker comes back) |
| the `:` line or the `/` line | `<C-r><C-r>` | into that line, at the cursor |
| anywhere | `:picker yank-ring` | wherever something is waiting for it — nowhere, from Normal mode (it says so) |

`<C-r>` followed by a register name (`<C-r>a`) is vim's register insert
and skips the picker.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by **contents** |
| `<CR>` | Insert the text where you were, and close |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close; nothing is inserted. A picker you opened this from comes back as you left it |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` do nothing — the pick is text to insert,
not something to open. `<C-q>` has nothing to send. `<C-l>` and
`<C-d>` do nothing, and `<C-r>` here does not nest a second one.

---

## What is listed

1. **The ring**, newest first: every yank *and* delete, numbered `0`,
   `1`, … with `line` or `char` beside it for what kind of text it was.
2. **Then every register**, named `"a`, `"0`, `"+` and so on.

Rows show the text folded onto one line, as in the
[`registers`](help:picker-registers) picker. The ring keeps
`yank.ring.size` entries (default **50**; `0` turns it off).

**It always inserts plain text.** A linewise yank is inserted at the
cursor, not pasted as a new line; for that, use `p` or the
`registers` picker.

There is no preview and no recency ranking — the ring's own order *is*
recency.

---

## See also

- [`yank-ring`](help:yank-ring) — the ring, its option and its commands.
- [`registers`](help:picker-registers) — paste a register the way `p`
  does.
- [`picker`](help:picker) — keys and options shared by every picker.
