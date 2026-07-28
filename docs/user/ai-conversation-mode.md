---
summary: "ai-conversation-mode: the agent conversation buffer — a read-only transcript with an editable prompt. CR sends, C-j newlines, C-c interrupts, C-t toggles trust."
related: [ai, agent, opencode, acp]
---

# ai-conversation-mode

The agent conversation buffer: the transcript above, an editable prompt
at the bottom. This is the ACP-backed view — see
[`opencode-mode`](help:opencode-mode) for the agent that most commonly
drives it.

Because it's a transcript-plus-prompt buffer, it also carries
[`repl-mode`](help:repl-mode): pressing `i` (or `a` / `o` / `A` / `I` /
`O`) anywhere jumps you to the prompt and enters Insert, rather than
trying to insert into the read-only transcript.

The headerline reports the session's token usage and cost, plus a
`⌛ N queued` count when prompts are waiting behind the running turn.

## Chords

| Chord | Mode | Action |
|---|---|---|
| `<CR>` | Insert | Send the prompt to the agent |
| `<C-j>` | Insert | Insert a newline in the prompt |
| `<C-c>` | Insert | Interrupt the active turn |
| `<C-t>` | Normal | Toggle trust mode (auto-accept vs review) |

`<CR>` sends, which is why a newline needs its own key — a multi-line
prompt is common enough (paste a stack trace, write a spec) that
`<C-j>` is worth the muscle memory.

## Trust mode

`<C-t>` switches between **review** and **auto-accept** for the agent's
permission requests:

- **Review** — each request the agent makes surfaces as an
  [`ai-permission-mode`](help:ai-permission-mode) prompt you answer.
- **Auto-accept** — requests are granted without asking.

Toggle deliberately. Auto-accept is the right setting when you're
watching a long mechanical task and wrong when the agent is touching
anything you'd want to see first.

## The transcript

Streams in as the agent works — you can read and search it while a turn
is still running. Tool calls and reasoning blocks are foldable, so a
long transcript stays navigable; the usual fold chords apply (see
[`folding`](help:folding)).

Only the prompt region is editable. The transcript is a record, and the
read-only gate is what keeps it one.

## See also

- [`opencode-mode`](help:opencode-mode) — the opencode agent.
- [`ai-permission-mode`](help:ai-permission-mode) — answering a
  permission request.
- [`ai-log-mode`](help:ai-log-mode) — the agent process's own log.
