---
summary: "picker-history: every `:` command you have run, newest first; <CR> puts it back on the `:` line to edit — it does not run it."
related: [history, ex:history]
---

# The `history` picker

Every command line you have run, newest first. `<CR>` loads the one you
pick into the `:` line **without running it** — recall a near miss,
fix it, then `<CR>`.

Open it with:

- `q:` in Normal mode (vim's command-window key), or
- `:history` / `:history commands`, or `:picker history`.

**If the `:` line is already open**, the picker starts filtered by what
you had typed: type `:magit-`, realise you ran the right one yesterday,
press `q:`.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter, fuzzily |
| `<CR>` | Put the command on the `:` line to edit — **unexecuted** |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>`. `<C-q>` has no location
to send. `<C-l>` and `<C-d>` do nothing.

---

## What is listed

The raw text of each command, newest first, the last **100**. A command
repeated back to back is listed once; the same command run at two
different times is listed twice, because the order is the point.

There is no recency *ranking* here on purpose — the list already is
the recency order, and a bonus on top would reshuffle it.

---

## See also

- [`search-history`](help:picker-search-history) — the same for `/`.
- [`commands`](help:picker-commands) — commands you have *not* run yet.
- [`picker`](help:picker#the-four-history-sources) — the four history
  pickers side by side.
