---
summary: "picker-commands: the command palette — every ex-command with its key, arguments and doc; <CR> runs it or opens the `:` line for its arguments."
related: [commands]
---

# The `commands` picker

Every ex-command, searchable by name, with the key it is bound to and
what it does. Pick one to run it — or, if it takes arguments, to start
typing them.

Open it with `:picker commands`, or bind `action:open-command-picker`
to a key of your own; it has no default binding.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by command name, fuzzily — `lspfmt` finds `lsp-format` |
| `<CR>` | Run the command — or, if it takes **any** argument, open the `:` line with the name filled in and the first argument's prompt, so nothing runs until you press `<CR>` again |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close; nothing runs |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>`. `<C-q>` has no location
to send and says so. `<C-l>` and `<C-d>` do nothing.

---

## What is listed

Every registered ex-command, alphabetically, **including plugin
commands** — the list is re-read each time it opens. Each row is the
name you would type after `:`, and the columns on the right are:

| Column | Shows |
|---|---|
| key | the chord bound to it, if any — only bindings that are live in this buffer (always-on ones, or ones from a mode that is active here) |
| mode | which mode that chord comes from |
| args | its arguments: `<name>` required, `[<name>]` optional |
| doc | the first line of its documentation |
| latency | how fast it is expected to answer |

So the palette also answers "is this bound to a key here?" and "what
does `:lsp-rename` want?". The filter matches names only; to go from a
key to its command, use `<C-h>k`.

Commands you run often rank a little higher next time. There is no
preview.

---

## See also

- [`history`](help:picker-history) — commands you have already run.
- [`ex-commands`](help:ex-commands) — the full reference.
- [`picker`](help:picker) — keys and options shared by every picker.
