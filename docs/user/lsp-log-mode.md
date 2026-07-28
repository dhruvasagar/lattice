---
summary: "lsp-log-mode: the subsystem-wide *lsp* buffer — every LSP event across every server, streamed live. :lsp-log."
related: [lsp, ex:lsp-log]
---

# lsp-log-mode

The `*lsp*` buffer: what the LSP subsystem is doing, across **every**
server. `:lsp-log`.

This is the first place to look when a language feature isn't working
and you don't yet know whose fault it is — the server didn't start, the
attach didn't happen, the request errored. Records stream in live while
the buffer is open.

For one server's own output, use
[`lsp-server-log-mode`](help:lsp-server-log-mode); for the raw protocol
traffic, [`lsp-trace-log-mode`](help:lsp-trace-log-mode).

## Behaviour worth knowing

- **It streams.** The mode subscribes when the buffer opens and appends
  records as they arrive — you don't refresh it.
- **The subscription ends with the buffer.** Closing it unsubscribes,
  so a log buffer you're not looking at costs nothing.
- **Read-only**, and an ordinary registry buffer: `/` to search, motions
  to walk it, `:ls` and the buffer picker to find it again.

## See also

- [`lsp`](help:lsp) — the subsystem: servers, attach lifecycle,
  capabilities, and every `:lsp-*` command.
- [`lsp-mode`](help:lsp-mode) — the per-buffer gate that decides
  whether LSP runs at all.
- [`messages-mode`](help:messages-mode) — the editor-wide log, for
  events that aren't LSP's.
