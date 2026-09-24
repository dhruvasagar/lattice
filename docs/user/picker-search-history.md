---
summary: "picker-search-history: every `/` pattern you have searched, newest first; <CR> puts it back on the search line to edit — it does not search."
related: [search-history]
---

# The `search-history` picker

Every pattern you have searched for, newest first. `<CR>` loads the
one you pick into the `/` line **without searching** — adjust it, then
`<CR>`.

Open it with:

- `q/` or `q?` in Normal mode, or
- `:history searches`, or `:picker search-history`.

**If the search line is already open**, the picker starts filtered by
the pattern you had typed.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter, fuzzily |
| `<CR>` | Put the pattern on the `/` line to edit — **not searched yet** |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>`. `<C-q>` has no location
to send. `<C-l>` and `<C-d>` do nothing.

---

## What is listed

The raw patterns, newest first, the last **100**. Blank patterns and
back-to-back repeats are not recorded.

**The line always opens searching forward**, even from `q?`. To search
backward with a recalled pattern, `<Esc>` the line and use `?` then
`<Up>`.

---

## See also

- [`history`](help:picker-history) — the same for `:` commands.
- [`picker`](help:picker#the-four-history-sources) — the four history
  pickers side by side.
