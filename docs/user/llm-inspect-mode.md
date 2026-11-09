---
summary: "llm-inspect-mode: *llm-inspect*, a request exactly as it goes to the provider — address, headers, body — with the key shown as <redacted>. Read-only; updates itself."
related: [llm-guide, llm, llm-chat-mode, llm-context-mode, inspect]
---

# llm-inspect-mode

`:llm-inspect` (`<leader>ai`) opens `*llm-inspect*`: a request as it goes over the wire,
with the address, the headers and the body. The key is always shown as `<redacted>`.

- **In a chat** it shows what sending the prompt you have typed would send, before anything
  is sent, with settings, system prompt and context included.
- **In any other buffer** it shows the last request sent from it.

The view is read-only and keeps itself current, so a request still being built shows when
it is ready, without a keystroke. When a reply surprises you, this is the first place to
look. See [`llm-guide`](help:llm-guide) for what leaves the machine and when.
