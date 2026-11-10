---
summary: "llm-inspect-mode: *llm-inspect*, a request exactly as it goes to the provider — address, headers, body, every exchange of it — with the key shown as <redacted>. Read-only; updates itself."
related: [llm-guide, llm, llm-chat-mode, llm-context-mode, inspect]
---

# llm-inspect-mode

`:llm-inspect` (`<leader>ai`) opens `*llm-inspect*`: a request as it goes over the wire,
with the address, the headers and the body. The key is always shown as `<redacted>`.

- **In a chat** it shows what sending the prompt you have typed would send, before anything
  is sent, with settings, system prompt and context included.
- **In any other buffer** it shows the last request sent from it: a line naming the
  request (model, start time, state, where the answer went), then each exchange it sent
  under its own rule. A request that ran tools sent one exchange per round.

What requests sent is kept for the editor run, up to `llm.history_mib` (64 MiB). Past it
the oldest text is dropped first, and its exchange says so; the newest is always kept.

The view is read-only and keeps itself current, so a request still being built shows when
it is ready, without a keystroke. When a reply surprises you, this is the first place to
look. See [`llm-guide`](help:llm-guide) for what leaves the machine and when.
