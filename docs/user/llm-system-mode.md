---
summary: "llm-system-mode: the system prompt in an ordinary buffer. :w applies it to the session and closes the buffer, :q! discards. Nothing is written to disk."
related: [llm, llm-chat-mode, system, prompt]
---

# llm-system-mode

`:llm-system` opens the system prompt of the current session in a normal buffer,
with the full editing grammar. A prompt that names a file shows the path.

| Command | Effect |
|---|---|
| `:w` | Apply the text to the session and close the buffer. No file is written. |
| `:q!` | Discard. The session is unchanged. |
| `:w <path>` | Write a copy to `<path>`, as in vim. |

An empty buffer clears the override. With a line range (`:3,9llm-system`) the lines seed the
buffer. See [`llm`](help:llm) for the other ways to override a setting.
