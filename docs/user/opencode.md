---
summary: ":opencode launches the opencode agent over ACP and opens a conversation buffer — you read the transcript as scrollback in Normal mode and type prompts in Insert mode; the agent edits your code through reviewable diffs you accept or reject."
related: [opencode, ai-prompt, ai-stop, ai-log, ai-conv-toggle-trust]
---

# opencode agent

Lattice drives the **opencode** coding agent as a child process over the
Agent Client Protocol (ACP) and surfaces the whole conversation in a
**buffer** — `*ai:opencode*`. You read the transcript as scrollback with
ordinary vim motions in Normal mode, and you type prompts to the agent in
Insert mode, as if the buffer were a terminal REPL. When the agent wants
to change a file it opens a reviewable side-by-side diff you accept or
reject; nothing is written until you do.

This is the mirror image of the [Claude Code](claude-code.md)
integration. There, the `claude` CLI connects *into* lattice and runs its
own TUI in a terminal buffer. Here, **lattice is the client**: it spawns
`opencode acp`, drives it, and owns the conversation UI itself — so the
agent's output is a normal Document you can search, yank, and fold, not a
terminal grid. Which one to reach for is a matter of which agent you want;
the review-the-diff workflow is the same in both.

> **Status:** the conversation loop — launch, prompt, streamed reply,
> edit-via-diff, accept/reject, interrupt, trust toggle — is implemented.
> The transcript renders the agent's message text, reasoning, and tool
> calls; a few polish items (in-place decoration-based tool-call status, a
> headerline mode indicator, a command-confirmation prompt for non-file
> operations) are tracked follow-ups. The user workflow below is stable.

---

## Quick reference

| Command / key | Behavior |
|---|---|
| `:opencode` | Start the opencode agent and open the `*ai:opencode*` conversation buffer |
| `i` / `a` / `o` / `A` / `I` / `O` (Normal) | Jump into the prompt and enter Insert — you always land in the prompt, never in the transcript |
| `<Enter>` (Insert) | Send the prompt to the agent and clear the input line |
| `<C-c>` (Insert) | Interrupt the active turn — the agent stops, the session stays open |
| `<C-t>` (Normal) | Toggle **trust mode** (auto-accept every edit) vs **review mode** (diff-gated) |
| `:diff-accept` / `:diff-reject` | Resolve a proposed edit (see [Reviewing edits](#reviewing-proposed-edits)) |
| `:ai-prompt <text>` | Send a prompt without the buffer — the headless / scriptable path |
| `:ai-stop` | Stop the agent and end the session |
| `:ai-log [opencode]` | Open the per-session **trace** log (handshake, permissions, errors) |

---

## Quick start

```
:opencode
```

`:opencode` does two things in one step: it launches `opencode acp` as a
child process and opens the `*ai:opencode*` conversation buffer in the
current pane. The buffer is where you talk to the agent.

The buffer behaves like a terminal REPL split across the two vim modes:

- **Normal mode is the scrollback.** The cursor roams the whole
  transcript. Every motion works — `j`/`k`, `<C-d>`/`<C-u>`, `gg`/`G`,
  `/` search, `y` yank. Reading is unrestricted, and nothing you do in
  Normal mode changes the conversation.
- **Insert mode is the prompt.** Pressing `i` (or `a`/`o`/`A`/`I`/`O`)
  drops you into Insert **at the prompt line** — the `> ` at the bottom of
  the buffer — no matter where the cursor was. You can only ever edit the
  prompt; the transcript above it is not user-editable.

Type your message and press `<Enter>` to send it. Your turn appears in the
transcript as `you:`, the agent's reply streams in below as `opencode:`,
and the prompt clears for your next message.

The transcript is a normal read-only Document. It shows up in `:ls`, you
reach it by name with `:b *ai:opencode*`, and it renders through the same
path as any other buffer — so search, folding, and yank all work on the
agent's output.

---

## Talking to the agent

### Sending a prompt

From Normal mode, press `i` to enter the prompt, type, and press
`<Enter>`. Each `<Enter>` is one turn — there is no multi-line prompt in
the buffer; a message is a single line sent on Enter. (For scripted or
multi-line input, `:ai-prompt <text>` sends a prompt without touching the
buffer.)

Because the insert-entering chords always relocate you to the prompt, you
never have to navigate there by hand, and you can never accidentally type
into the transcript.

### Interrupting a turn

If the agent is heading the wrong way, press `<C-c>` while in Insert mode
to interrupt the active turn. This forwards an ACP `session/cancel`: the
agent stops what it is doing, but the **session stays open** — your next
prompt continues the same conversation. To end the session entirely (and
stop the `opencode` process), use `:ai-stop`.

---

## Reviewing proposed edits

When the agent wants to change a file, it asks for permission and lattice
opens the change as an interactive **side-by-side diff** — the original on
the left, the agent's proposed content on the right. This is the same diff
engine described in [Diff & merge](diff.md), so the sign column, both-pane
hunk tints, and `]c` / `[c` hunk navigation all work, and it opens folded
to just the changes with the cursor on the first one.

You resolve the proposal with:

| Command | Outcome |
|---|---|
| `:diff-accept` | Accept the change — it's written, and the agent is told the edit was applied |
| `:diff-reject` | Discard it — nothing is written, and the agent is told the edit was denied |

The agent's request **blocks on your verdict** — there is no timeout, so
you review at your own pace. Both panes are editable, exactly as with
`:diffsplit`: you can tweak the agent's proposal on the right before
accepting, and accept writes the live right-hand content.

### What runs without asking

Not every agent action opens a diff. In **review mode** (the default):

- **Reads auto-run.** Reading files, searching, and fetching don't change
  anything, so the agent does them without prompting you.
- **File edits open a diff** — the review flow above.
- **Everything else is denied.** A command execution, or any other
  mutating operation lattice can't show you as a diff, is **refused** in
  review mode. This is deliberate: there's no way to run arbitrary
  commands on your machine without your say-so. If you want the agent to
  run freely, turn on trust mode.

---

## Trust mode

**Trust mode** is the opt-in that lets the agent act without the diff gate.
Press `<C-t>` in Normal mode (in the conversation buffer) to toggle it; the
mode echoes which state you flipped to:

```
ai: trust mode on — edits auto-accepted
ai: review mode — edits gated on diff review
```

| Mode | Edits | Commands / other mutations | Reads |
|---|---|---|---|
| **Review** (default) | Open a diff you accept/reject | Denied | Auto-run |
| **Trust** | Auto-accepted, no diff | Auto-allowed | Auto-run |

Trust mode is **per session** and starts **off** every time you
`:opencode` — it never silently carries over from a previous session, so a
fresh agent always begins in review mode. Toggle it back to review with
`<C-t>` at any time; the change takes effect on the agent's next request.

---

## The headless commands

The conversation buffer is the primary way to use the agent, but the same
session is reachable without it — useful for scripts, mappings, or a quick
one-off:

| Command | Behavior |
|---|---|
| `:ai-prompt <text>` | Send `<text>` to the running agent as a prompt (no buffer interaction) |
| `:ai-stop` | Stop the agent and close the session |
| `:ai-log [opencode]` | Open the per-session trace log buffer |

`:ai-prompt` requires a running session (`:opencode` first). It's the
scriptable equivalent of typing in the prompt and pressing `<Enter>`.

---

## The trace log vs the conversation

Lattice keeps two separate surfaces for the agent, and it's worth knowing
which is which:

- **The conversation** (`*ai:opencode*`, opened by `:opencode`) is what
  you read and talk to — message text, reasoning, tool calls.
- **The trace log** (`*ai:opencode:<n>*`, opened by `:ai-log`) is the
  diagnostic stream — the ACP handshake, permission requests, decode
  failures, and session lifecycle. It's the agent equivalent of
  [`:lsp-log`](lsp.md): you open it when something misbehaves, not to hold
  a conversation.

This split means the conversation buffer stays clean (just the dialogue)
while every protocol detail is still one `:ai-log` away when you need to
debug.

---

## Lifecycle and security

- **`:opencode`** starts the `opencode acp` child process and opens the
  conversation. **`:ai-stop`** ends the session and stops the process.
- **The agent runs as a child process** driven over stdio — no network
  server is opened, and no third-party code runs inside lattice's address
  space. (This is the opposite topology from [Claude Code](claude-code.md),
  which binds a loopback WebSocket the agent dials into.)
- **Edits are gated by review mode by default** — the agent cannot write a
  file until you accept its diff, and it cannot run commands at all unless
  you turn on trust mode. Trust is an explicit, per-session choice.

---

## Related

- [`claude-code.md`](claude-code.md) — the other agent integration (the
  `claude` CLI as an IDE peer over WebSocket/MCP).
- [`diff.md`](diff.md) — the side-by-side diff UI used to review edits.
- [`buffers.md`](buffers.md) — the buffer registry the conversation lives in.
- [`docs/dev/architecture/agent-ui.md`](../dev/architecture/agent-ui.md)
  — developer reference for the conversation buffer's design.
- [`docs/dev/architecture/agent-integration.md`](../dev/architecture/agent-integration.md)
  — why the two agents share one capability surface.
