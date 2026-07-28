---
summary: "lsp-server-log-mode: one language server's own log — *lsp:<server>:<workspace>*, scoped to a single instance. :lsp-server-log."
related: [lsp, ex:lsp-server-log]
---

# lsp-server-log-mode

One language server instance's log, in its own buffer:
`*lsp:<server>:<workspace>*`. `:lsp-server-log`.

Where [`lsp-log-mode`](help:lsp-log-mode) shows the whole subsystem,
this shows one server — which is what you want once you know *which*
server is misbehaving, and especially when two workspaces each have
their own instance of the same server and the combined log is
interleaved noise.

The buffer name carries both halves of the identity: the server and the
workspace it's serving. The mode derives that identity by parsing the
name, so each instance's buffer receives only its own records.

## Behaviour worth knowing

- **It streams**, appending records as the server produces them.
- **The subscription ends with the buffer** — closing it unsubscribes.
- **Read-only**, and an ordinary registry buffer.

## See also

- [`lsp-log-mode`](help:lsp-log-mode) — every server at once.
- [`lsp-trace-log-mode`](help:lsp-trace-log-mode) — the protocol
  traffic rather than the server's own messages.
- [`lsp`](help:lsp) — the subsystem overview.
