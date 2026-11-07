# LLM layer — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons. Design contract:
> [`../../architecture/llm-layer.md`](../../architecture/llm-layer.md), read it first; this
> plan argues from it and does not restate it.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.

**Status: ✅ LL.1–LL.9 built (first usable version); 📝 LL.10–LL.24 planned.**

**Goal:** direct model access from any buffer, with a REPL chat, per-session overrides,
reviewed tools, and MCP behind one tool interface.

**Architecture:** native in `lattice-ai/src/llm/` behind a `llm` cargo feature. No-I/O provider
adapters, a request lifecycle independent of surfaces, output sinks, and a `ToolBroker` that
is all the loop sees. The conversation model is shared with ACP.

**Tech stack:** Rust, tokio, `reqwest` (rustls, streaming), `rmcp` (LL.22 on), tree-sitter
markdown through the existing grammar.

## Global constraints

Each slice implicitly includes these.

- **One slice, one commit**, committed as it goes green, with a message saying why it exists.
  A slice that cannot compile without its neighbour lands with it, and the message says so.
- **Verify with targeted tests** plus `cargo check --tests`. Whole-crate gates run once per
  batch, one crate at a time, with `CARGO_BUILD_JOBS=3`, and never without asking Dhruva.
- **Before each commit:** `scripts/precommit.sh <touched-crate>...` for changed Rust. Docs-only
  commits skip it. Add `lattice-ui-gpui` and `lattice-ui-tui` in separate runs.
- **Every non-trivial slice ships four artefacts:** doc, bench where perf-relevant, tests
  including failure modes, graceful errors. User-facing slices add `docs/user/` help pages.
- **Mode ownership.** Chords and handler bodies live in `lattice-ai`. The only host and core
  changes are the generic ones listed in design §10; a slice needing another stops and asks.
- **TUI and GPUI in lockstep.** A new `Effect` arm or decoration touches both in the same slice.
- **No UI-thread work.** All I/O and decoding spawn on `boot.runtime_handle()`, never a bare
  `tokio::spawn` from a handler.
- **Async results land without a keypress**, through `SubsystemBoot::inbound`, and tests assert
  that without dispatching another key.
- **Commands are dashed and namespaced** (`llm-*`). `debug!` for per-chunk logs, never `info!`.
- **API keys never appear** in logs, `:llm-inspect`, session files, exports or error text.

## Review focus

Inputs the design is silent on that are most likely to bite. Each has a test in the slice named.

1. **A multi-byte character split across two network chunks** should decode correctly, not as
   replacement characters or a dropped stream. → LL.3
2. **Pressing `<CR>` while a reply is streaming** should queue the prompt and show it, as
   `:opencode-acp` does, not drop it or start a second overlapping turn. → LL.5
3. **Cancelling between a tool call and its result** must not leave a history the provider
   rejects on the next turn, which would brick the chat. → LL.16
4. **A key reaching visible text through an error**, such as a provider echoing the header, or a
   proxy's error page, should be scrubbed. → LL.4
5. **A model sending malformed tool arguments** should go back to it as a tool error it can
   correct, not end the session or panic. → LL.16

## Dependencies

```
LL.1 ─┬─ LL.5 ─ LL.6 ─┬─ LL.7 ─ LL.8
LL.2 ─┘               ├─ LL.10 ─ LL.11
LL.3 ─ LL.4 ─ LL.5    ├─ LL.14 ─ LL.15
LL.9 ─ LL.10          ├─ LL.16 ─ LL.17 ─ LL.18 ─ LL.22 ─ LL.23 ─ LL.24
LL.12 ─ LL.13         ├─ LL.19 ─ LL.20
                      └─ LL.21
```

LL.13 also needs LL.10. LL.7 precedes LL.10, which copies settings.
**First usable version: LL.6.** MCP is last by decision; nothing before LL.22 touches the
Claude Code server.

---

## Foundations

### LL.1 — Protocol-neutral conversation model ✅

**Why:** the design shares one conversation mode between ACP and LLM sessions, and
`conversation.rs` is built on ACP's types. Behaviour-preserving.

**Files:** create `crates/lattice-ai/src/conversation/{mod,model,projection,tool_fold}.rs`.
Move from `acp/conversation.rs`, `acp/conversation_mode.rs`, `acp/tool_fold.rs`. Keep ACP's
`SessionUpdate` folding in `acp/conversation.rs` as an adapter. Modify `acp/supervisor.rs`
imports. The module compiles whenever `acp` or `llm` is on.

**Produces:**

	pub enum ToolKind { Read, Search, Edit, Execute, Other }
	pub enum ToolStatus { Pending, Running, Ok, Err }
	pub enum Block {
		Text(String),
		Reasoning(String),
		ToolCall { id: String, origin: Option<String>, title: String, status: ToolStatus,
		           kind: ToolKind, input: Option<String>, output: Option<String> },
		Edit { path: String, status: EditStatus },
		Permission { /* neutral option list; no ACP type */ },
	}

**Tests:** the existing `conversation` and `conversation_mode` suites pass unchanged in intent;
`acp_adapter_maps_every_session_update.rs`; a compile-time check that `conversation/` imports
nothing from `agent_client_protocol` (a grep test, as the repo's other boundary tests do).

**Done when:** `:opencode-acp` behaves identically, and a build with `llm` and without `acp`
compiles the module (from LL.3).

### LL.2 — Per-buffer editable tail ✅

**Why:** `AiConversationMode` keeps one process-wide anchor, so a second conversation buffer
would corrupt the first's prompt.

**Files:** `crates/lattice-mode/src/mode.rs` (`Mode::editable_tail`), `crates/lattice-host/src/dispatch.rs`
(`active_editable_tail`), `crates/lattice-ai/src/acp/conversation_mode.rs`.

**Produces:** `fn editable_tail(&self, buffer: BufferId) -> Option<EditableTail>`. ACP's anchor
moves to a per-buffer map cleared by the mode's guard.

**Tests:** `two_conversation_buffers_keep_their_own_prompt.rs` (fails before the change):
streaming into A leaves B's prompt editable and A's anchor right; closing a buffer leaves no
stale entry.

**Bench:** none; the lookup is per edit and O(1).

## Core harness

### LL.3 — Provider adapters ✅

**Why:** wire formats tested with no socket. Starts the `llm` feature.

**Files:** `crates/lattice-ai/Cargo.toml` (`llm` feature, in `default`), `src/llm/mod.rs`,
`src/llm/provider/{mod,anthropic,openai}.rs`, fixtures in `crates/lattice-ai/tests/fixtures/llm/`.

**Produces:**

	pub struct LlmRequest { pub model: String, pub system: Option<String>, pub messages: Vec<Message>,
	                        pub tools: Vec<ToolDescriptor>, pub params: Params }
	pub enum StreamEvent { Text(String), Reasoning(String), ToolCallStart { id: String, name: String },
	                       ToolCallInput { id: String, json_fragment: String }, ToolCallEnd { id: String },
	                       Usage(Usage), Done(StopReason), Error(ProviderError) }
	pub trait Provider: Send + Sync {
		fn build(&self, req: &LlmRequest, auth: &ApiKey) -> Result<HttpRequest, ProviderError>;
		fn decoder(&self) -> Box<dyn StreamDecoder>;
	}
	pub trait StreamDecoder: Send { fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent>; }

`ToolDescriptor` uses MCP's shape: name, description, JSON Schema input.

**Tests:** recorded-stream fixtures for each provider covering text, reasoning, tool calls and
usage; `a_character_split_across_chunks_decodes.rs` feeds every fixture split at every byte and
asserts identical events (**review focus 1**); truncated stream yields `Error`, never a panic;
unknown event types are skipped; Ollama-style OpenAI-compatible responses without `usage`.

### LL.4 — Transport and request lifecycle ✅

**Why:** real network, cancellation, and the lifecycle every surface displays.

**Files:** `Cargo.toml` (`reqwest` workspace dep, rustls, stream), `src/llm/transport.rs`,
`src/llm/request.rs`, `src/llm/install.rs`, `src/install.rs`.

**Produces:**

	pub enum RequestState { Pending, Streaming, AwaitingTool, AwaitingReview,
	                        Done, Failed(String), Cancelled }
	pub struct RequestRegistry; // ArcSwap snapshot; registered as a service
	impl RequestRegistry {
		pub fn start(&self, target: RequestTarget, req: LlmRequest, p: Arc<dyn Provider>,
		             sink: Box<dyn Sink>) -> RequestId;
		pub fn cancel(&self, id: RequestId);
		pub fn snapshot(&self) -> Arc<Vec<RequestInfo>>;
	}
	pub trait Sink: Send { fn event(&mut self, ev: StreamEvent) -> SinkFlow; } // Continue | Stop

**Tests** (against a local tokio TCP mock): streams to completion; `cancel_stops_a_stalled_stream`
returns within 100ms with the server silent; 401, 429 and 500 become named `Failed` messages;
connection drop mid-stream; `a_key_never_reaches_visible_text.rs` with a server echoing the
header and a proxy HTML error (**review focus 4**); `requests_run_off_the_actor_thread.rs`
started from a `current_thread` runtime. Also throughput plus runtime-responsiveness coverage.

**Bench:** decode throughput in `benches/`.

### LL.5 — `:llm` chat buffer ✅

**Why:** the first end-to-end path.

**Files:** `src/llm/modes/chat.rs` (`llm-chat-mode` major, implies `repl-mode`, and `read-only-mode`
per the two-declarations rule), `src/llm/commands.rs`, `src/llm/config.rs` (a minimal `[llm]` read:
providers, default model, `api_key_env`/`api_key_command`), `docs/user/llm-chat-mode.md`.

**Chords:** `<CR>` send, `<C-j>` newline, `<C-c>` interrupt, `gq` close. Buffers are named
`*llm:<n>*` and created through `ModeActivator::ensure_named_document`.

**Tests:** a reply reaches the buffer with no further key; `sending_while_streaming_queues.rs`
shows the queued count and sends in order (**review focus 2**); `x`, `dd`, `p` refused in history,
allowed in the prompt; missing key shows an error in the buffer and the prompt stays usable;
closing the buffer cancels; two chats at once do not cross; the caret stays in the prompt while
text arrives above; the buffer does not block `:qa`.

**TUI/GPUI:** no new effects expected; verify both render it.

### LL.6 — Markdown transcript and folded blocks ✅

**Why:** highlighting and the block display. **First usable version.**

**Files:** `src/conversation/projection.rs`, `src/conversation/tool_fold.rs`, the chat mode's
language declaration through the mode registry's language index (`Editor::lang_for_major`), and
config for `llm.fold.reasoning` and `llm.fold.tools`.

**Behaviour:** head line outside fenced bodies; output language by tool, then JSON, then `text`;
fences longer than their content; unclosed fences closed by the projection; `closed`, `open`,
`hidden` per block type.

**Tests:** `a_half_received_fence_does_not_colour_the_prompt.rs` asserting resolved styles, not just
that a query compiled; a fence inside tool output; an expanded block stays expanded as text streams;
`hidden` then `closed` restores blocks; `<CR>` does not follow links here; `zM` and `za` with a reply
containing `#` headings (design §11).

**Bench:** `benches/llm_projection.rs`: projection cost for a growing reply left in an open fence, and a 100 KB tool output.

**Landed:** `ProjectionOptions` (markdown, per-type `FoldDisplay`) is a parameter of the shared
projection and fold sources; `:opencode-acp` passes the default and is unchanged. The chat declares
`target_language() = "markdown"` (grammar only: `markdown-mode` is not activated, so its `<CR>`
link chord stays out). Fold values are the `llm.fold.*` options from LL.7.
`benches/llm_projection.rs` covers projection of a growing reply (about 68 µs for a 2000-line
reply). No existing keystroke-ratchet bench was easy to extend with a streaming producer, so the
ratchet comparison is not done here; the projection runs on the drain task, not the UI thread.

## Settings

### LL.7 — Layers, profiles and trust ✅

**Files:** `crates/lattice-config/src/` (`OptionOrigin::{Profile, Project}`), `src/llm/settings.rs`,
`src/llm/config.rs`, `docs/user/llm.md`.

**Produces:**

	pub struct Resolved { pub values: Settings, pub origins: BTreeMap<SettingKey, OptionOrigin> }
	pub fn resolve(layers: &Layers, buffer: BufferId, req: &RequestOverrides, trust: Trust)
		-> Result<Resolved, SettingsError>;

Session values are typed options, so `:setlocal llm.model=…` and `:set llm.model?` work.

**Tests:** precedence of every layer pair; **every row of the design's trust table**, including
project config setting `base_url`, key commands, wider tools, trust and MCP servers, each refused
with a message; switching profile clears overrides and echoes them; unset env var names the variable;
a failing `api_key_command` reports without its output; unsupported parameter omitted with a notice.

**Landed:** `OptionOrigin::{Profile, Project}`; the `llm.*` options (`options.rs`, group `llm`);
`settings.rs` (`resolve`, `Layers`, the trust table, one `validate` shared by options, profiles and
project files). Host changes, all generic: `BufferOptionsResolved` (published after every per-buffer
resolution, the only way off-thread readers see `:setlocal`), `llm.providers` / `llm.profiles` as
structural config prefixes, and `:setlocal name&` now finds an option by name instead of parsing a
dummy value (a validated option could not be cleared). The loader refuses `[llm]` in
`.lattice/config.toml`: project LLM config is `.lattice/llm.toml`, read per request off-thread.
`:llm [profile]` and `:llm-profile` (the one switch: `:setlocal` cannot clear siblings). Fold sources
read a `ProjectionFrame` the drain publishes with the text, so text and folds share one snapshot.
`default_model` is gone, replaced by `model`. Unsupported parameters: a short per-provider list in
`config.rs` (`omitted_params`), reported once per chat in `:messages`; no 400-retry, no capability
table. Open: the notice is a log line, not a transcript row; a buffer's mirrored overrides are not
dropped when a non-chat buffer closes (sparse, a few bytes).

### LL.8 — Overriding on the go ✅

**Files:** `src/llm/menu.rs` (a `TransientSpec` of `Flag`/`Argument` rows), `src/llm/commands.rs`
(`:llm-menu`, `:llm-system`, argument parsing), `src/llm/prompt_cookie.rs`,
`docs/user/llm-system-mode.md`.

**Tests:** the menu shows value and origin and names the session; `=` toggles scope and stays toggled;
session scope persists, next-request scope does not; a menu change during a stream applies to the next
turn; `model=` argument for one request only; `@writer` in the last turn only; `@unknown` left as text;
`@coding` raising tools asks first; selected source containing `@coding` is not scanned;
`:llm-system` `:w` applies, and `:q!` discards.

**Landed:** `menu.rs` (`:llm-menu`, a `TransientSpec` of action rows: each shows value and origin, `=`
flips the scope and reopens the menu, `x` clears; bound to `<M-m>` in a chat; handler bodies contributed
by `llm-chat-mode`), `prompt_cookie.rs`, `modes/system.rs` (`llm-system-mode`), `parse_request_args`.
`LlmChats::submit_with` carries per-prompt `RequestOverrides` (kept in step with the queue);
`plan_send` decides a typed `@profile` (`Plain` / `Profile` through the restricted path /
`NeedsConfirm` through `Effect::Confirm`, whose yes half applies at full trust for that request);
`RequestOverrides::pasted_profile` lets a cookie profile be restricted while menu and argument values
keep user trust. Two generic seams: `Mode::write_action` (a mode claims a bare `:w`; the host walks
active modes as it does for `refresh_action`) and `lattice-ai` depending on `lattice-picker` for
`TransientSpec`. Review round 1 closed: `:wq` / `:x` end at the claimed write
(`effect_closes_buffer`), a Visual selection reaches ranged ex-commands as lines (`resolve_ex_range`),
the menu shows the project layer from an off-thread cache (refreshed at chat start and by each
request), the `s` row's form applies in the scope the menu was in (the name carries `:next`), a
request's `@profile` sits at the request layer above session overrides, and `:w` in a form it cannot
place echoes an error. Not built: `:llm-ask model=...` wiring (the parser and `submit_with` are here;
the command is LL.10).

⛔ **Deferred: the menu's `P` (save as profile).** It needs a user-TOML writer, and none exists:
`lattice-config`'s loader is read-only and names `:customize` write-back (on `toml_edit`) as post-1.0.
It returns with that writer, built on it, off-thread, and never writing keys.

## Any buffer

### LL.9 — Operator seams ✅

**Why:** ask needs to be a non-mutating operator bound from outside `lattice-host`.

**Files:** `crates/lattice-grammar/src/registry.rs` (`OperatorSpec::mutates`, set on every builtin),
`crates/lattice-host/src/dispatch.rs` (`run_read_only_motion` reads it, replacing the `yank` id
check), `crates/lattice-mode/src/subsystem_boot.rs`, `crates/lattice-host/src/boot_context.rs`.

**Produces:** `fn bind_operator(&mut self, layer: KeymapLayer, prefix: &str, op: OperatorId)` on
`SubsystemBoot`, forwarding to `register_operator_bindings_in`.

**Tests:** `y` still works and `d`, `c` still refuse on a read-only buffer; a non-mutating test
operator runs there; an operator bound through the seam composes with motions, text objects, counts
and Visual; it lives only where its minor is active.

**Built:** `OperatorSpec::mutates` is a required field (no default) set on every operator; a plugin
operator's is `true` because the WIT `operator-spec` has no such bit. `bind_operator` queues until
`editor_boot` hands `BootContext` the keymap binder, then binds immediately; the binder is the existing
`OperatorChordWirer` service, extended with `bind_in(layer, ..)`. The linewise form repeats the
prefix's last key. `fold` and `g/` are now `mutates = false`, so a read-only buffer runs them.

### LL.10 — Ask and send ✅

Carved into three slices, one commit each.

| Slice | Scope | Status |
|---|---|---|
| LL.10a | `Effect::DisplaySyntheticBuffer`: show a named buffer in another pane, focus unmoved, reuse a pane already in the same major, else split. Host-applied; TUI + GPUI peer arms; no WIT mirror yet. Tests: `lattice-host/tests/display_synthetic_buffer.rs` (7). | ✅ |
| LL.10b | `llm-mode` (`Universal` minor), the ask operator, the instruction prompt, the request buffer with the source as a folded first turn; read-only sources; blockwise collapses to one span; `.` decided. | ✅ |
| LL.10c | `<leader>aA` (menu first), `destination=<buffer>`, size threshold, `llm.exclude` refusing `.env`. | ✅ |

**LL.10b landed.** `operator:llm-ask` (`mutates: false`, `blockwise_per_row: false`) captures its
span when it runs and opens an `Effect::OpenPrompt` whose buffer name (`*llm-ask:<token>*`) carries
the held ask to `action:llm-ask-send`; `<Esc>` fires nothing and held asks are capped at 16.
`:'<,'>llm-ask [key=value…] instruction` is the scriptable front-end (current line without a range).
Both end in `ask_effect`: `LlmChats::create_for_source` (named `*llm:<file>:<n>*`, settings copied
as `create_with` copies them, plus the source's pending menu choices) and `submit_source`, then
`Effect::DisplaySyntheticBuffer`. The source is a new conversation block, `Block::Source { label,
lang, text, link }`: projected as a `▸ foo.rs:3-10` head over a fence in the file's language, folded
by the tool-call fold source and closed past 8 lines, sent to the model as `From <label>:` and a
fence; `gf` in the chat goes back through `source_at_line`. `llm-mode` is `Universal`, so
`*compilation*`, `*messages*` and terminal buffers have the chord. One generic seam change:
`bind_operator` now expands `<leader>` through the binder (`OperatorChordWirer::expand_leader`) at
bind time, and `lattice-mode` re-exports `KeymapLayer` so a subsystem can call it. `.` is decided
(design §11): an ask is not repeatable. `@profile` that widens tools is refused from an ask with a
pointer to `:llm <profile>`, since an ask has no prompt to confirm into. Not covered here: an ask
from a multibuffer or terminal through the operator is exercised only through `llm-mode`'s
`Universal` activation, not a keystroke test; `sink.rs` is not needed until LL.13 (the dedicated
buffer reuses the chat's sink).
One grammar fix fell out of the blockwise test: a block collapsed to one span (`blockwise_per_row:
false`) ended *before* the head's character, so the caret's column was dropped. It now ends past
it, as charwise Visual does (`past_head` in `dispatcher.rs`). That also changes `gU`/`gu`/`g~` on a
block, which share the collapse; they still act on one anchor..head span rather than vim's
rectangle, a pre-existing gap this slice does not take on.


**LL.10c landed.** `operator:llm-ask-menu` (`<leader>aA`) holds its ask and opens `:llm-menu`
armed with it (`LlmChats::arm_menu_ask`, per-buffer menu state beside `menu_scope`); the menu gains
an `a` row (`action:llm-ask-continue`) that opens the instruction prompt. Opening the menu by hand
(`:llm-menu`, `<M-m>`) disarms it, so no stale ask is offered. `llm.exclude` is a typed list option
(`ExcludeGlobs`, `globset`; name-only globs match the file name, `/` globs the path; replaces the
default `.env`/`.env.*`/`*.pem`/`*.key`/`id_rsa*`/`id_ed25519*`/`.netrc`), checked at the operator
before the prompt and again in `ask_effect`, which `:llm-ask` reaches. A source over
`ASK_CONFIRM_BYTES` (32 KiB) is held and returned as `Effect::Confirm`; its yes action declares a
two-slot schema, since a confirm's args travel through the schema and an unslotted value is dropped.
`destination` now accepts a chat buffer name (`is_buffer_name`); an ask naming one submits to that
session, an unknown name is refused, and `below`/`echo`/`register`/`review` (LL.13-14) open a new
buffer with a note. The threshold is a constant, not an option: nothing yet asks to tune it.

**Files:** `src/llm/modes/llm_mode.rs` (global minor, `<leader>a` chords), `src/llm/operators.rs`,
`src/llm/sink.rs`, `docs/user/llm-mode.md`.

**Behaviour:** new dedicated buffer per request, settings copied at creation, source as a folded first
turn with a link, focus stays, repeat asks reuse the pane, `<leader>aA` opens the menu first,
`destination=<buffer>` continues a chat. Size threshold and `llm.exclude` apply here.

**Tests:** focus unmoved; settings copied and not linked, both directions; read-only source works;
`.env` refused by name; oversized selection asks; a cancelled instruction prompt sends nothing;
multibuffer and terminal sources; blockwise ask collapses to one span; macro does not wait; `.` repeats
or the gap is recorded here (design §11).

### LL.11 — Rewrite through review 🚧

Carved into two slices, one commit each. Dhruva chose (A) on 2026-11-07: extend the
programmatic diff rather than give rewrite its own review buffer, because the bus diffed disk
and saved on Accept, and LL.17's edit tools need the same buffer-text review.

| Slice | Scope | Status |
|---|---|---|
| LL.11a | `lattice-diff`: `DiffBaseline::{Disk, Text}`, `OnAccept::{Save, ReturnText}` on `ProgrammaticDiffRequest`; host reads the baseline from text when given, names (not paths) a returned-text right side, and sends its live text back on Accept before the outcome (`ProgrammaticAccept`). `lattice-agent::review_text`. Tests: host text-review accept (edits returned, nothing written) and reject; agent round-trips. | ✅ |
| LL.11b | `<leader>ar` rewrite operator (`src/llm/rewrite.rs`), instruction prompt, hidden session (`ChatSession::wait_turn_end`), review through `review_text`, accept applied over a `NeverGrows` tracked range as one edit, a changed span reviewed again against its current text (the deliberate confirm; a `Confirm` effect sent from the task would be dropped, since off-keystroke effects reach only host-applied variants), deleted span / closed source show the session, refusals (read-only by `mutates`, blockwise, composed view, `llm.exclude`, empty), fence unwrap, diffs queued one at a time, a macro replays a rewrite. Tests: `lattice-ui-tui/src/app/llm_rewrite.rs` end to end against a mock server (11). | ✅ |
| LL.11c | `.` repeats a rewrite (design §11 names it the operator `.` should reach). The host records `.` only from a mutating effect (`effect_mutates`), and a rewrite's effect is its prompt, so this needs a host change design §10 does not list: honour `OperatorSpec::repeatable`. Dhruva chose this (A) on 2026-11-07 over "no `.`" and "`.` resends without a prompt" (which would send unseen, against §5): `.` re-runs the operator, its prompt pre-filled with the last instruction. | 📝 |

**Noted for follow-up:** a programmatic diff takes the focus when it opens. A rewrite's answer
arriving while the user types sends the remaining keys to the diff. The tests hold the answer to
avoid it; openDiff behaves the same. A UX question for the diff subsystem, not this slice.


**Files:** `src/llm/rewrite.rs` (on `lattice_agent::review_diff`).

**Tests:** accept applies, reject leaves the buffer untouched; source changed while waiting marks the
diff stale and accepting needs confirmation; source closed moves the result to a session buffer;
blockwise and multi-file multibuffer refused up front; read-only refused; `.` and macro repeats queue
one diff at a time; a model reply wrapped in a code fence is unwrapped.

### LL.12 — Anchored append 📝

**Why:** inline output's substrate. The main core change.

**Files:** `crates/lattice-core/src/document.rs`, `tracked.rs`, `undo.rs`;
`crates/lattice-runtime/src/{actor,document,handle}.rs`; `crates/lattice-host/src/dispatch.rs`
(`should_adopt_owner_write`).

**Produces:**

	fn append_to_tracked(&self, id: TrackedRangeId, text: String) -> Pending<AppendOutcome>;
	pub enum AppendOutcome { Appended, Collapsed, Unknown }

Resolves in the actor's mailbox turn, grows the range, amends its own undo entry while that entry is
top of the stack, and otherwise starts a new one. `Document`'s default returns `Unknown`, so kinds
without text refuse.

**Tests:** a property test interleaving user edits with appends asserts the response stays contiguous
and the user's text intact; collapse refuses; undo removes the whole response with nothing interleaved
and the latest segment otherwise; the caret is carried in an ordinary buffer, active and inactive pane;
adoption does not double-move a caret the host positioned itself; Insert's undo group is untouched.

**Bench:** append cost against document size; keystroke latency during appends.

### LL.13 — Inline output 📝

**Files:** `src/llm/sink.rs` (document-span sink, one write per frame interval).

**Bench:** sustained streaming (chat and inline) beside typing against the keystroke ratchet; moved from LL.6, which has no streaming producer.

**Tests:** every Inline row of the design's edge-case table: typing above, typing at the point,
deleted anchor, `u` while streaming, read-only fallback, second request at one point; partial text
gets an "interrupted" marker; nothing left behind on an immediate failure; the view follows only when
the caret is at the end; only streamed lines change on screen.

### LL.14 — Other destinations and running requests 📝

**Files:** `src/llm/sink.rs` (echo, register), `src/llm/modes/requests.rs`, `src/llm/commands.rs`
(`:llm-requests`, `:llm-stop`, `:llm-last`), `src/llm/modeline.rs`.

**Tests:** `dd` cancels from the list; `:llm-stop` with zero, one and many requests, opening the list
when ambiguous; `:llm-last` after echo, register and rewrite; hidden buffer keeps streaming and notifies
without a keypress; tools-enabled request aimed at `below` moves and says so; `:q` warns and restore
never resumes a request; modeline repaints off-keystroke.

### LL.15 — Context and inspect 📝

**Files:** `src/llm/context.rs`, `src/llm/modes/context.rs`, `<leader>ac`, `:llm-context`, `:llm-inspect`.

**Tests:** attached buffer and file text read at send time; `dd` removes; excluded file refused; inspect
matches what the mock server receives byte for byte and shows no key; a closed attached buffer is
reported, not silently dropped.

## Tools

### LL.16 — ToolBroker, loop and read tools 📝

**Files:** `src/llm/tools/{mod,broker,lattice}.rs`, `src/llm/loop.rs`, `crates/lattice-agent/src/editor_access.rs`
(reads against unsaved text), `docs/dev/architecture/agent-integration.md` (fix the stale §2).

**Produces:**

	pub trait ToolSource: Send + Sync {
		fn list(&self) -> BoxFuture<'_, Result<Vec<ToolDescriptor>, ToolError>>;
		fn call(&self, name: &str, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>>;
	}
	pub struct ToolBroker; // namespaces sources, applies selection and the trust table

**Tests:** the loop runs read, result, final answer against the mock; unsaved buffer text is what a read
returns; `cancelling_between_call_and_result_keeps_history_valid.rs`, where the next turn's payload is
accepted by a strict mock (**review focus 3**); `malformed_tool_arguments_return_a_tool_error.rs`
(**review focus 5**); an unknown tool name; a turn limit stops a runaway loop with a message; tool results
never widen permissions.

### LL.17 — Edit tools through review 📝

**Files:** `src/llm/tools/lattice.rs`, `crates/lattice-agent/src/editor_access.rs`.

**Tests:** edit opens a diff and the verdict reaches the model; reject returns a usable refusal; `<C-t>`
trust applies without a diff, resets per session; review surfaces as `AwaitingReview`; stale target; several
edits queue; closing the chat rejects its pending diffs through `close_session_diffs`.

### LL.18 — Exec and permissions 📝

**Files:** `src/llm/tools/lattice.rs`, `src/acp/permission_mode.rs` moved to `src/permission/` on neutral types.

**Tests:** exec asks, defer leaves it pending, deny reaches the model; cancel kills the process tree; output is
capped; `:opencode-acp` permissions unchanged; a project cannot enable exec.

## Session extras

### LL.19 — Fork and retry 📝

**Files:** `src/llm/modes/chat.rs`, `src/conversation/model.rs`, `:llm-retry`.

**Tests:** fork copies history to that turn, with the prompt ready, and leaves the original untouched,
including its tool calls; retry uses current settings; switching provider drops reasoning data and says so;
retry while streaming is refused.

### LL.20 — Export and session files 📝

**Files:** `src/llm/session_file.rs`, `:llm-export`, resume command.

**Tests:** `.md` round trip; `.org` conversion including fences; session resume restores tool ids; a tampered
or newer-version file is refused with a message; a session file cannot widen trust; no key on disk.

### LL.21 — Gemini 📝

**Files:** `src/llm/provider/gemini.rs`, fixtures.

**Tests:** the LL.3 suite, including the split-at-every-byte check, on Gemini fixtures.

## MCP (last)

### LL.22 — `rmcp` gate: `LatticeServer` over the existing WebSocket 📝

**Why:** one implementation. **A gate:** if it cannot pass, stop and tell Dhruva. The fallback is
`rmcp` client-only, with `dispatch.rs` and `protocol.rs` kept.

**Files:** `Cargo.toml` (`rmcp` under the `mcp` feature), `src/mcp/lattice_server.rs`, `src/mcp/server.rs`;
delete `src/mcp/{dispatch,protocol}.rs`.

**Tests:** `tests/mcp_ws_roundtrip.rs` unchanged and green; the catalogue equals today's by name and schema;
`selection_changed` and `at_mentioned` still reach the client; a blocking `openDiff` does not stall the read
loop; bad token still 401; **a real `claude` session driven by hand**, with the result recorded in the commit.

### LL.23 — Lattice's tools in-process 📝

**Files:** `src/llm/tools/mcp.rs` (`Connection` as a `ToolSource`), `src/llm/tools/lattice.rs` (moves onto
`LatticeServer` with the full catalogue).

**Tests:** the LL.16–LL.18 suites pass unchanged, which is the proof nothing above the broker changed; the
WebSocket connection still sees only the Claude Code catalogue.

### LL.24 — External servers 📝

**Files:** `src/llm/tools/mcp.rs` (stdio, streamable HTTP), `src/llm/config.rs` (`[llm.mcp.*]`),
`src/llm/modes/mcp.rs` (`:llm-mcp`), `docs/user/llm-mcp-mode.md`.

**Tests** (fixture server): tools namespaced and selected by profile with `github.*`; external tools ask unless
allowed by name; `readOnlyHint` shown and not trusted; a server failing to start reports in `:llm-mcp` and
does not block a request; a server dying mid-call becomes a tool error; `tools/list_changed` updates the list;
project config cannot declare a server; cancel kills a stdio child.
