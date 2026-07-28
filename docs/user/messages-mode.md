---
summary: "messages-mode: the *messages* audit log — every echo-area message lattice has shown, kept in a real buffer you can search, yank, and scroll back through."
related: [messages, ex:messages]
---

# messages-mode

The `*messages*` buffer: a running log of everything lattice has told
you in the echo area. Open it with `:messages`.

The echo area shows one line and then it's gone. This is where those
lines go, so "what did that error say?" has an answer after the message
has scrolled past.

It is an ordinary buffer, which is the point — search it with `/`, yank
from it, walk it with any motion, split it alongside your work. There
is no separate message-viewer UI to learn.

## What lands here

Anything at `info` level or above: LSP servers attaching, a macro
recording stopping, `:q` refusing to close a dirty buffer, errors from
any subsystem. Per-keystroke and per-frame diagnostics go to the debug
log instead — they'd flood this buffer at 30 Hz and bury the events you
actually want.

Run lattice with `--log-level debug` to see those too, on stderr.

## Behaviour worth knowing

- **Read-only from your side.** Typing won't edit it; the subsystems
  that write to it bypass the read-only gate by construction, not by a
  capability you can grant yourself.
- **The major mode is `messages-mode`, not `text-mode` + read-only.**
  The buffer has its own identity so the editor can treat it as the
  log it is — the same arrangement `*lsp*` has with
  [`lsp-log-mode`](help:lsp-log-mode).
- **It's a real registry buffer**, so `:ls`, `:bn` / `:bp`, and the
  buffer picker all reach it.

## See also

- [`lsp-log-mode`](help:lsp-log-mode) — the LSP subsystem's own log.
- [`help`](help:help) — the introspection commands, for when the
  question is "what does this do" rather than "what just happened".
