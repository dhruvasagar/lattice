---
summary: "ai-log-mode: one agent process's log — *ai:<provider>:<index>*, streamed live. Where to look when an agent won't start or dies mid-turn."
related: [ai, agent, ex:ai-log]
---

# ai-log-mode

The log for one agent process: `*ai:<provider>:<index>*`. `:ai-log`.

Where to look when an agent isn't behaving — it didn't start, it died
mid-turn, or the conversation stopped receiving anything. The
[conversation buffer](help:ai-conversation-mode) shows what the agent
*said*; this shows what the process *did*.

The buffer name carries the provider and which instance, so several
agents running at once each get their own log rather than one
interleaved stream.

## Behaviour worth knowing

- **It streams.** The mode subscribes on open and appends records as
  they arrive; no refresh needed.
- **The subscription ends with the buffer** — closing it unsubscribes,
  so a log you're not reading costs nothing.
- **Read-only**, and an ordinary registry buffer: searchable, yankable,
  reachable through `:ls` and the buffer picker.

This is the same shape as [`lsp-server-log-mode`](help:lsp-server-log-mode)
— per-process log buffers work identically across subsystems, so what
you learn about one applies to the others.

## See also

- [`ai-conversation-mode`](help:ai-conversation-mode) — the
  conversation itself.
- [`messages-mode`](help:messages-mode) — the editor-wide log.
