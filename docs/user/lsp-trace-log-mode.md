---
summary: "lsp-trace-log-mode: the LSP protocol trace for one server instance — the requests and responses on the wire. :lsp-trace."
related: [lsp, ex:lsp-trace]
---

# lsp-trace-log-mode

The **protocol trace** for one language server instance: the LSP
requests and responses actually crossing the wire. `:lsp-trace`.

The twin of [`lsp-server-log-mode`](help:lsp-server-log-mode), scoped
to trace records rather than the server's own log messages. Reach for
it when the server *says* it's fine and the editor still isn't getting
what it asked for — the trace shows which request went out and what
came back.

This is the deepest of the three LSP log views, and usually the last
one to try:

| Buffer | Shows | Use when |
|---|---|---|
| [`lsp-log-mode`](help:lsp-log-mode) | subsystem events, all servers | something's wrong, unclear where |
| [`lsp-server-log-mode`](help:lsp-server-log-mode) | one server's own messages | you know which server |
| `lsp-trace-log-mode` | one server's protocol traffic | server looks healthy, behaviour still wrong |

## Behaviour worth knowing

- **It streams**, and the subscription ends when the buffer closes.
- **Read-only**, and an ordinary registry buffer — searchable and
  yankable like any other, which is the point when you want to paste a
  request into a bug report.

## See also

- [`lsp`](help:lsp) — the subsystem overview.
