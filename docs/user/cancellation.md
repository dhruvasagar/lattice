---
summary: "Cancelling a running operation with <C-g>: what it stops (project search, LSP commands you asked for), what it deliberately leaves alone (automatic requests, background work), and why <Esc> and <C-c> are not the cancel key."
related: [project-search-mode, lsp, modal-editing, emacs-keys-mode]
---

# Cancelling a running operation

Most of what the editor does finishes before you notice. Some things
don't: a search across a large repository, a rename that touches
hundreds of files, a language server thinking hard about a symbol.

**`<C-g>`** stops whatever you started and puts you back in Normal
mode.

With nothing running it is simply a mode reset — it clears a half-typed
chord, a pending count, a pending register — so pressing it
speculatively is always safe. That is deliberate: a cancel key you have
to think twice about before using is not much of an escape hatch.

It never discards unsaved changes (that's `:q!`) and never clears your
registers or yank ring.

---

## What it stops

| Operation | `<C-g>` |
|---|---|
| Project-wide search ([`project-search-mode`](help:project-search-mode)) | Stops the scan. Results found so far stay in the buffer |
| Refreshing a search with `gr` | Stops the replacement scan |
| Hover (`K`), go-to-definition (`gd` / `gD` / `gy` / `gI`), references (`gr`) | Abandons the request |
| Document + workspace symbols, call hierarchy, type hierarchy | Abandons the request |
| Rename, format, code actions, expand-region | Abandons the request |

A cancelled search leaves what it had already found. That is the useful
behaviour — you stopped it because you had seen enough, not because you
wanted the results thrown away.

## What it deliberately leaves alone

`<C-g>` cancels **work you asked for**. It does not touch work the
editor is doing on its own behalf:

- Completion and signature help while you type
- Symbol highlighting under the cursor, inlay hints, semantic
  highlighting, code lenses, folding ranges
- Background indexing, file watching, auto-save
- A language server still starting up

This is not an omission. Those requests fire on every keystroke and
cursor move, so treating them as cancellable would make `<C-g>`
unpredictable — and, worse, would mean *typing* interfered with the
search you were waiting on.

---

## Starting something new also stops the old

You rarely need `<C-g>` for this. Running a second search abandons the
first automatically, so you can retype a query without waiting, and
`gr` replacing a search stops the one it replaces. A second hover
supersedes the first the same way.

What supersede does *not* do is cross between unrelated things.
Pressing `K` to glance at a symbol while a search is running leaves the
search alone — only `<C-g>` stops both.

---

## Why not `<Esc>`

`<Esc>` does **not** cancel, and that is on purpose.

Most vim users press `<Esc>` constantly and without thinking — to
confirm they're in Normal mode, between edits, out of habit. If it
cancelled, a reflexive double-tap would kill a thirty-second search,
and you would have no way to tell "it finished" from "I stopped it."

A key you press without deliberation should not be able to destroy work
in progress. So cancelling got its own key, and `<Esc>` kept doing
exactly what it always did.

## Why not `<C-c>`

`<C-c>` is vim's interrupt, so it would have been the obvious choice.
It is reserved here as a **prefix**: `<C-c>g` opens magit's dispatch
menu, `<C-c>f` its file dispatch, `<C-c><C-c>` confirms a commit
message, `<C-c><C-k>` aborts one.

A key bound to a command directly can't also be a prefix — the command
fires the moment you press it, and everything underneath becomes
unreachable. Binding `<C-c>` to cancel would have silently deleted all
of those chords.

Individual modes still use `<C-c>` for their own more specific stop:
`compilation-mode` kills the build, an AI conversation interrupts the
turn, and the `:` and `/` lines cancel that line. Those are scoped to
the buffer you're in.

## `<C-g>` in Visual and Select

There, `<C-g>` keeps its vim meaning — it toggles between Visual and
Select while preserving the selection — so those two modes have no
cancel chord. Press `<Esc>` first to drop the selection, then `<C-g>`.

Worth the trade: you are rarely sitting in Visual with a selection
while waiting on a search, and the alternative was taking a
vim-standard chord away.

---

## Notes

**Mid-chord it takes two presses.** With a pending operator (you typed
`d` and stopped), `<C-g>` aborts the operator — the same thing vim does
for any invalid continuation. A second press then cancels. You are
never stuck, just one key slower.

**With emacs-keys off**, `<C-g>` still works. It is a builtin binding,
not part of the [`emacs-keys`](help:emacs-keys-mode) tribute, so `:set
noemacs-keys` doesn't take your cancel key away.

## See also

- [`project-search-mode`](help:project-search-mode) — the search this
  most often stops
- [`lsp`](help:lsp) — language-server requests
- [`modal-editing`](help:modal-editing) — modes, and what `<Esc>` does
- [`emacs-keys-mode`](help:emacs-keys-mode) — the `<C-x>` leader
