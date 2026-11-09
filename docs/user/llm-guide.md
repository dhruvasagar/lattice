---
summary: "The LLM layer from start to finish. Setup, chat, asking about text from any buffer, context, tools with review, MCP servers, profiles, watching and stopping requests, the privacy model, and how it differs from the agent integrations."
related: [llm, llm-chat-mode, llm-mode, llm-mcp-mode, llm-system-mode, ai-conversation-mode, claude-code-mode]
---

# Language models

Lattice can call a language model directly, over its HTTP API, from inside the
editor. There is no agent program in between: the editor builds the request,
streams the reply, runs the tools the model asks for and shows you the result.
Anthropic, Gemini and any OpenAI-compatible server (including local ones such as
Ollama) are supported.

This page is the whole story in one place. The reference pages cover each part
in full and are linked from the section that uses them; the last section lists
them all.

## What it is

There are three ways in, and they share one engine.

- **A chat.** `:llm` opens a buffer with a read-only transcript above and an
  editable prompt below. You talk to the model there.
- **An ask.** `<leader>aa` followed by a motion sends the text that motion covers
  to the model, from any buffer, and the answer opens beside you without taking
  the focus.
- **A rewrite.** `<leader>ar` followed by a motion asks the model to change that
  text. The answer comes back as a diff you accept or reject.

Every use is the same thing underneath: some text, an instruction and a set of
settings, sent to a destination. The entry points differ only in their defaults.

### Principles

These are the decisions that shape everything below. Each is something the code
does, not an aspiration.

**The model is called directly.** `:llm` does not start a child process or run
someone else's loop. Lattice speaks the provider's API itself, which is why the
request is yours to inspect (`:llm-inspect`), the settings are yours to change
per buffer, and a local server works the same as a hosted one.

**Every surface is an ordinary buffer.** A chat transcript, the list of running
requests (`*llm-requests*`), the list of attached context (`*llm-context*`), the
MCP server list (`*llm-mcp*`) and the system prompt (`:llm-system`) are all
buffers. You search them, yank from them, split them, and close them with the
commands you already know. Nothing lives in a sidebar or a panel with its own
rules.

**Asking is a vim operator.** `<leader>aa` takes a motion or a text object like
`d` or `y` does, so `<leader>aaip` asks about a paragraph, `<leader>aaj` about
this line and the next, and `<leader>aa` in Visual about the selection. The
grammar you already have is the interface. Asking reads text and changes nothing,
so it works in read-only buffers too: `*compilation*`, `*messages*`, help.

**Nothing is changed without your review.** A model's edit opens as a diff
against the file as you have it, and you accept or reject it. A rewrite does the
same. A shell command asks before it runs, showing the whole command. The one
switch that lifts the diff (`<C-t>`) lifts it for edits only, for one chat, and
is never saved.

**Keys stay out of everything you might share.** An API key is read from an
environment variable or a command, held only to sign the request, and never
written to a log, to `:llm-inspect` (shown as `<redacted>`), to a saved session,
to an export, or to an error message. A literal key in a config file is refused.

**Nothing blocks typing.** Requests run off the UI thread and the reply streams
in. You can read, search and yank in a transcript while it fills, keep editing
the buffer an answer is being written into, and close a window without cancelling
the request in it.

## Setup in five minutes

Providers live in the `[llm]` table of `~/.config/lattice/lattice.toml`. They load
when the editor starts.

### Anthropic

```toml
[llm]
default_provider = "anthropic"
model            = "claude-sonnet-4-5"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
```

Export the variable in the shell that starts lattice, then run `:llm`.

### A local OpenAI-compatible server

```toml
[llm]
default_provider = "local"

[llm.providers.local]
kind     = "openai-compatible"
base_url = "http://localhost:11434/v1"
model    = "qwen2.5-coder"
```

A local server usually wants no key, and none is required for one. Providers named
`anthropic` or `gemini` get that kind without saying so; any other name defaults to
`openai-compatible`.

### Where keys come from

| Key | Meaning |
|---|---|
| `api_key_env = "NAME"` | Read the key from this environment variable |
| `api_key_command = "pass show ..."` | Run through `sh -c`; the first line of its output is the key |

`api_key_command` runs in the background, so a slow password manager does not
freeze the editor; it is cut off after 30 seconds. A literal `api_key = "..."` is
refused with a message saying so. If the key is missing or the command fails, the
chat says so under the transcript and the prompt stays usable: fix it and send
again. The message names the variable or the command's exit status, never its
output.

### Several providers, and prices

You can configure as many providers as you like and choose per request with
`provider/model`:

```toml
[llm.providers.gemini]
api_key_env = "GEMINI_API_KEY"
model       = "gemini-2.5-flash"

[llm.providers.anthropic]
api_key_env          = "ANTHROPIC_API_KEY"
input_cost_per_mtok  = 3.0     # USD per million tokens
output_cost_per_mtok = 15.0
```

With both prices set, the chat's headerline shows a running cost. Without both, it
shows none: a guessed price is worse than no price. A `gemini` `base_url` includes
the version (`.../v1beta`).

Reference: [`llm-chat-mode`](help:llm-chat-mode) (provider keys) and
[`llm`](help:llm).

## Chatting

`:llm` opens a new chat. Each one is its own buffer, named `*llm:1*`, `*llm:2*`
and so on, so unrelated questions never share a history and several chats can run
at once. `:llm writer` opens a chat from the profile called `writer`
(see [Profiles](#profiles-options-and-project-config)).

Type in the prompt and press `<CR>` to send. The reply streams into the transcript
above. The headerline shows the model, its state and the token count, and the
cost if you configured prices.

| Chord | Mode | Action |
|---|---|---|
| `<CR>` | Insert | Send the prompt |
| `<C-j>` | Insert | Newline in the prompt |
| `<C-c>` | Insert | Interrupt the reply |
| `gq` | Normal | Close the chat |
| `gb` | Normal | Fork a new chat from the question under the cursor |
| `gf` | Normal | On text an ask sent, go back to where it came from |
| `<C-t>` | Normal, Insert | Trust this chat's edits; again to stop |
| `<M-m>` | Normal, Insert | Open `:llm-menu` |

The transcript is read-only history with one editable region, the prompt. `i`, `a`,
`o`, `A`, `I` and `O` jump to the prompt and enter Insert, and `x`, `dd` and `p`
work in the prompt and are refused in the history.

The transcript is markdown, highlighted as such, with fenced code in its own
language. A code block still arriving is closed for you, so half a block never
colours the rest of the buffer as code. Reasoning and tool calls are folded blocks:
`za` opens one, `zM` and `zR` close and open all. Set how each kind is shown with
`llm.fold.reasoning` and `llm.fold.tools` (`closed`, `open`, `hidden`; `hidden`
leaves a block out of the view without deleting it).

### Sending while a reply streams

Pressing `<CR>` while the model is answering does not start a second request. The
prompt is queued, shown as `you (queued):`, and sent when the reply finishes, in
order. The first reply is never split by a later question. `<C-c>` stops the reply
in progress and the next queued prompt goes out. If a reply fails, the error is
shown and queued prompts wait; the next prompt you send resumes them.

### Forking and retrying

The history cannot be edited, so going back is done by forking. `gb` on a past
question (or on its answer) opens a new chat holding everything before that
question, with the question in the prompt ready to edit. `:llm-fork` does the same.
The original is left as it was, so you can compare where the two go. A fork starts
from the chat's settings and context; `<C-t>` trust and "allow for this chat"
command grants are not carried over.

`:llm-retry` sends the last question again in place of its answer, with the settings
as they are now. That is how to try another model:

```
:setlocal llm.model=local/qwen2.5-coder
:llm-retry
```

It is refused while a reply is still coming; stop it with `<C-c>` first.

When a chat moves to another provider, reasoning the first provider signed is left
out of the requests (only that provider can verify it), and `*messages*` says so
once.

### Keeping a chat

| Command | Does |
|---|---|
| `:llm-export notes.md` | Write the chat for reading: the transcript with every block open. `.org` gets org. Nothing reads an export back |
| `:llm-save chat.json` | Save the chat to continue later |
| `:llm-resume chat.json` | Open a saved chat again as a new chat: history, tool calls and their results, settings |

The chat must have finished replying before `:llm-save`. Both writing commands keep
an existing file unless you add `!`. A saved or exported chat is written readable by
you only. An org export never contains live links: `#+` lines in a reply, and links
that run code (`elisp:`, `shell:`), stay text.

A session file is treated like config from a stranger, because it may be one. It can
choose the model, a system prompt as text and parameters, and can narrow your tools,
but it cannot turn on tools your own config does not, point requests at another
server, or name a key command. What it tried is listed in `*messages*`. A file edited
after lattice wrote it is refused (its history may no longer be what the model was
sent), and so is one written by a newer lattice.

Reference: [`llm-chat-mode`](help:llm-chat-mode).

## Asking about text from any buffer

You do not need a chat to ask. From a source file, a compiler error, a terminal's
output or a search result:

| Keys | Asks about |
|---|---|
| `<leader>aaip` | The paragraph |
| `<leader>aaj` | This line and the next |
| `<leader>aaa` | This line |
| `<leader>aa` in Visual | The selection |
| `<leader>aA{motion}` | The same, opening `:llm-menu` first |

`<leader>` is `<Space>` unless you set `keymap.leader`. A prompt opens for the
instruction (`explain this`, `why does it panic?`); `<CR>` sends, `<Esc>` sends
nothing, and an empty instruction sends the text alone. The text is captured when
you press the motion, so editing while you type the instruction changes nothing.

The answer opens in a new buffer beside you. Your caret, your mode and your pane do
not move. A second ask reuses that pane. The buffer is a chat: its first turn is what
you sent, under a head line naming the file and lines; `gf` goes back to that place,
and typing a follow-up in its prompt continues the conversation. Answers are named
after their source, `*llm:main.rs:4*`.

`.` does not repeat an ask (an ask is not a change, as `y` is not). It does repeat a
rewrite.

`:llm-send` (`<leader>as`) needs no motion and no instruction: the buffer from its first
line to the cursor's line is the prompt. Write a question in any buffer and send it; what
is below the cursor stays. In a chat it sends the prompt, as `<CR>` does.

### Destinations

Where the answer goes is the `destination` setting. A chat is the default.

| Destination | The answer |
|---|---|
| `chat` | Opens as a chat beside you |
| `below` | Streams into your buffer on new lines under the text you asked about |
| `echo` | Shows in the echo area when complete (first line, and how many more) |
| `register` | Goes to a register, ready for `p` |
| `<buffer name>` | Becomes the next turn of an open chat, e.g. `*llm:main.rs:1*` |
| `review` | Not built yet: an ask that names it opens a chat and says so |

From the command line, a range and leading `key=value` words set this request only:

```
:'<,'>llm-ask destination=below write the doc comment for this
:'<,'>llm-ask model=local/qwen temperature=0 explain this
:'<,'>llm-ask destination=*llm:main.rs:1* and this part?
```

`:llm-ask` without a range asks about the current line. `system` is not one of the
`key=value` words: a system prompt is a paragraph and has `:llm-system`.

**Below.** The answer is written into the buffer as it streams. Text you type above
it moves it down; text you type where it is being written stays after it. Your caret
stays where it was. `u` stops the request and takes back what was written, all at once
if you typed nothing in between. Deleting the lines it is going under stops it and
keeps what was written. A failure partway leaves what arrived followed by an
`[interrupted]` line, and `u` removes both. One answer at a time at one place. A
read-only buffer, or a view composed of several files, cannot take it: the answer
opens in a chat instead and the echo area says why. The full answer is also kept in a
hidden chat.

**Register.** Name the register before the operator, as for a yank: `"a<leader>aaj`
puts the answer in `a`. Without one it goes in the unnamed register. A multi-line
answer puts as lines.

**Echo and register** change nothing in the buffer; a failure only shows in the echo
area. `:llm-last` opens the latest ask's chat beside you, so the whole answer of an
echo, register, below or rewrite request, with its history, is a command away. The last
32 unshown chats are kept.

### The menu

`:llm-menu` (`<leader>am`, or `<M-m>` in a chat) lists every setting with where its
value came from and names the session the next request would use. You can change the
model, temperature or destination there. `=` switches the change between the
**session** and the **next request only**; `x` clears. `<leader>aA{motion}` opens it
before the prompt, so you choose, then press `a` to type the instruction. `q` dismisses
it and sends nothing.

### Rewriting

`<leader>ar{motion}` (or Visual) takes an instruction (`make this a match`, `fix the
grammar`) and never changes the text directly. When the answer arrives a diff opens in
a new tab: your text as it was on the left, the proposal on the right, which you can
edit. `:diff-accept` applies the right side as **one change** (one `u` undoes it);
`:diff-reject` leaves the buffer alone. Nothing is written to disk either way.

You can go on editing while the model works, and the rewritten text follows your edits
as a mark would. If you changed the text itself, accepting re-opens the diff against the
text as it now is, so you see exactly what would be replaced. If you deleted the text or
closed the buffer, nothing is applied and the answer opens in its chat so it is not
lost. A reply that is one fenced block is unwrapped before the diff.

`<leader>aR{motion}` does the same but opens `:llm-menu` first, as `<leader>aA` does for an
ask: change the model or temperature for this rewrite, then press `a` to type the instruction.

Rewrites queue: a second waits for the first diff to be settled, so a macro that
rewrites several places shows them one at a time. A rewrite is refused, with the reason,
in a read-only buffer, in a composed view (rewrite in the file), over a block selection,
and in a file `llm.exclude` names.

### Large selections

A selection over 32 KB (about 8,000 tokens) asks before it is sent, naming its size.
`y` sends it; anything else sends nothing.

### Changing the chords

Every llm chord is a prefix plus one key. The prefix is the option `llm.prefix`
(`<leader>a` by default):

```toml
[llm]
prefix = "<C-c>."
```

| After the prefix | Does |
|---|---|
| `a{motion}` | Ask |
| `A{motion}` | Ask, opening `:llm-menu` first |
| `r{motion}` | Rewrite |
| `R{motion}` | Rewrite, opening `:llm-menu` first |
| `c{motion}` | Attach text to the context |
| `l` | `:llm` |
| `s` | `:llm-send` |
| `m` | `:llm-menu` |
| `C` | `:llm-context` |
| `i` | `:llm-inspect` |
| `q` | `:llm-requests` |
| `L` | `:llm-last` |
| `k` | `:llm-stop` |

Press the prefix and wait and which-key lists them; the next key still runs its chord,
so typing quickly never waits. A buffer that binds the prefix's first key itself wins in
that buffer: with `<C-c>.`, that is `*compilation*`, where `<C-c>` stops the command.

Reference: [`llm-mode`](help:llm-mode).

## Context

Context is text you attach so requests from a buffer carry it along: the module a
question is about, a type defined elsewhere, a notes file.

| Keys or command | Attaches |
|---|---|
| `<leader>ac{motion}` | The text the motion covers |
| `:llm-context-add` | The whole buffer |
| `:llm-context-add notes.md src/types.rs` | Those files |

Two properties matter. Attached text is read **when each request is sent**, not when you
attach it, so the model sees it as it is then, and attached lines follow your edits.
And context belongs to a buffer: every ask from that buffer carries it, a chat opened
from the buffer takes a copy, and from then on has its own.

`:llm-context` opens `*llm-context*`, listing what requests from the current buffer carry
and where each item stands now; `dd` on a row removes it. If an item cannot be sent as
listed (its buffer closed, its lines deleted, its file unreadable, or it now matches
`llm.exclude`) the request is refused with the reason. It is never sent without the
item.

### Seeing exactly what is sent

`:llm-inspect` (`<leader>ai`) shows a request as it goes to the provider: the address, the
headers and the body, with the key shown as `<redacted>`. In a chat it shows what sending
the prompt you have typed would send, before anything is sent, with settings, system
prompt and context included. In any other buffer it shows the last request sent from it.
When a reply surprises you, this is the first place to look.

## Tools

By default a model only sees what you send. Tools let it look at the project, change
files and run commands while it answers. **Nothing is on until you choose it**, with
`llm.tools` in your config, a profile, or `:setlocal` in a chat:

```toml
[llm]
tools = ["lattice.read", "lattice.edit"]
```

A selector is `server.name`: one tool (`lattice.read_file`), a kind (`lattice.read`,
`lattice.edit`, `lattice.exec`), or all of a server's (`lattice.*`). Each call shows as a
folded block in the transcript.

### `lattice.read`

| Tool | Does |
|---|---|
| `read_file` | Reads a file, up to 2000 lines at a time (a regular file of at most 16 MiB) |
| `list_directory` | Lists a directory |
| `search` | Searches the project for a regular expression, skipping what `.gitignore` ignores |

These run without asking, so they are kept to what you could see yourself. A file you
have open is read as you have it, unsaved edits included. A path outside the project
(`../`, `/etc`, a link out) is refused and the model is told why. A file `llm.exclude`
names is never read.

### `lattice.edit`

| Tool | Does |
|---|---|
| `edit_file` | Replaces an exact piece of text in a file |
| `write_file` | Creates a file, or replaces all of one |

**Each change opens as a diff** against the file as you have it. `:diff-accept` applies
it, `:diff-reject` refuses it, and you can edit the right side first. The model is told
which you chose. A file you have open is changed in its buffer, unsaved, as one change
`u` undoes; any other file is written. If the file changed while the diff was open,
nothing is applied and the model is told to read it again. Diffs open one at a time, and
`:llm-requests` shows `awaiting review` meanwhile. Closing the chat closes any diff it
still has open.

**`<C-t>` trusts a chat's edits**: they apply without a diff, and the headerline says
`edits trusted` until you press `<C-t>` again or close the chat. Its limits are the point:

- It is per chat. It is never saved and never carried to a fork or a resumed chat.
- It covers edits only. Commands still ask, and an edit applied without review
  drops the commands allowed for the chat, so each asks again: what `cargo test`
  runs changes when `build.rs` or a test does.
- An edit under `.git` still opens a diff, however the path is written. A hook there runs
  later, and `git status` would never show it.

### `lattice.exec`

| Tool | Does |
|---|---|
| `run_command` | Runs a shell command in the project |

**Every command asks first.** A menu shows the command and where it runs, with three
answers: allow once, allow this command for this chat, or deny. The menu shows the whole
command so nothing can hide the rest: characters that draw nothing or a blank you cannot
tell from a space (zero-width, direction overrides, no-break and ideographic spaces,
fillers) are written as `\u{..}`, a long run of spaces or tabs as `␣×N`, and a run of
empty lines as one line counting them. A command over 40 lines or 4000 characters is refused
without asking, and the model is told to put it in a script. A denial is passed to the
model, which is told not to try again unless you ask. `<Esc>` decides later: the request
waits and `:ai-permission` brings the question back.

"Allow for this chat" covers that **exact command, run in the same place**, until the chat
closes or an edit is applied without review (see `<C-t>` above). The same `cargo test`
runs again without asking; any other command asks. It is never saved.

Limits on what runs:

- The command runs in the project, in its own process group. Stopping the request
  (`<C-c>`, `:llm-stop`, closing the chat) ends everything it started, and nothing
  outlives the command: a `server &` it left running is stopped when the shell exits.
- It stops after 120 seconds unless the model asks for longer, up to 600.
- The model sees at most the first 30 000 bytes of each of the output and error streams,
  with a note saying where it was cut.

A project's `.lattice/llm.toml` cannot turn `lattice.exec` on. Only your own config, a
profile, or `:setlocal` in the chat can.

### Tool rounds

One request may run tools for up to `llm.tool_rounds` rounds (25 unless you set it). A
model still calling tools then is stopped with a message; send again to let it go on.
`:llm-stop` stops the request wherever it is, including while a tool runs, and the call
is marked as stopped so the next prompt carries on cleanly.

A request that is not a chat cannot show a tool call or ask your approval. An ask whose
settings allow tools, aimed at `below`, `echo` or a register, opens in a chat instead, and
the echo area says so.

Reference: [`llm-chat-mode`](help:llm-chat-mode).

## External MCP servers

An [MCP](https://modelcontextprotocol.io) server gives a model tools of its own: issues,
a database, a documentation search. Configure one in **your own** `lattice.toml`; a
project's `.lattice/llm.toml` cannot declare one, because a stdio server is a command to
run.

```toml
[llm.mcp.github]
command = "github-mcp-server"          # stdio: lattice starts it
args    = ["stdio"]
env     = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }
allow   = ["get_issue", "search_issues"]   # these run without asking

[llm.mcp.docs]
url     = "https://example.com/mcp"    # streamable HTTP
headers = { Authorization = "Bearer $DOCS_TOKEN" }
timeout = 60
```

Select its tools like any others: `tools = ["lattice.read", "github.*"]`. The kind groups
(`read`, `edit`, `exec`) apply to lattice's own tools only; a server's word for what its
tool does is not trusted.

- **Every server tool asks before it runs**, unless it is named in `allow`. A server that
  marks a tool read-only gets that said in the question, and the tool asks all the same.
  A server tool's edits happen in its own process and cannot go through lattice's diff
  review, so your answer is the only review there is.
- A server starts when a request first selects one of its tools, never for a request that
  does not. The request waits at most 10 seconds for it and carries on without it if it
  fails or is slow.
- A call that runs past `timeout` (300 seconds by default) is cancelled on the server and
  fails, and the model is told it took too long. The server stays up.
- Stopping a request cancels its call on the server. Stopping a server kills its whole
  process tree, so one started through `npx` or a shell leaves nothing behind.
- A value in `env` or `headers` may name a variable (`$NAME` or `${NAME}`), read when the
  server starts. Neither values nor variable names are shown in `*llm-mcp*`, and anything
  the server prints that contains a token is shown as `***`.

`:llm-mcp` opens `*llm-mcp*`: each server, its state (`not started`, `starting`,
`running`, `failed: ...`, `exited: ...`, `stopped`) and its tools, each marked `allowed`
or `asks`. `r` starts the server on a line again, `dd` stops it. A server that failed to
start is not retried until you press `r`; one that was running and exited is started
again by the next request that needs it. Changes to `[llm.mcp]` apply on the next start
of lattice.

Reference: [`llm-mcp-mode`](help:llm-mcp-mode).

## Profiles, options and project config

A setting's value comes from the highest layer that sets it, key by key. `:set llm.model?`
shows which layer won.

| Layer | Source |
|---|---|
| default | the provider's own `model` |
| global | `lattice.toml` `[llm]`, or `:set llm.model=...` |
| profile | `[llm.profiles.<name>]` in `lattice.toml` |
| project | `.lattice/llm.toml` |
| session | `:setlocal llm.model=...` |
| request | arguments on one command |

A **session is a buffer**. There is no session object to manage: a new chat starts with a
copy of the session of the buffer it was opened from, then stands alone. A request reads
its settings once, when it starts; change the model mid-reply and that reply finishes as
it began.

### Profiles

A profile is a named bundle of starting values:

```toml
[llm]
default_profile = "general"

[llm.profiles.writer]
model  = "local/qwen2.5-coder"
system = "~/.config/lattice/llm/editor.md"   # a file, read when a request starts
tools  = []

[llm.profiles.coding]
model = "anthropic/claude-sonnet-4-5"
tools = ["lattice.read", "lattice.edit", "github.*"]
```

`:llm writer` opens a chat from a profile. `:llm-profile writer` switches the current
session, clearing its overrides and saying which. An unknown name lists the known ones.
`@writer` in the text you type applies that profile to that one request, and its values win
over your session overrides for that turn, though it still cannot widen your tools without
asking. `@unknown` is just text.

### Options

| Option | Meaning |
|---|---|
| `llm.model` | `provider/model`, or a bare model on the default provider |
| `llm.system` | System prompt text; a path in your own config |
| `llm.temperature`, `llm.max_tokens`, `llm.reasoning` | Request parameters |
| `llm.messages` | How much history is sent: `all`, `last`, or a number |
| `llm.tools` | Tool selectors, as above |
| `llm.destination` | Where answers go; a kind or an open chat's buffer name |
| `llm.fold.reasoning`, `llm.fold.tools` | `closed`, `open`, `hidden`; applied to open chats live |
| `llm.profile` | The profile in use |
| `llm.default_profile`, `llm.default_provider` | Global only |
| `llm.exclude` | Globs never sent; global only |
| `llm.tool_rounds` | Rounds of tool calls one request may run; global only |
| `llm.prefix` | The keys every llm chord starts with; global only |

`:set llm.model?` shows value and layer, `:setlocal llm.model=x` changes it for this buffer,
`:setlocal llm.model&` goes back. An empty value means "not set at this layer".

`:llm-system` opens the system prompt in an ordinary buffer: `:w` applies it to the session
and closes it, `:q!` discards, and nothing is written to disk. See
[`llm-system-mode`](help:llm-system-mode).

### Project config

A repository can carry `.lattice/llm.toml`, and a cloned repository is not yours. So a
project may set the model, an inline system prompt, parameters, and tools **narrower** than
yours. It may not set `base_url`, providers, key sources, wider `tools`, `trust`, `[mcp.*]`,
profiles, `destination`, or a `system` naming a file. Each refused key is reported in
`:messages` once per chat, and the rest of the file still applies. LLM keys in
`.lattice/config.toml` are not read in any spelling, and a warning says so.

One thing a project can still do is name **another of your providers** as its model
(`model = "paid/big"`), which decides where your code and that provider's key go. When it
does, `:messages` says so once per chat, naming the provider and your default. A resumed
session file is held to the same rules and gives the same warning.

Reference: [`llm`](help:llm).

## Watching and stopping requests

A request keeps running when you move on, so you can see and stop any of them from anywhere.

- `:llm-requests` (`<leader>aq`) opens `*llm-requests*`: every running request and the last
  few that finished, with its model and where its answer is going (`below main.rs:4`, `echo`,
  `register "a`, a chat, a rewrite). It updates itself; `dd` on a row stops that request.
- `:llm-stop` (`<leader>ak`) stops this buffer's request: the chat's, or the one an ask from
  this buffer started. With none here it stops the only one running; when that would be a
  guess, it opens `:llm-requests` instead.
- `<C-c>` in a chat prompt does the same for that chat. An answer being written into a buffer
  keeps what arrived and ends with `[interrupted]`.
- While any request runs, the modeline shows `llm`, or `llm 3` for three, so one in a chat
  whose window you closed is never out of sight. The echo area says when such a chat finishes
  or fails.
- `:q` on the last window, or `:qa`, with a request running says so and does not quit; `:q!`
  quits anyway, and the request ends with the editor. Nothing resumes a request later.

### Retries

A request the provider turns away for now (HTTP 429, 502, 503, 504 or 529) is sent again, up
to twice, before it fails. It waits as long as the provider asks, or a few seconds, and
`:llm-requests` shows it as `retrying` with the reason. A provider asking for more than 30
seconds is not waited for: the request fails, saying how long it asked for. Stopping a
request ends the wait at once. Connecting gives up after 15 seconds; there is no total
timeout, so a long generation is not cut off, but a stream silent for 120 seconds is.

## Privacy and security

Everything the layer does for safety, in one place.

**Keys.** A key comes only from `api_key_env` or `api_key_command`; a literal key is refused.
It never appears in `:llm-inspect` (`<redacted>`), logs, debug output, session files, exports
or error text. A provider's rejection is shown with the key scrubbed, and a key embedded in a
custom `base_url` is never shown. A failing key command reports its exit status, not its output.

**What leaves the machine.** What a request sends is visible beforehand with `:llm-inspect`.
Text is sent only from what you ask about, what you attached, and what a tool you enabled
reads. `llm.exclude` is a list of globs for files that are never sent:

```toml
[llm]
exclude = [".env", ".env.*", "*.pem", "*.key", "id_rsa*", "id_ed25519*", ".netrc"]
```

A glob without `/` matches the file name in any directory; one with `/` matches the whole
path. It holds for asking, attaching context, rewriting and the read tools, sees through
links (a file is checked by where it really is), and counts a view composed of several files
as every file it shows, checked again when the request is sent. Setting it **replaces** the
list, so copy the default to add to it; an empty list excludes nothing.

**Review gates.**

| Action | Gate |
|---|---|
| Model edits a file | A diff you accept or reject. `<C-t>` skips it for one chat, except under `.git` |
| Model runs a command | Asks, showing the whole command. Allow once, for this chat (exact command; dropped by an unreviewed edit), or deny |
| MCP server tool | Asks unless named in that server's `allow` |
| Rewrite | A diff, never a direct write |
| Large selection | Asks over 32 KB |

**Tool confinement.** The read and edit tools stay inside the project; a path or a link
leading out is refused.

**Untrusted input.** A project's `.lattice/llm.toml` and a resumed session file are held to
the same table: they may narrow your tools but never widen them, never name a provider, key
source or MCP server, and never turn on `lattice.exec`. A session file that was edited after
lattice wrote it is refused. A saved chat or export is readable by you only.

**Output you did not write.** An export never contains a live code-running org link. A
command's question shows invisible characters escaped. MCP server output containing a token
is shown as `***`.

**What this does not cover.** Once text is sent, the provider has it. A model's reply is
still a model's reply: review the diff. And an MCP server tool you allow acts in its own
process, outside lattice's review.

## How it relates to the agent integrations

Lattice has two other ways to work with a model. They are different tools.

| | LLM layer (`:llm`) | ACP conversation (`:opencode-acp`) | Claude Code (`:claude`) |
|---|---|---|---|
| Who runs the loop | Lattice | An agent process lattice starts | The `claude` CLI, in a terminal buffer |
| How the model is reached | Its HTTP API, directly | Through that agent | Through that agent |
| Entry points | Chat, ask and rewrite operators from any buffer | A conversation buffer | The agent's own terminal UI |
| Edits | Diffs you review | Permission requests you answer | Side-by-side diffs |
| Tools | `lattice.*` and your MCP servers | The agent's own | The agent's own |

The shapes are alike on purpose: a transcript with a prompt, edits as reviewable diffs.
The LLM layer is the one where lattice owns the request, so you control the model, the
system prompt, what is sent and what each tool may do. An agent integration brings its own
loop and its own tools, which is the reason to pick one: you want that agent. Trust in an ACP
conversation (`<C-t>`) toggles auto-accept for the agent's permission requests; trust in an
LLM chat lifts the diff for edits only and never for commands.

See [`ai-conversation-mode`](help:ai-conversation-mode),
[`ai-permission-mode`](help:ai-permission-mode) and
[`claude-code-mode`](help:claude-code-mode).

## Troubleshooting

| You see | It means |
|---|---|
| A message under the transcript naming a variable | `api_key_env` names a variable the editor was not started with. Export it and restart, then send again |
| `api_key_command ... failed (exit status N)` | The command failed; run it in a shell to see why. Its output is never shown |
| `api_key` is refused | A literal key is in the config. Use `api_key_env` or `api_key_command` |
| A request shown as `retrying` | The provider answered 429, 502, 503, 504 or 529. It is tried up to twice more |
| Request failed saying how long the provider asked to wait | It asked for more than 30 seconds; try again later |
| An ask refused naming a file and a glob | The file matches `llm.exclude`. Change the list if you meant to send it |
| A request refused naming a context item | The item's buffer was closed, its lines deleted, its file is unreadable or excluded. Remove it in `*llm-context*` or reopen it |
| An ask opened a chat, not `below` | The buffer is read-only or a composed view, or the settings allow tools |
| `:llm-retry` refused | A reply is still coming. `<C-c>` first |
| `:llm-save` refused | The chat has not finished replying, or the file exists (use `!`) |
| A resumed chat is refused | The file was edited after lattice wrote it, or written by a newer lattice |
| A tool call "not offered" in `*llm-mcp*` | Its name has characters providers do not accept |
| An MCP server stuck at `failed` | Fix the command, then `r` on its line; it is not retried on its own |
| Tool run stops with a message about rounds | `llm.tool_rounds` was reached; send again to go on |
| `:q` will not quit | A request is running. `:llm-stop`, or `:q!` |
| A project setting ignored | `.lattice/llm.toml` tried a key a project may not set; `:messages` names it |
| The llm chords do nothing in one buffer | That buffer binds the prefix's first key itself (`<C-c>` in `*compilation*`) |

When the cause is not obvious, `:llm-inspect` shows what was sent and `:messages` holds the
warnings the layer raised.

## Where to go next

- [`llm`](help:llm): settings, layers, profiles, project config.
- [`llm-chat-mode`](help:llm-chat-mode): the chat buffer, tools, export and resume, provider setup.
- [`llm-mode`](help:llm-mode): the ask and rewrite operators, destinations, context, `llm.exclude`.
- [`llm-mcp-mode`](help:llm-mcp-mode): external MCP servers.
- [`llm-system-mode`](help:llm-system-mode): editing the system prompt.
- [`ai-conversation-mode`](help:ai-conversation-mode), [`ai-permission-mode`](help:ai-permission-mode),
  [`claude-code-mode`](help:claude-code-mode): the agent integrations.
