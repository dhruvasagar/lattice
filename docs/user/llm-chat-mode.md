---
summary: "llm-chat-mode: a chat with a language model in its own buffer — a read-only transcript with an editable prompt. CR sends, C-j newlines, C-c interrupts, gb forks, gq closes; :llm-retry asks again; :llm-save / :llm-resume keep a chat, :llm-export writes it out."
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
| `gb` | Normal | Fork a new chat from the question under the cursor |
| `<C-t>` | Normal, Insert | Trust this chat's edits (apply without a diff); again to stop |

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

## Forking and retrying

The history cannot be edited, so going back is done by **forking**. `gb`
on a past question (or on the answer to it) opens a new chat holding
everything before that question, with the question in its prompt, ready to
edit and send. `:llm-fork` does the same. The chat you forked from is left
exactly as it was, so you can compare where the two go. The fork starts from
this chat's settings and context; trust (`<C-t>`) and commands you allowed
"for this chat" are not carried over.

`:llm-retry` sends the last question again **in place of its answer**,
with the settings as they are now. That is how to try another model:

```
:setlocal llm.model=local/qwen2.5-coder
:llm-retry
```

It is refused while a reply is still coming: stop it with `<C-c>` first.

Some models send their reasoning back with a signature that only the same
provider can check. When a chat moves to another provider, that reasoning
is left out of the requests, and `*messages*` says so once.

## Keeping a chat

`:llm-export notes.md` writes the chat to a file for reading later: the
transcript as you see it, with every reasoning and tool block open. A
`.org` file gets org instead: each turn a heading, code fences as
`#+begin_src` blocks, and lines org would read as structure escaped. A
reply's `#+` lines and links that run code (`elisp:`, `shell:`) stay
text, so opening the export never acts on them. An export is for keeping,
not resuming; nothing reads it back.

`:llm-save chat.json` saves the chat to **continue later**, and
`:llm-resume chat.json` opens it again as a new chat: its history, the
tool calls and their results, and the model settings. The chat must have
finished replying first. Like `:w`, both writing commands keep an existing
file unless you add `!` (`:llm-save! chat.json`).

A session file never holds a key. Because one may come from someone else,
it is trusted as a project's config is: it may choose the model, a system
prompt (as text) and parameters, and narrow the tools, but it cannot turn
on tools your own config does not, point requests at another server, or
name a key command. What it tried is listed in `*messages*`. A file edited
after lattice wrote it is refused, since its history may no longer be what
the model was sent, and so is one written by a newer lattice.

## Setting up a provider

Chats read an `[llm]` table from your `lattice.toml` when the editor
starts. Providers are named; `kind` is `anthropic`, `gemini` or
`openai-compatible` (a provider named `anthropic` or `gemini` gets that
kind without saying so; anything else defaults to `openai-compatible`).

```toml
[llm]
default_provider = "anthropic"
model = "claude-sonnet-4-5"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.gemini]
api_key_env = "GEMINI_API_KEY"
model = "gemini-2.5-flash"

[llm.providers.local]
kind = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder"
```

- `default_provider` is optional when only one provider is configured.
- `model` is `provider/model` or a bare model on the default provider; a
  provider's own `model` is used when nothing else names one.
- `base_url` has a sensible default for `anthropic`, `gemini` and OpenAI.
  A `gemini` base URL includes the version (`.../v1beta`).
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

`lattice.edit` lets it change files too:

| Tool | Does |
|---|---|
| `edit_file` | Replaces an exact piece of text in a file |
| `write_file` | Creates a file, or replaces all of one |

**Nothing is changed until you say so.** Each change opens as a diff
against the file as you have it: `:diff-accept` applies it,
`:diff-reject` refuses it, and you can edit the right side first. The
model is told which you chose. A file you have open is changed in its
buffer, unsaved, as one change `u` undoes; any other file is written.
If the file changed while the diff was open, nothing is applied and the
model is told to read it again. Diffs open one at a time, with rewrites
in the same line, and `:llm-requests` shows `awaiting review` meanwhile.

**`<C-t>`** in the chat trusts its edits: they apply without a diff,
and the headerline says `edits trusted` until you press `<C-t>` again or
close the chat. It is never saved and never carried to another chat. An
edit under `.git` still opens a diff: a hook there runs later, and `git
status` would never show it.
Closing a chat closes any diff it still has open.

`lattice.exec` lets it run commands:

| Tool | Does |
|---|---|
| `run_command` | Runs a shell command in the project |

**Every command asks first.** A menu shows the command and where it
runs, with three answers: allow once, allow this command for this chat,
or deny.
Move to one and press `<CR>`. A denial is passed to the model, which is
told not to try again unless you ask. `<Esc>` decides later: the request
waits, and `:ai-permission` brings the question back.

The command runs in the project, in its own process group, so stopping
the request (`<C-c>`, `:llm-stop`, closing the chat) ends everything it
started. Nothing outlives the command either: a `server &` it left
running is stopped when the shell exits. It stops after 120 seconds unless the model asks for longer (up
to 600), and the model sees at most the first 30 000 bytes of each of
its output and error streams, with a note saying where it was cut.

"Allow this command for this chat" covers that exact command, run in the
same place, until the chat closes: the same `cargo test` runs again
without asking, but any other command asks. It is never saved. `<C-t>`
trusts edits only: commands still ask. A project's
`.lattice/llm.toml` cannot turn `lattice.exec` on; only your own config,
a profile, or `:setlocal` in the chat can.

A selector is `server.name`: one tool (`lattice.read_file`), a kind
(`lattice.read`, `lattice.edit`, `lattice.exec`), or all of a server's
(`lattice.*`).

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
