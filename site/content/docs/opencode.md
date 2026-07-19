+++
title = "opencode agent"
+++



`:opencode` runs the **opencode** coding agent's native terminal UI in a
lattice terminal buffer. You get opencode's complete interface — its
prompt with line editing, `/` commands, model switching, session
management, history, and its own diff-based edit review — running inside
lattice, with a thin `opencode-mode` layered on top for lattice-side
navigation. This is the same shape as the [Claude Code](claude-code)
integration: the agent runs its own TUI in a terminal buffer, and lattice
provides the surrounding editor.

Why this shape: opencode is a terminal-native agent, and its TUI *is* the
product — readline-style input, a model picker, `/`-command autocomplete,
snapshot undo. Running that real TUI gives you all of it for free, exactly
as its authors intended, instead of lattice reimplementing a weaker copy.

> **Also available:** `:opencode-acp` drives opencode *headlessly* over the
> Agent Client Protocol and renders the conversation as a lattice **buffer**
> with a modal prompt, and — its one distinguishing feature — opens the
> agent's proposed edits in **lattice's own diff view** (`]c`/`do`/`dp`,
> your theme) rather than opencode's. It's the buffer-native alternative,
> kept for people who want edits reviewed in lattice; see
> [The ACP buffer alternative](#the-acp-buffer-alternative). For v1 the
> terminal `:opencode` above is the recommended experience.

---

## Quick reference

| Command / key | Behavior |
|---|---|
| `:opencode` | Launch opencode's TUI in a terminal buffer (opencode-mode active) |
| *(in the terminal, Insert)* | Every key goes to opencode — type prompts, run `/` commands, drive its UI as normal |
| `<Esc>` | Drop to Normal-in-terminal to scroll / search the scrollback with vim motions |
| `i` / `a` | Return to Insert (forward keys to opencode again) |
| `:opencode-acp` | The alternative: headless ACP + a lattice conversation buffer with lattice-owned diff review |

---

## Quick start

```
:opencode
```

This opens a terminal buffer running `opencode` in the current directory
and activates `opencode-mode` on it. From here you're in opencode's own
interface — type a prompt and press Enter, run `/models` to switch models,
`/undo` to roll back, and so on. Everything opencode's TUI does works,
because it *is* opencode's TUI.

Because the agent runs in a lattice **terminal buffer**, it's a first-class
buffer: it shows up in `:ls`, you can split it alongside your code, and you
can switch to Normal-in-terminal to scroll back through the conversation
with `j`/`k`, `<C-u>`/`<C-d>`, and `/` search — then drop back into Insert
to keep talking to the agent.

---

## Driving opencode

opencode's TUI owns the interaction, so its own documentation is the
reference for prompts, commands, and shortcuts. The lattice-specific part
is just the two-mode terminal wrapper (`opencode-mode` over
[`terminal-mode`](terminal)):

- **Insert-in-terminal** (the default when you open it) forwards every
  keystroke straight to opencode — including `<C-c>`, arrow keys, and `/`.
  This is where you do all your work with the agent.
- **`<Esc>`** switches to **Normal-in-terminal**: the keystrokes stop going
  to opencode and instead drive lattice — vim motions over the scrollback,
  `/` to search it, `y` to yank. Press `i` or `a` to hand keys back to
  opencode.

That's the whole lattice surface for v1. There are no lattice keybindings
competing with opencode's — in Insert, opencode has the keyboard.

### Models, `/` commands, and configuration

All of these are **opencode's**, not lattice's:

- **Switch models** with opencode's own `/models` picker (or whatever it
  binds), and set your default model / provider in opencode's config
  (`~/.config/opencode/`).
- **`/` commands** (sessions, undo, help, …) are opencode's — type them at
  its prompt as you would in any terminal.
- **Line editing / history** come from opencode's prompt (`<C-w>`, `<C-u>`,
  up-arrow history, etc.), because a real program is driving the PTY.

Lattice deliberately doesn't wrap or shadow any of it.

---

## Reviewing edits

In the terminal `:opencode`, **opencode reviews its own edits** — its
Plan/Build flow, per-step approval, and snapshot-based `/undo`, shown in
its TUI. Lattice does not intercept the edits for v1; the agent applies
them to disk and lattice picks up the changes like any external edit
(reload with `:e` if a buffer you have open changed underneath you).

If you specifically want the agent's edits to open in **lattice's** diff
view instead, that's exactly what `:opencode-acp` is for — see below. A
native, deeper edit-review integration for the terminal path (lattice's
diff view while opencode's TUI runs) is a planned follow-up.

---

## The ACP buffer alternative

`:opencode-acp` is a different topology for the same agent: lattice drives
`opencode acp` **headlessly** (no opencode TUI) and renders the conversation
as a lattice buffer, `*ai:opencode*`.

| | `:opencode` (terminal) | `:opencode-acp` (buffer) |
|---|---|---|
| Interface | opencode's native TUI | a lattice Document (Normal scrollback, Insert-mode prompt) |
| Prompt line editing, `/` commands, model picker | ✅ native | ✗ (would be lattice's to build) |
| Edit review | opencode's TUI | **lattice's diff view** (`]c`/`do`/`dp`, accept/reject) |
| Interrupt | opencode's `<C-c>` | `<C-c>` (Insert) → `session/cancel` |
| Trust vs review | opencode's Plan/Build | `<C-t>` toggles a lattice review/trust gate |

Use `:opencode-acp` when reviewing the agent's edits **in lattice's own
diff UI** matters more to you than opencode's full TUI. It's the earlier
buffer-native design, kept intact; the terminal path is the recommended
default because it gives opencode's complete experience for free.

---

## Lifecycle and security

- **`:opencode`** starts `opencode` as a child process in a terminal buffer;
  closing the terminal ends it (opencode's own `/exit` works too).
- **The agent runs as a normal child process** in the terminal — no network
  server is opened by lattice for this path, and no third-party code runs
  inside lattice's address space.
- **Edit approval is opencode's** in the terminal path (its Plan/Build +
  `/undo`); if you want lattice to gate edits behind its own diff review,
  use `:opencode-acp`.

---

## Related

- [`claude-code.md`](claude-code) — the other terminal-topology agent
  (the `claude` CLI as an IDE peer over WebSocket/MCP).
- [`terminal.md`](terminal) — the terminal buffer opencode runs in, and
  the Normal/Insert-in-terminal model `opencode-mode` builds on.
- [`diff.md`](diff) — the side-by-side diff UI `:opencode-acp` uses to
  review edits.
- [`docs/dev/architecture/agent-integration.md`](../dev/architecture/agent-integration)
  — why the two agents (and the two opencode topologies) share one design.
