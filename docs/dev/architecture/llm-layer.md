# The LLM layer — models as a capability of every buffer

> **Status: 📝 design, awaiting review.** Nothing here is implemented. Substrate
> findings are verified against source; §11 names what only building can settle.
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
| Send up to the cursor | `:llm-send` | New dedicated buffer; inside a chat, that chat |
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
  with that prompt ready to edit. The original stays intact.
- **`:llm-retry`** re-runs the last turn with current settings, which is also how to try
  another model.
- **`:llm-export`** writes `.md` (and `.org`) for keeping. Resuming uses a separate
  session file.

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

**The loop has one way to reach any tool: an MCP client connection.** Lattice's own tools
are an MCP server like any other, reached in-process. Nothing in the loop knows which
server is lattice's.

**That is the end state, and it lands last.** The harness is the priority, so the loop first
talks to a `ToolBroker` holding lattice's tools directly, and MCP arrives behind that same
interface once everything else works. Tool descriptors use MCP's shape from the start (name,
description, JSON Schema input, content result), so nothing above the broker changes when
it does. Until then external servers are unavailable and the Claude Code server is untouched.

	[llm.mcp.github]
	command = "github-mcp-server"	# stdio; or url = "https://…" for streamable HTTP
	env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }

	[llm.profiles.coding]
	tools = ["lattice.read", "lattice.edit", "lattice.exec", "github.*"]

Tools are namespaced by server, and profiles and `:setlocal` select them with the same
syntax. `:llm-mcp` is a buffer listing each server, its state and its tools. A server that
fails to start is reported there and never blocks a request; servers start when a session
first needs them.

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
  behind it directly until `mcp` lands.
- **`mcp`** (last) — built on `rmcp`, the official Rust SDK, for client and server:
  - `Connection`: one MCP client session over any transport: in-process duplex, stdio
    child, streamable HTTP. A tool source for the broker.
  - `LatticeServer`: one `ServerHandler` over `EditorAccess`, with a per-connection
    catalogue. In-process it offers the full set, including exec. Over the existing
    loopback WebSocket it offers exactly today's Claude Code catalogue, so connecting
    `claude` gains nothing it does not have now.

  The current hand-written `dispatch.rs` and `protocol.rs` are replaced by this; `server.rs`
  keeps the listener, lockfile and auth. Tool-call ids and provider data live in the session
  store.
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
- **`rmcp` against Claude Code.** Source review of 3.8.0 is favourable: `CustomNotification`
  for `selection_changed` and `at_mentioned`, a pinnable `V_2024_11_05`, and a sink/stream
  transport for the WebSocket. None of it is proven. The first MCP slice must pass
  `mcp_ws_roundtrip.rs` and a real `claude` session before the rest builds on it. If it
  cannot, `rmcp` stays client-only and the hand-written server stays.
- **Reviewable writes from tools.** Lattice's edit tools need `read_text_file`-style reads
  and writes on `EditorAccess`, against unsaved buffer text rather than disk.
