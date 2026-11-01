---
summary: "llm-chat-mode: a chat with a language model in its own buffer — a read-only transcript with an editable prompt. CR sends, C-j newlines, C-c interrupts, gq closes."
related: [llm, ai, chat, model]
---

# llm-chat-mode

`:llm` opens a chat with a language model. The transcript is above, an
editable prompt at the bottom, and the reply streams in as the model
writes it. It is the same shape as
[`ai-conversation-mode`](help:ai-conversation-mode), but the model is
called directly over its API instead of through an agent process.

Every `:llm` opens a **new buffer**, named `*llm:1*`, `*llm:2*` and so
on, so unrelated questions never share a history and chats can run at
once. Because it is a transcript-plus-prompt buffer it also carries
[`repl-mode`](help:repl-mode): `i`, `a`, `o`, `A`, `I` and `O` jump to the
prompt and enter Insert rather than trying to insert into the history.

The headerline shows the model, its state, the token count and, if you
configured prices, the cost. `⌛ N queued` appears while prompts wait
behind a reply.

## Chords

| Chord | Mode | Action |
|---|---|---|
| `<CR>` | Insert | Send the prompt |
| `<C-j>` | Insert | Insert a newline in the prompt |
| `<C-c>` | Insert | Interrupt the reply |
| `gq` | Normal | Close the chat |

`<CR>` sends, which is why a newline needs its own key.

## Sending while a reply streams

Pressing `<CR>` while the model is still answering does not start a
second request. The prompt is **queued**: it shows in the transcript as
`you (queued):` and is sent when the reply finishes, in the order you
sent it. The first reply is never split by a later question.

`<C-c>` stops the reply in progress. A queued prompt, if any, is sent next.

## Setting up a provider

Chats read an `[llm]` table from your `lattice.toml` when the editor
starts. Providers are named; `kind` is `anthropic` or `openai-compatible`
(a provider named `anthropic` defaults to the first).

```toml
[llm]
default_provider = "anthropic"
default_model = "claude-sonnet-4-5"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.local]
kind = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder"
```

- `default_provider` is optional when only one provider is configured.
- A provider's own `model` wins over `default_model`.
- `base_url` has a sensible default for `anthropic` and OpenAI.
- `input_cost_per_mtok` and `output_cost_per_mtok` (USD per million
  tokens) turn on the cost in the headerline. Without both, no cost is
  shown: a guessed price is worse than none.

### Keys

A key never goes in the config file. Give one of:

- `api_key_env = "NAME"` — read from that environment variable.
- `api_key_command = "pass show anthropic"` — run through `sh -c`; the
  first line of its output is the key. It runs in the background, so a slow
  password manager does not freeze the editor, and it is given 30 seconds.

A local OpenAI-compatible server (Ollama, llama.cpp) needs neither.

If the key is missing, or the command fails, the buffer says so under the
transcript (`! environment variable ANTHROPIC_API_KEY is not set`) and the
prompt stays usable: fix it and send again. The message names the
variable or the exit status, never the command's output.

Edits to the config apply the next time the editor starts.

## The transcript

You can read, search and yank anywhere in it while a reply is streaming;
the caret stays in the prompt. Only the prompt is editable, and `x`, `dd`,
`p` and the other operators are refused in the history and work in the
prompt.

Closing the buffer (`gq`, `:bd`) cancels the reply in flight. An unsaved
chat does not block `:qa`.

## See also

- [`ai-conversation-mode`](help:ai-conversation-mode) — the agent
  conversation buffer.
- [`repl-mode`](help:repl-mode) — the insert-entry keys.
