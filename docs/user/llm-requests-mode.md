---
summary: "llm-requests-mode: *llm-requests*, every LLM request running and the last few finished, with state, model and destination. dd on a row stops it. Read-only; updates itself."
related: [llm-guide, llm-mode, llm-chat-mode, llm-context-mode, requests]
---

# llm-requests-mode

`:llm-requests` (`<leader>aq`) opens `*llm-requests*`: every request running, then the last
few that finished, newest first. Each row shows its state (`streaming`, `retrying` with the
reason, `awaiting review`, ...), its model and where its answer is going (`below main.rs:4`,
`echo`, `register "a`, a chat, a rewrite).

| Keys | Effect |
|---|---|
| `dd` | Stop the request on this line, as `<C-c>` stops a chat's. An answer being written into a buffer keeps what arrived and ends with `[interrupted]`. |

The list is read-only and keeps itself current: a request starting or finishing shows
without a keystroke. `:llm-stop` (`<leader>ak`) stops the current buffer's request without
opening the list. See [`llm-guide`](help:llm-guide) for retries and timeouts.
