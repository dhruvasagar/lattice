# The LLM layer — models as a capability of every buffer

> **Status: ✅ built** (LL.1–LL.24; LL.23 dropped; `<leader>aR` and `:llm-send` added for
> the release, see the slice plan's "Release readiness"). The user guide is
> [`../../user/llm-guide.md`](../../user/llm-guide.md).
> Slice sequencing: [`../operations/slice-plans/llm-layer.md`](../operations/slice-plans/llm-layer.md).
>
> **Siblings:** [`agent-integration.md`](agent-integration.md) (the `EditorAccess`
> port, MCP and ACP adapters), [`agent-ui.md`](agent-ui.md) (the ACP transcript
> this deliberately differs from), [`owner-write-caret.md`](owner-write-caret.md)
> (how a caret survives an edit it did not make),
> [`buffer-local-options.md`](buffer-local-options.md).

## 1. Why

Every agent lattice integrates today brings its own loop: `:claude`, `:opencode`
and `:pi` run someone else's program. Nothing lets a user consult a model directly
over its API, from whatever they are looking at, with settings they control. This
layer does that. Coding is its primary use and not its definition: an org file
being drafted, a compiler error and a terminal's output are equally valid places
to ask from.

gptel is the UX reference, because its central idea fits lattice: *sending works
anywhere, and a chat buffer only adds conveniences.* It is input and not a
template. Its per-heading org properties are left out, for one thing, because they
would put org knowledge in a native layer that should not have any.

**Native, in `lattice-ai`, with no plugin seam.** Because the layer works on any
buffer, org gets it without any org-specific surface. The cost is accepted: a
provider bug is not quarantined the way a guest's would be, so off-thread I/O and
no panics on the hot path are hard requirements rather than good manners.

The existing agents keep their behaviour. Internally, `:opencode-acp` moves onto the
shared conversation model (§8) and `:claude` onto the shared MCP implementation (§10).

## 2. One idea, several entry points

Every use is one **request**: *source text + instruction + settings → destination*.
The entry points differ only in their defaults.

| Intent | Invocation | Destination |
|---|---|---|
| Have a conversation | `:llm [profile]` | New chat buffer |
| Ask about text | ask operator + motion, or Visual | New dedicated buffer |
| Send up to the cursor | `:llm-send` | As ask (a chat beside it by default); inside a chat, that chat |
| Change text | rewrite operator + motion, then an instruction | Diff review, never a direct write |
| Script or bind it | `:'<,'>llm-ask explain this` | As ask |
| Have it work on code | `:llm coding` | Chat with tools on |
| Adjust one request | `:llm-menu` | Whatever is chosen |

Commands are dashed and namespaced per the naming rule. Coding is a profile, so
there is no `:agent` command.

Provisional default chords, on an `llm-mode` minor active on every document:

| Chord | Action |
|---|---|
| `<leader>aa{motion}` | Ask |
| `<leader>ar{motion}` | Rewrite |
| `<leader>aA{motion}`, `<leader>aR{motion}` | The same, opening the menu first |
| `<leader>ac{motion}` | Add to context |
| `<leader>as` | Send up to the cursor |
| `<leader>am` | Menu |
| `<leader>al` | Open a chat |

**The prefix is an option, `llm.prefix`** (default `<leader>a`; `<C-c>.` is the emacs-style
alternative), and every chord above is that prefix plus one key. One value, global: a prefix
per buffer would make the same muscle memory do different things in different windows, which
costs more than the flexibility buys. `llm-mode` owns the chords and binds them into its own
keymap layer, and **rebinds them on `OptionChanged`**, not once at install: `lattice.toml` is
read after boot, so a prefix read at install would only ever see the default. The layer is the
mode's alone, so a rebind drops the layer and binds afresh under the new prefix, and nothing
under the old one can survive.

The prefix alone opens no menu of its own. Held, it shows which-key's popup, which reads the
same layer the dispatcher walks, so it can never list a chord that will not fire. A real
transient on the bare prefix was weighed and deferred: it needs the prefix node to be both
bound and a prefix, resolved by a vim-style `timeoutlen`, and an effect that starts an
operator from a menu row. Both are generic host mechanisms (which-key.md §7 names the
escalation as its intended follow-up), not llm features. Until then, a buffer that binds the
prefix's first key by itself (`<C-c>` in `*compilation*`) shadows the llm chords there, by the
trie's terminal-wins rule.

## 3. The dedicated buffer

**A dedicated buffer is a transcript with a prompt.** It is a major mode implying
`repl-mode`: history is read-only, only the prompt at the bottom is editable, and the
insert-entry keys jump to it. `<CR>` sends and `<C-j>` adds a newline, as in
`:opencode-acp`. Motions, search and yank work over the whole transcript. A structured
session store is the source of truth and the buffer is its projection, so tool-call ids
and signed reasoning survive intact because no edit can reach them.

An editable-document chat was approved first and then reversed. Its benefits are
available in the user's own buffer through `destination=below`, and it needed a hidden
ledger tying provider data to text that could change. **Chats are transcripts; documents
are your own buffers.**

What in-place editing gave is replaced by:

- **Fork:** a chord on a past turn opens a new chat holding the history up to that point,
  with that prompt ready to edit. The original stays intact. The chord is `gb` (branch);
  `:llm-fork` is its ex-command. On an answer it forks from the question the answer
  belongs to. The copy is the model's, not the buffer's: reasoning data (keyed by turn
  index, which a prefix keeps) and the arguments of the tool calls those turns made come
  along, so the fork's first request is as valid as the original's next one would be. It
  starts from the source's settings and context like any chat made from a buffer; trust and
  "allow for this chat" are per-chat grants and are not copied. The prompt is seeded by the
  same replace that seeds the transcript, so there is no frame with an empty prompt.
- **`:llm-retry`** re-runs the last turn with current settings, which is also how to try
  another model. It drops the last reply (and the reasoning and call arguments only it
  held) and sends the question again: turns are removed only from the end, so nothing kept
  is re-keyed. It is refused while a reply streams; stopping is a separate decision, and
  retry silently interrupting would lose the reply the user may still want.
- **Changing provider** leaves another provider's reasoning out of the request: only the
  provider that signed it can verify it. The adapter already drops it; the chat says so
  once per chat and provider, in `*messages*`, beside the other "not sent" notes.
- **A signature on a call, not on reasoning.** Gemini signs its thinking on a reply part,
  usually the first function call, often with no thought text at all; newer models reject a
  call sent back without its signature, which would break a tool loop on round two. Round-trip
  data lives only on reasoning blocks, and that is kept: the adapter opens an empty reasoning
  block before a signed call and its data names the call, so building the next request puts
  the signature back on that call. An empty block is data only: the projection and the org
  export show nothing for it. A signature on a text part is optional and is dropped, since
  keeping it would split the reply's text around a block of its own. Rejected: a signature
  field on `Block::ToolCall`. It would make one provider's wire detail part of the shared
  model ACP also uses, for a case the existing keyed map already holds.
- **`:llm-export`** writes `.md` (and `.org`) for keeping. Resuming uses a separate
  session file: `:llm-save` / `:llm-resume`. The markdown is the projection with every
  block open; org is built from the model, not by re-reading the markdown, so prose that
  reads `you:` is never a turn. The session file is versioned JSON with its own shapes (the
  in-memory model can change without breaking a kept file) and keeps what the wire needs
  exactly: tool-call ids and arguments, reasoning data. Its settings are the chat's
  *effective* ones (a model from a profile comes back without the profile) and go back
  through the same trust filter as project config, since a file may be downloaded. Its
  checksum (FNV-1a over the session's JSON) catches a file edited after it was written,
  whose history may no longer be what a provider signed; it is not a signature, and what a
  forged file can do is bounded by the trust table, not by the checksum. Encode, decode and
  render run on the blocking pool: the editor's runtime is single-threaded, and a plain
  spawned task would run them on its thread.

Other properties of the buffer:

- **A new buffer per request.** Every question gets a clean history and requests
  overlap freely. Rejected: one buffer per source (unrelated questions share a
  history and must queue) and one reused `*llm*` (destroys the previous answer).
- **Focus stays where it was.** The buffer opens in a split and repeat asks reuse
  that pane, through the generic `Effect::DisplaySyntheticBuffer` (design.md §5.10.5),
  not an LLM-owned pane rule. `gq` closes it. They are named `*llm:foo.rs:3*`, are found through a
  picker, and an unsaved one does not block `:qa`.
- **The source text is the first turn,** folded when long, with a link back, so
  what was sent is visible.
- **To continue an existing chat, say so:** `destination=<buffer>`, or pick one in
  the menu.

Writing into an existing buffer is opt-in (`destination=below`, as a `writer`
profile would set). Files are untouched by default. Text sent from such a buffer has no
user and assistant turns, so in v1 it goes as a single message.

### What a transcript looks like

**The transcript is markdown, highlighted by `markdown-mode`'s grammar.** Fenced code gets
its language through the existing injections. Comparison:

| | Markdown | Org |
|---|---|---|
| Lattice support | Native in `lattice-syntax`, with fenced injections and folds | A plugin; the native layer cannot depend on it |
| What models emit | Natively | Must be converted while streaming, which is fragile |
| Other tools | gptel's default chat mode | gptel's optional mode |

Org users still get org: `:llm-export` converts, and `destination=below` in an org file
writes into it.

Because the buffer is a projection, its text stays valid markdown at every moment. **An
unclosed fence mid-stream is closed by the projection**, so one half-received code block
cannot colour the rest of the transcript and the prompt as code. Tool output uses a fence
longer than any it contains.

**Blocks.** A tool call is a head line that stays visible when folded, plus fenced bodies
that get their highlighting from the existing injections. The input is `json`. The output
takes its language from the tool's result where known (a file's language, `diff`, `bash`),
then `json` if it parses, otherwise `text`.

	▸ github · search_issues  [ok]
	```json
	{ "query": "fold overlap" }
	```
	```json
	{ "total_count": 3 }
	```

The head line sits outside the fence so a folded block still reads as a sentence and its
status can change in place. Reasoning is prose, so it stays a prefixed foldable block
rather than a fence. A tool's origin (lattice, an MCP server) is a label on the same block,
not a separate kind.

Per block type, as ordinary typed options settable globally, per profile or per session:

| Option | Values | Default |
|---|---|---|
| `llm.fold.reasoning` | `closed`, `open`, `hidden` | `closed` |
| `llm.fold.tools` | `closed`, `open`, `hidden` | `closed` |

`hidden` removes a block from the projection and not from the store, so changing it brings
the blocks back. Folds use `Fold::identity` as `tool_fold.rs` already does, so an expanded
block stays expanded as text arrives. `za` toggles one block.

**Everything-is-a-buffer does the integration work.** A read-only source
(`*compilation*`, a terminal) cannot take inline output, so its reply goes to a
dedicated buffer and the echo area says so.

## 4. Settings

**A session is a buffer.** There is no session object to manage. Rejected: one
session per project (cannot vary per buffer) and named sessions that buffers attach
to (a concept nothing needs yet; sending several buffers to one chat already shares
settings).

Which session a request uses:

- New dedicated buffer: settings are **copied at creation**, from the requesting
  buffer's session plus the request's overrides. Nothing stays linked.
- A named existing chat: that chat's settings.
- Inline, rewrite and other non-chat destinations: the source buffer's session.

**Layers, lowest to highest:** defaults → global config → profile → project →
session → request.

A **profile** is a named bundle: model, system prompt, parameters, tools,
permissions, default destination. It supplies starting values and nothing is fixed
by it.

	[llm]
	default_profile = "general"

	[llm.providers.anthropic]
	api_key_env = "ANTHROPIC_API_KEY"	# or api_key_command; never a key in TOML

	[llm.providers.local]
	kind = "openai-compatible"
	base_url = "http://localhost:11434/v1"

	[llm.profiles.writer]
	system = "~/.config/lattice/llm/editor.md"
	destination = "below"
	tools = []

### Overriding on the go

Model and provider, system prompt, temperature, max tokens, reasoning effort,
messages sent, context on/off, tools, permissions and destination are all
changeable four ways:

1. **`:llm-menu`** shows each effective value and its origin, for the session the
   next request would use, and names it (`new session from foo.rs`). `=` switches
   scope between **session** (default) and **next request**, and stays switched.
   `P` saves the current values as a profile, `x` clears overrides.
2. **Command arguments,** for one request:
   `:'<,'>llm-ask model=local/qwen temperature=0 explain this`.
3. **`@profile` in a prompt,** for one request. Only known names match. Only the
   last turn is scanned, which with an operator means only the typed instruction,
   never the selected source.
4. **`:llm-system`** opens the system prompt in a normal buffer and `:w` applies
   it. With a range it uses the selection.

Session values are ordinary typed options: `:setlocal llm.model=…` works and
`:set llm.model?` shows the origin. There is no `:llm-set`.

**Rules.** A change applies to the next request, never to one in flight. Switching
model keeps history; switching provider drops provider-specific reasoning data, and
says so. A setting a model does not support is omitted, and says so. Switching
profile replaces the base and clears session overrides, echoing what it cleared.
Session files keep a chat's settings; a source buffer's overrides last until it closes and
are never written into its text.

### Who may set what

Some sources of settings are not necessarily the user.

| Source | May set | May not set |
|---|---|---|
| Menu, ex-commands, global config | Everything | — |
| `@profile` in a prompt (may be pasted) | Model, system prompt, parameters | Wider tools or permissions; asks first |
| Project config, session files (may be cloned or downloaded) | Model, system prompt, parameters, narrower tools | `base_url`, key commands, wider tools, trust, MCP servers |

Without this, opening a file from a cloned repo could send an API key to that
repo's server, enable exec, or start a program, since a stdio MCP server is a command to
run. The cost: a project cannot pre-enable tools or declare MCP servers.

## 5. Requests outlive their surfaces

A request has its own lifecycle and each surface only displays it. Two commands
follow:

- **`:llm-requests`** lists everything running, with its target; `dd` cancels.
  `:llm-stop` stops the current buffer's request or the only one running, and opens
  the list rather than guess when that is ambiguous.
- **`:llm-last`** reopens the latest result. Echo, register and rewrite output keep
  a hidden session buffer, so the full response can always be seen and followed up.

**Tool use needs a dedicated buffer.** A tools-enabled request aimed at `below`,
echo or a register moves to one and says so. Tool permissions: reads run, edits go
to diff review, exec asks; `<C-t>` trusts for that session.

**Privacy: nothing is sent unless it can be seen first.** `:llm-context` is a buffer
listing what is attached (`dd` removes), and `:llm-inspect` shows the exact payload.
A `llm.exclude` glob list refuses matches such as `.env`.

**Context** (LL.15) is attached per buffer, like session settings: asks from a buffer carry
its list, and a session made from it (a chat, an ask's buffer) takes a copy and owns it from
then on, so attaching in one chat never leaks into another. Items are a whole buffer, a span,
or a file, and all are read when the request is sent: a span is a tracked range in its
document, so it follows edits (linewise spans grow at their edges, as typing on their lines
should; charwise ones keep to what was chosen). An item that cannot be read as the list
shows it (buffer closed, span deleted, file unreadable, now excluded) **fails the request**
rather than being skipped: the list is what the user agreed to send, and sending less would
make it a lie. The block heads the request's last user message, so each request carries
the context once and current, and earlier turns go as they were.

`:llm-inspect` in a chat builds the next request through the same preparation the send
uses (settings, project file, system prompt, context) and shows it with the key's place
built as `<redacted>`, so no key is read to show it; elsewhere it shows the last request
sent from the buffer. The body is shown pretty-printed; compacted, it is byte for byte what
the provider received (pinned by the TUI tests against a recording mock).

Rejected: one project-wide context list (asks from unrelated buffers would share it, and a
chat could not narrow it); reading text at attach time (the model would see stale text with
no sign of it); skipping an unreadable item with a warning (a silent narrowing of what the
user saw listed).

### Edge cases

| Situation | Behaviour |
|---|---|
| Inline: typing above the insertion point | The insertion point moves with the text |
| Inline: typing at the insertion point | The response stays contiguous; typed text stays after it (§9) |
| Inline: the insertion point's text is deleted | Stops; partial text stays; echo explains |
| Inline: `u` while streaming | Cancels, then undoes (§9 for how far) |
| Chat: caret in the prompt while text streams above | The caret stays in the prompt, by the existing owner-write adoption |
| Inline: read-only source | Dedicated buffer, and says so |
| Inline: second request at one point | Refused, pointing at `:llm-requests` |
| Rewrite: source changed while waiting | Accepting does not apply: the diff reopens against the text as it is now, with the accepted text as the proposal. Accepting that is the deliberate confirm, and it shows what it overwrites, which a yes/no dialog cannot (LL.11b) |
| Rewrite: source buffer closed | Result moves to a session buffer, so the answer is not lost |
| Rewrite: blockwise, or across multibuffer files | Refused up front, with the reason |
| Rewrite: repeated by `.` or a macro | Diffs queue and open one at a time |
| Dedicated: hidden | Keeps streaming; modeline shows it; notifies when done |
| Dedicated: closed | Cancels |
| Failure with nothing received | Dedicated buffer shows the error; elsewhere the echo area, with no text left behind |
| Failure partway | Partial text stays, with a visible "interrupted" marker |
| `:q` with requests running | Warns; a restored session never resumes one silently |
| Oversized source | Asks first, over a threshold |
| Macros | Requests are asynchronous; the macro does not wait |

## 6. Tools and MCP

**The loop has one way to reach any tool: the `ToolBroker`.** Behind it sit tool sources:
lattice's own tools (`LatticeTools`, called directly) and one MCP client connection per
external server. Tool descriptors use MCP's shape (name, description, JSON Schema input,
content result), so the broker treats the two alike, and nothing in the loop knows which is
which.

**Lattice's own tools are not served to the loop over MCP.** The plan was once to move them
onto `LatticeServer` and reach them through an in-process MCP connection (LL.23, dropped
2026-11-08). Their per-call context does not fit the protocol: which chat a review belongs
to, whether the chat trusts edits, the review opening and closing (it drives
`AwaitingReview`), and the one-line `describe` the permission prompt shows. Carrying those
would take protocol extensions whose only consumer is lattice itself, and a serialization
round trip (about 58 µs) on every call, for no gain: no external client wants the full
catalogue, and the client path is exercised by every external server anyway. Revisit if an
external MCP client ever should drive lattice's full tool set.

	[llm.mcp.github]
	command = "github-mcp-server"	# stdio; or url = "https://…" for streamable HTTP
	env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }
	allow = ["get_issue"]	# runs without asking

	[llm.profiles.coding]
	tools = ["lattice.read", "lattice.edit", "lattice.exec", "github.*"]

Tools are namespaced by server, and profiles and `:setlocal` select them with the same
syntax. `:llm-mcp` is a buffer listing each server, its state and its tools. A server that
fails to start is reported there and never blocks a request; servers start when a session
first needs them.

**External servers (LL.24)** are `tools::mcp::McpServer`, one per table, each a tool
source registered with the broker at boot and started by the first offer that selects it.
Starting (spawn or connect, `initialize`, `tools/list`) is bounded at 10 s, and the broker
lists its sources together, so one slow server costs a request one bound, not one per
server. A server that **failed** to start stays failed until `r` in `*llm-mcp*`: a wrong
command fails identically every time, and retrying it per request charges each request
the attempt. A server that was **running** and exited is started again by the next
request that needs it, since a crash after a good start is more likely transient. A call
in flight when its server dies is that call's error. Dropping a call (the request
stopped) sends MCP's `notifications/cancelled` and leaves the server up; stopping or
restarting a server, a failed start, and the connection ending kill a stdio child's whole
process group (`lattice_core::process::{own_process_group, kill_process_tree}`), so a
server behind `npx` or a shell leaves nothing running.

`allow` is per server and names tools exactly; it lives in the same user-only table as
the command, so a project file, which cannot declare a server, cannot allow one either.
`env` and `headers` values expand `$NAME` / `${NAME}` when the server starts; an unset
variable fails the start by name. Values are never shown, and the child's stderr is
scrubbed of them before it is logged or shown. A tool whose wire name providers would
reject (`[A-Za-z0-9_-]`, at most 64 with `server__` in front) is listed in `*llm-mcp*` as
not offered rather than renamed: a renamed tool is one the server does not know.

Rejected for external servers: retrying a failed start on every request (above); killing
the server when a call is cancelled (other chats share it, and MCP has a cancellation
message for exactly this); an `allow = ["*"]` wildcard (it would make a server's whole,
changing tool list run unasked, including tools added after the user wrote the line);
believing `readOnlyHint` to skip the question (it is the server's claim, §6's table);
renaming tools with unusable names (calls would carry a name the server never listed).

**Trust is decided by us, not claimed by the server.**

| Tool | Permission |
|---|---|
| `lattice` read tools | Run |
| `lattice` edit tools | Diff review |
| `lattice` exec | Asks |
| Any external tool | Asks, unless allowed by name in user config |

MCP's `readOnlyHint` is a claim by the server, so it is shown and never trusted. An external
tool's edits happen in its own process, so they cannot pass through diff review, only the
prompt before it runs. Tool results are untrusted input to the model, and a result cannot
widen permissions.

**The loop is one request** (LL.16). An exchange that ends asking for tools moves the request
to `AwaitingTool`; the calls run; the next exchange goes out under the same id, state and
cancellation token, so `:llm-stop` stops the whole loop wherever it is, and `:llm-requests`
shows one row for it. The broker makes an **offer** when the request starts: the selection
resolved once into the tools it may call. The loop calls through the offer and nothing else,
so a result, being only text, has no path by which to add a tool or loosen a permission; a
call to a name the offer lacks is refused by name.

**The conversation store is the record, and the next exchange is built from it.** Each call's
name and exact arguments sit in the session (`Conversation::tool_calls`, beside reasoning
data), its result in the tool block; `history::messages` projects both, splitting a turn at
each run of calls and answering **every** call it sends. A call stopped before it returned is
answered with an error result, so the history is valid however the loop ended, and the
request continuing a loop is the same projection as the one a later prompt sends. A call
whose arguments never finished arriving is dropped with nothing to answer. Each continuation
is the first request as sent plus everything after its last user message: settings, system
prompt and context are not resolved again, so a loop keeps the terms it started with.

**Names and selection.** A tool is `server.tool` wherever a person writes it and
`server__tool` on the wire, since providers allow only `[A-Za-z0-9_-]`. Lattice's tools are
namespaced like any server's, so a later server cannot collide with them. A selector is
`server.*`, `server.name`, or a kind group (`read`, `edit`, `exec`) that applies only where
kinds are trusted, that is, to lattice's own tools.

**Lattice's read tools** (`read_file`, `list_directory`, `search`) answer from the buffer
when the file is open, unsaved edits included (`EditorAccess::read_text_file`), and are
confined to the project root and outside `llm.exclude`. They run without asking, and a
result is untrusted input that can carry instructions, so a read that could reach `~/.ssh`
would let any file a model reads exfiltrate it. Outputs are capped with a note saying how to
read on. A request runs at most `llm.tool_rounds` rounds (default 25), then stops with a
message.

Arguments a tool cannot use come back as an error result, never a failed request: arguments
that are not a JSON object are kept as `{}` (a provider rejects a call whose input is not an
object) with the reason, and the call is not run.

**Lattice's edit tools** (LL.17: `edit_file`, an exact `old_string` → `new_string`
replacement that must match once unless `replace_all`; `write_file`, a whole file, new or
not) never write what the user has not seen. Each proposal is a buffer-text review
(`lattice_agent::review_text`) against the file **as the user has it**: the buffer's text
when open, else the disk's, else empty. What the user accepts, edits included, is applied
to that same text: into the buffer as one minimal edit (`EditorAccess::apply_buffer_text`
replaces only the differing span, so the lines around it stay pixel-stable and one `u`
undoes it), unsaved; or to the disk, only if the file still holds what was reviewed. A
target that changed meanwhile, on disk or in a buffer, is refused with "read it again":
applying would overwrite a change the user made. The verdict is the tool's result — written,
applied in the editor, rejected, closed undecided, or accepted after the user changed it,
which tells the model to read the file before its next edit.

Reviews queue at **one gate across the llm layer**, shared with the rewrite operator: a
second diff opening over the first would take the focus mid-decision. While one is open the
request is `AwaitingReview` (a round reports it through `RoundStatus`), then `AwaitingTool`.

**Every review carries its session's tag**, `origin_session` on the diff, numbered from
`ORIGIN_BASE` (2⁶²) so it can never meet an IDE connection's id, which counts from 1. A
review whose call is dropped undecided (the request stopped, the chat closed, which stops
its request) closes the session's diffs through `Effect::CloseAllSessionDiffs`, the
diff-only variant: a verdict that would reach nobody must not stay on screen asking for one.

**`<C-t>` in a chat trusts its edits**: they apply without a diff, and the headerline says
`edits trusted` for as long as it lasts. Trust belongs to the chat on screen: never saved,
never copied to another session, and dropped when the chat closes. The stale-target refusal
still applies, so trust skips the question, never the check. An edit inside a `.git`
directory is reviewed even when trusted: hooks and `core.fsmonitor` there run code later,
and nothing under `.git` appears in `git status` or `git diff`, so no later look at the
work would catch it. Other files that run later (`build.rs`, a Makefile) do show there.

Rejected for edits: writing the disk under an open buffer (the buffer would show text the
file no longer holds, and `:w` would silently undo the model's change); a whole-buffer
replace on accept (every line would repaint and the caret would jump, breaking the
keystroke UX contract for content the user did not edit); a per-tool review queue (two
diffs at once, one from a rewrite, would fight for the focus); trust as an option
(`llm.trust`) a profile or project file could set — §4's table puts trust out of a
project's reach, and a key in the chat it applies to is the narrowest grant.

**Lattice's exec tool** (LL.18: `run_command`, selected as `lattice.exec`) asks before
every run. The question is the same popup an ACP agent's permission request uses, and
the request is `AwaitingReview` while it waits. It names the tool and shows the command
(`$ cargo test`, plus `in <dir>` when the model gave one), with three answers: allow
once, allow for this chat, deny. A denial is the tool's result ("the user denied
running …; do not try it again unless they ask"), so the model learns why and the loop
continues. Deferring (`<Esc>`) leaves the question pending: `:ai-permission` reopens it.
A request stopped while it waits withdraws the question as a denial, so a stale popup
can never run anything.

The command runs in `sh -c` (`cmd /C` on Windows) at the project root, or a `cwd` inside
it, through `lattice_core::process::run_cancellable_capped` in its **own process group**:
`:llm-stop`, `<C-c>` or closing the chat kills the whole tree, not just the shell. It
times out (120 s by default, up to 600 s if the model asks), and each stream keeps its
first 30 000 bytes and drains the rest, so a noisy command can neither fill memory nor
block on a full pipe. The result carries the exit status, both streams, and a line
saying where each was cut.

**"Allow for this chat"** lives on the chat session, like `<C-t>`: never saved, never
copied, cleared when the chat closes. For most tools it covers the tool. For a tool of
the `Execute` kind it covers the exact call (the command and where it runs), offered as
"Allow this command for this chat": allowing `cargo test` must not allow whatever a
prompt-injected tool result asks for next, while re-running the same tests still asks
once. `<C-t>` trusts edits only; it never
lets a command run unasked, because a command's effects cannot be reviewed or undone.
Exec is off unless the user's own config, a profile, or `:setlocal` selects it; §4's
table already keeps a project file from widening `llm.tools`, and that holds for
`lattice.exec`.

**One permission popup, many producers.** The popup moved from the ACP module to a
neutral `permission` module and reads a `PermissionSources` registry: each producer adds
a closure that lists its conversation stores (ACP adds its one store; the llm layer adds
every chat's). The popup shows the oldest pending request across all of them and answers
it on the store that holds it. The broker reaches the user through `CallContext.ask`,
which returns `Answer::{Once, ForChat, Deny}`; the chat's asker turns that into a
pending permission on its store. A tool that needs asking with no asker attached is
refused, never run.

Rejected for exec: running commands through the user's terminal buffer (the output
would be the user's scrollback, not a result, and a stop could not tell the model's
process from the user's); trusting a command prefix ("always allow `cargo`") in this
slice (prefix rules are a policy language, and getting one wrong runs arbitrary code;
the exact-command "allow for this chat" covers the repeated case without a parser); a second popup for
llm questions (two menus for one decision, and only one could be focused when both
fire).

Rejected: a request id per exchange (the modeline, `:llm-requests` and `:llm-stop` would
see a loop as unrelated requests, and a cancel between exchanges would find nothing to
cancel); accumulating the loop's messages beside the store (two records of one conversation,
and the store's is the one a later prompt sends, so a divergence would be a bug that only
shows on the next turn).

## 7. UX contract and scope

Streaming never moves the caret; the view follows only if the caret is already at
the end of the response. Only streamed lines change on screen, and syntax colour
may catch up. `:llm-stop` works everywhere and `<C-c>` in a chat. The chat
headerline carries model, profile (`*` when overridden), state, tokens and cost; the
modeline carries only whether a request is running.

**Not in v1:** ghost-text completion, images, org-specific branching or per-heading
settings, and OAuth for remote MCP servers (a token from an environment variable or
command works).

## 8. Substrate — what exists, verified against source

| Need | Exists | Consequence |
|---|---|---|
| A span that follows edits | `lattice_core::tracked` — `TrackedRanges` in `Document`, transformed in the edit loop, with `Stickiness` and `collapsed`. Used by snippets | The anchor. `owner-write-caret.md` §2 predates it and still says none exists |
| Addressed write from off-thread | `apply_edit_batch` on the actor handle bypasses the read-only gate | Bypasses it for user files too, so the read-only refusal must be ours |
| Caret survives an owner write | OWC ✅, but `should_adopt_owner_write` applies only to buffers with an editable tail | Free in chat. Must be extended for inline output in ordinary buffers |
| REPL input surface | `repl-mode`, `EditableTail`, `run_editable_tail_invocation` | Direct reuse |
| Prompt anchor | `AiConversationMode` holds **one** process-wide `AtomicU32`, because v1 had one buffer, and `Mode::editable_tail` takes no buffer. The host asks only the major | Must become per-buffer. Fixes ACP too. The chat must be a major |
| Tool and reasoning folds | `tool_fold.rs`: closed by default, identity-stable while streaming, hardcoded `closed: true` | Reuse; closed state becomes an option |
| Markdown highlighting | `lattice-syntax`, block + inline + fenced injections. The language is normally bound by major (`lang_for_mode_id`), and `ai-conversation` has none today | The chat major needs a binding that is not `markdown-mode` itself, whose `<CR>` follows links |
| Wake without a keypress | `SubsystemBoot::inbound` | The route for effects; document writes publish on their own |
| Menu that stays open | `TransientItemKind::Flag` and `Argument` (with picker sources) re-seat the menu; `Action` closes | No new menu needed. Open: state is per-menu, projected into a command's args, so session-scope writes need an apply path |
| Custom operators | `register_operator` + `OperatorSpec`. But chord composition (`register_operator_bindings_in`) is in `lattice-host`, and `surround` is bound from `editor_boot` | `lattice-ai` cannot reach it, so `SubsystemBoot` needs a generic way to bind an operator under a mode's layer |
| Operators on read-only sources | `run_read_only_motion` allows `y` by id. Its comment says a `mutates` bit is "worth adding when a second non-mutating operator appears" | Ask is that operator: add `OperatorSpec::mutates`. Rewrite stays refused |
| Highlighting without being that language | `Editor::lang_for_major` falls back to the mode registry's language index, which is how a plugin major with no file path gets a grammar | The chat major declares markdown there |
| Not blocking quit | `*messages*` and `*ai:opencode*` are synthetic-name Documents | Dedicated buffers follow them, not `ephemeral`, which also hides from `:ls` |
| Option origin | `OptionOrigin`: `Default`, `GlobalConfig`, `BufferLocal`, `ModeContribution` | Needs `Profile` and `Project` variants |
| Diff review | `review_diff`, `ProgrammaticDiffBus` | Reuse, extended: the bus diffed disk and saved on Accept, wrong for a buffer with unsaved edits or no file. LL.11a adds a text baseline and an unsaved, returned Accept (`review_text`, agent-integration.md) |
| Cancellation | `CancellationToken` in `lattice_protocol` | Direct reuse |
| Permission popup | `ai-permission-mode` | Built on ACP's `PermissionOption`; must be made protocol-neutral |
| Off-thread execution | `boot.runtime_handle()` is `lsp_runtime()`, a multi-thread runtime. The editor actor itself is `current_thread`, so a bare `tokio::spawn` from a handler lands on the actor thread | Requests spawn on the boot handle, explicitly, and a test pins it |
| HTTP | `ureq` (blocking, for plugin downloads). `reqwest` is present only as `zed-reqwest` via GPUI | A new dependency (§10) |

**The ACP `Conversation` store is reused, made protocol-neutral.** With a transcript it is
the same shape as this layer's session. The coupling is concentrated: `conversation.rs`
carries most ACP references (`SessionUpdate`, `ToolKind`, `PermissionOption`), while
`conversation_mode.rs` and `tool_fold.rs` have few. ACP becomes an adapter that folds into
the neutral model, as the LLM session does. `:opencode-acp` must stay green throughout.

**`EditorAccess` is smaller than `agent-integration.md` §2 says.** Source has no
`read_text_file`, `write_text_file` or `review_diff` methods on it. That document is
stale; the tool surface has to add reads and writes.

## 9. Two write paths

**Chat** projects from the store, as `ai-conversation-mode` does today, because tool
status updates in place and a pure append cannot express that. **Inline** (`below`) is
plain text with no store, so it appends. They do different jobs, but they are two paths.
The rest of this section is inline only.

**Anchor.** Each stream owns one `TrackedRange`, empty at the insertion point, marked
`NeverGrows`, so user text at its edges stays outside it.

**The write must be atomic with the anchor.** Reading the range from a snapshot and
then sending an edit races any user edit landing between the two, so a plain
`apply_edit_batch` can write at a stale offset. The write has to resolve the anchor
inside the document actor's mailbox turn: a new operation, append text at the end of
tracked range `id` and grow the range. If the range is collapsed, the write is refused
and the stream stops (§5).

**Contiguity corrects an earlier statement.** The UX review said typed text "stays
ahead of the stream, which continues after it". That would interleave the user's
characters into the response. With the anchor above, the response stays one contiguous
span and typed text stays after it, pushed down as the response grows. The caret goes
with it by OWC's transform (at-or-after shifts), once adoption is extended to these
buffers (§8).

**Undo is not free, and a single `u` is not always the whole response.**
`begin_undo_group` is one coalescing flag per document, already used by Insert sessions,
so a stream holding it would absorb the user's edits and be closed by their `<Esc>`.
Stream writes therefore cannot use it. They amend their own undo entry while it is still
the top of the stack; a user edit in between seals it and the next write starts another.
One `u` removes the whole response when nothing interleaved, and the latest segment
otherwise. Amending a buried entry was rejected, because it breaks linear undo. A user's
redo stack is cleared by a stream write, as by any edit.

**Pacing.** Chunks coalesce to at most one write per frame interval on the off-thread
task. Only the tail changes, so the keystroke contract holds; syntax may catch up. A
bench covers sustained streaming beside typing.

## 10. Shape (draft)

Everything lives in `lattice-ai/src/llm/` behind a `llm` cargo feature, by heuristic #6:
`lattice-ai` owns this domain and a separate crate would only group files. The feature
carves off the HTTP dependency, as `acp` and `mcp` do theirs.

- **`provider`** — a no-I/O adapter per wire format (Anthropic, OpenAI-compatible,
  Gemini): build a request, decode a stream event. Tested without a socket.
- **`transport`** — `reqwest` with rustls, async, because dropping the future is what
  makes cancelling a stalled stream immediate. `ureq` blocks in a read a cancel cannot
  interrupt. The compile cost is real and feature-gated.
- **`request`** — the lifecycle (§5): id, state (`Pending`, `Streaming`, `AwaitingTool`,
  `AwaitingReview`, terminal), target, cancellation token, usage. An `ArcSwap` snapshot
  and a typed bus event feed `:llm-requests` and the modeline.
- **`sink`** — where output goes: document span, review, echo, register. Surfaces differ
  only here.
- **`settings`** — resolves §4's layers, enforces the trust table.
- **`session`** — the protocol-neutral conversation model shared with ACP, with the
  projection and folds.
- **`tools`** — `ToolBroker`: holds a session's tool sources, namespaces their tools, applies
  §6's trust table and the profile's selection, and is all the loop sees. Lattice's tools sit
  behind it directly (§6).
- **`mcp`** (last) — built on `rmcp`, the official Rust SDK, for client and server:
  - `Connection`: one MCP client session to an external server, over a stdio child or
    streamable HTTP. A tool source for the broker.
  - `LatticeServer`: one `ServerHandler` serving the Claude Code catalogue over the
    loopback WebSocket, so connecting `claude` gains nothing it did not have before.

  The hand-written `dispatch.rs` and `protocol.rs` were replaced by this in LL.22; `server.rs`
  keeps the listener, lockfile and auth (see `ide-protocol.md` §4 for what changed on the
  wire). Tool-call ids and provider data live in the session store.
- **`modes`** — `llm-mode` (minor, global on documents: operators, shared chords) and
  `llm-chat-mode` (major, implies `repl-mode`). Both own their chords and handler bodies;
  buffers are created through `ModeActivator`.

The acid test holds: no new `Editor::` methods and no new host `Action` variants. The
expected exceptions are all generic: the document actor's write operation (§9), a
per-buffer `editable_tail`, owner-write adoption beyond editable tails and off a keystroke (on
the editor's wake, LL.13b), the document actor
rebasing a host write positioned before an append (LL.13a, `owner-write-caret.md` §10), a language binding
for a non-language major, `OperatorSpec::mutates`, `OperatorSpec::repeatable` made real (LL.11c:
a non-mutating operator `.` repeats, its prompt replayed pre-filled), operator binding on `SubsystemBoot`, the
`OptionOrigin` variants, a quit guard (`lattice_mode::QuitGuards`, LL.14d: `:q` refuses while
a subsystem's check names work it would lose, so a request in flight is never dropped silently),
a displayed-buffer query (`BufferStore::is_displayed`, LL.14d: the host records which buffers
the current tab shows on each publish, so a chat answering behind a closed window can say so and
one in view stays quiet), and `Mode::write_action` (a form buffer such as `:llm-system` is
submitted, not saved, so a mode must be able to claim a bare `:w` the way it claims `gr`
through `refresh_action`, rather than the host knowing system prompts).

## 11. Open questions

Each of these is settled by the slice that needs it, and none changes the UX above.

- ~~**`.` on an operator under `<leader>a`.**~~ **Settled by LL.10b: `.` does not repeat an
  ask.** `.` repeats the last change and an ask changes nothing, as `y` changes nothing
  (strict vim semantics, paramount #3); a repeat would also re-send a prompt typed once. The
  host already decides this from the effect (`effect_mutates`), so no flag is needed. Rewrite
  (LL.11) is a change and is the operator `.` should reach. **Settled by LL.11c:** its effect is
  a prompt, so the host also records an operator declaring `OperatorSpec::repeatable`, and `.`
  reopens the prompt pre-filled with the instruction typed last time (Dhruva, 2026-11-07). A macro replays the ask's
  invocations and does not wait for the request.
- **Headings and folds.** A reply's own `#` headings produce section folds that overlap the
  block folds. Turn labels stay plain lines for this reason. Confirm `zM` and `za` compose.
- **`rmcp` against Claude Code. Settled by LL.22 (3.6.0).** It carries the contract: the
  version pins to `2024-11-05`, the pushed notifications share the socket through our own
  writer, and requests run concurrently. One cost: rmcp requires a complete `initialize`
  (`capabilities`, `clientInfo`) before any request, which the hand-written server never
  checked. `mcp_ws_roundtrip.rs` sent a partial one and skipped it in the `openDiff` test;
  both now send the real client's handshake rather than a shim keeping lattice lenient for
  a test (Dhruva, 2026-11-08). The real `claude` session passed (recorded in the LL.22
  slice plan and commit); it also exposed a pre-existing handshake bug, the unechoed `mcp`
  subprotocol, fixed on its own.
- **Reviewable writes from tools.** Lattice's edit tools need `read_text_file`-style reads
  and writes on `EditorAccess`, against unsaved buffer text rather than disk.
