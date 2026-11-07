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
| `gf` | Normal | On text an ask sent, go back to where it came from |

`<CR>` sends, which is why a newline needs its own key.

## Sending while a reply streams

Pressing `<CR>` while the model is still answering does not start a
second request. The prompt is **queued**: it shows in the transcript as
`you (queued):` and is sent when the reply finishes, in the order you
sent it. The first reply is never split by a later question.

`<C-c>` stops the reply in progress. A queued prompt, if any, is sent next.
If a reply fails instead, the error is shown and the queued prompts wait: the next
prompt you send resumes them in order. The failed question is not sent to the
model again.

## Setting up a provider

Chats read an `[llm]` table from your `lattice.toml` when the editor
starts. Providers are named; `kind` is `anthropic` or `openai-compatible`
(a provider named `anthropic` defaults to the first).

```toml
[llm]
default_provider = "anthropic"
model = "claude-sonnet-4-5"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.local]
kind = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder"
```

- `default_provider` is optional when only one provider is configured.
- `model` is `provider/model` or a bare model on the default provider; a
  provider's own `model` is used when nothing else names one.
- `base_url` has a sensible default for `anthropic` and OpenAI.
- `input_cost_per_mtok` and `output_cost_per_mtok` (USD per million
  tokens) turn on the cost in the headerline. Without both, no cost is
  shown: a guessed price is worse than none.

A key never goes in the file: use `api_key_env` or `api_key_command`
(a literal `api_key` is refused). If the key is missing, or the command
fails, the buffer says so under the transcript and the prompt stays
usable: fix it and send again. The message names the variable or the exit
status, never the command's output.

Profiles, the `llm.*` options, project config and `:llm-profile` are in
[`llm`](help:llm). `:llm writer` opens a chat from a profile.

## The transcript

You can read, search and yank anywhere in it while a reply is streaming;
the caret stays in the prompt. Only the prompt is editable, and `x`, `dd`,
`p` and the other operators are refused in the history and work in the
prompt.

Closing the buffer (`gq`, `:bd`) cancels the reply in flight. An unsaved
chat does not block `:qa`.

## Markdown and folded blocks

The transcript is markdown and is highlighted as such, with fenced code
highlighted in its own language. It stays valid markdown while a reply
streams: a code block that has not finished arriving is closed for you, so
it never colours the rest of the transcript or the prompt as code.

Reasoning and tool calls are **blocks**. A tool call is a head line that
stays visible when the block is folded, then its input and output in
fenced bodies:

````
▸ github · search_issues [ok]
```json
{ "query": "fold overlap" }
```
```json
{ "total_count": 3 }
```
````

The label before the `·` says who ran the tool (a server name); it is
absent for built-in tools. Output is shown as `diff` for edits and `bash`
for commands, otherwise as `json` if it parses and `text` if not. A fence
is always longer than any fence inside the output.

`za` on a head line opens or closes that block, and `zM` / `zR` close or
open every fold. A block you opened stays open as the reply keeps
streaming. Set how each kind is shown with `llm.fold.reasoning` and
`llm.fold.tools`, in `lattice.toml` or with `:set` / `:setlocal`. A change
applies to open chats at once:

```toml
[llm.fold]
reasoning = "closed"   # closed | open | hidden
tools = "closed"
```

- `closed` (the default) shows the head line and hides the body.
- `open` shows the whole block.
- `hidden` leaves the block out of the transcript. It is not deleted:
  setting it back to `closed` or `open` shows those blocks again.

`<CR>` sends the prompt in this buffer. It does not follow markdown links,
as it does in a markdown file.

## Tools

A chat can let the model read your project while it answers. Nothing is
on until you choose it, with `llm.tools` in your config, a profile, or
`:setlocal` in the chat:

```toml
[llm]
tools = "lattice.read"
```

`lattice.read` gives these, each shown as a tool block in the transcript:

| Tool | Does |
|---|---|
| `read_file` | Reads a file, up to 2000 lines at a time |
| `list_directory` | Lists a directory |
| `search` | Searches the project for a regular expression, skipping what `.gitignore` ignores |

They run without asking, so they are kept to what you could see yourself:

- **A file you have open is read as you have it**, unsaved edits
  included, not as it is on disk.
- **Only inside the project.** A path outside it (`../`, `/etc`, a link
  out) is refused, and the model is told why.
- **Never a file `llm.exclude` names**, as for everything else sent.

A selector is `server.name`: one tool (`lattice.read_file`), a kind
(`lattice.read`), or all of a server's (`lattice.*`). Edit and command
tools come later; until then only reads exist.

One request may run tools for up to `llm.tool_rounds` rounds (25 unless
you set it). A model still calling tools then is stopped with a message;
send again to let it go on. `:llm-stop` (or `<C-c>`) stops the request
wherever it is, including while a tool runs, and the call is marked as
stopped so the next prompt carries on cleanly.

## See also

- [`ai-conversation-mode`](help:ai-conversation-mode) — the agent
  conversation buffer.
- [`llm-mode`](help:llm-mode) — asking about text from any buffer.
- [`repl-mode`](help:repl-mode) — the insert-entry keys.
- [`llm`](help:llm) — settings, profiles and project config.
