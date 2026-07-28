---
summary: "pi-mode: marks the terminal buffer running the pi agent. :pi launches it; pi's own TUI owns the conversation, so lattice adds nothing on the hot path."
related: [ai, agent, pi, ex:pi]
---

# pi-mode

Marks the [terminal buffer](help:terminal-mode) running the **pi**
agent. `:pi` launches it.

pi ships its own TUI, and lattice deliberately doesn't compete with it.
The conversation, the prompt (readline, `/` commands, model switching),
history, and the session tree are all pi's — inside the terminal. This
mode adds nothing on top, which is exactly why the integration feels
like running pi rather than like a lossy reimplementation of it.

What you get from lattice is the terminal being a real buffer: pi runs
in a pane you can split, its scrollback is searchable and yankable via
[`terminal-normal-mode`](help:terminal-normal-mode), and `:ls` finds it
like anything else.

## Behaviour worth knowing

- **It's a marker minor over `terminal-mode`**, activated manually by
  `:pi` on the terminal running the agent. It contributes no chords and
  no options.
- **Its value is identity and a seam.** The buffer knows what it is,
  which is what a future lattice-native integration (an RPC-backed
  conversation buffer, a headerline status row) would attach to.
  [`opencode-mode`](help:opencode-mode) started the same way.

## See also

- [`terminal-mode`](help:terminal-mode) — the buffer pi runs in.
- [`opencode-mode`](help:opencode-mode) — the other terminal-hosted
  agent.
- [`claude-code-mode`](help:claude-code-mode) — the IDE-protocol
  integration, which works differently.
