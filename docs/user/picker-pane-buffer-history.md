---
summary: "picker-pane-buffer-history: the buffers THIS pane has shown, newest first; <CR> walks back to one without losing the way forward."
related: [pane-buffer-history]
---

# The `pane-buffer-history` picker

The trail of buffers the **active pane** has displayed, newest first —
the list `<C-6>` and `<C-7>` walk one step at a time. `<CR>` walks the
pane straight to the entry you pick.

Open it with `:history pane-buffers` or
`:picker pane-buffer-history`. There is no key for the picker itself;
`<C-6>` / `<C-7>` walk the trail without it.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by buffer name |
| `<CR>` | **Walk** this pane to that entry, restoring its cursor and scroll |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close; the pane stays where it is |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>` — the trail belongs to
this pane, so the walk happens here. `<C-q>` has no location to send.
`<C-l>` and `<C-d>` do nothing.

---

## A walk, not a visit

`<CR>` **moves** your position in the trail; it does not add a new
entry. Everything newer stays reachable: walk back three buffers from
the picker, and `<C-7>` still steps forward through the three you
skipped. (Opening a buffer the ordinary way *does* add an entry, and
drops the forward part — like a browser's history.)

---

## What is listed

Each row is `* name:line` — the `*` marks where you are in the trail,
and the line is where the cursor was when you left that buffer. Only
the active pane's trail is shown; every pane has its own. It keeps
`pane.buffer-history-size` entries (default **100**).

No preview, and no recency ranking — the trail's order *is* recency.

---

## See also

- [`buffers`](help:buffers) — the pane trail and `<C-6>` / `<C-7>`.
- [`jumps`](help:picker-jumps) — where the *cursor* has been, across
  buffers: a cursor trail, not a buffer trail.
- [`picker`](help:picker#the-four-history-sources) — the four history
  pickers side by side.
