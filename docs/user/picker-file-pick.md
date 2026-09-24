---
summary: "picker-file-pick: choose a file and hand its path to whatever asked — the `files` list, as a value instead of an open."
related: [file-pick]
---

# The `file-pick` picker

The same list as [`files`](help:picker-files), with one difference:
**`<CR>` does not open the file.** It hands the file's path back to
whatever asked for one — a transient menu argument (magit's
file-scoped rows), a command argument, the `:` line.

You rarely open it yourself. Opened bare with `:picker file-pick
[root]`, nothing is waiting for a path, and `<CR>` says so.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by path, fuzzily; space separates fragments |
| `<CR>` | **Supply** the path (relative to the root) to the caller, and close |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Give up — nothing is supplied |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` and `<C-q>` do nothing here — the pick is a
value, not a file to open, so there is nowhere for a split to put it and
nothing to send to the error list. `<C-l>` and `<C-d>` do nothing
either.

---

## What is listed

Exactly what `files` lists, walked the same way: hidden entries,
`.git`, `target`, `node_modules`, `dist` and `.cache` skipped;
`.gitignore` not read; regular files only; at most 5000. The path you
get back is **relative to the root**, as it is displayed.

No columns, no preview, and no recency ranking — the caller decides
what the path is for, so "what you opened last" is not a useful order.

---

## See also

- [`files`](help:picker-files) — the same list, opening the file.
- [`dir-pick`](help:picker-dir-pick) — the directory peer.
- [`picker`](help:picker) — keys and options shared by every picker.
