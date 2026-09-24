---
summary: "picker-lsp-instances: choose which running language server's log or protocol trace to open, when more than one matches."
related: [lsp-log, lsp-trace-log, lsp-server-log]
---

# The LSP server picker

`:lsp-log <server>`, `:lsp-server-log` and `:lsp-trace-log [server]`
open one server's log. When **more than one** running server matches —
two workspaces each with `rust-analyzer`, say — this picker asks which.
One match opens straight away; none says so and lists what is running.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by server name or workspace path |
| `<CR>` | Open that server's log (or protocol trace, for `:lsp-trace-log`) |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-q>` has no location to send. `<C-l>` and `<C-d>` do nothing, and
there is no preview.

---

## What is listed

One row per running server and workspace: the server id, its workspace
root, how many buffers it is attached to, and a summary of what it can
do. The name you passed (`:lsp-log rust`) narrows the list before it
opens.

---

## See also

- [`lsp-server-log-mode`](help:lsp-server-log-mode) and
  [`lsp-trace-log-mode`](help:lsp-trace-log-mode) — the buffers this
  opens.
- [`lsp`](help:lsp) — the language-server integration.
