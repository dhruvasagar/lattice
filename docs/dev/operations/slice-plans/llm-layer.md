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

### LL.11 — Rewrite through review ✅

Carved into two slices, one commit each. Dhruva chose (A) on 2026-11-07: extend the
programmatic diff rather than give rewrite its own review buffer, because the bus diffed disk
and saved on Accept, and LL.17's edit tools need the same buffer-text review.

| Slice | Scope | Status |
|---|---|---|
| LL.11a | `lattice-diff`: `DiffBaseline::{Disk, Text}`, `OnAccept::{Save, ReturnText}` on `ProgrammaticDiffRequest`; host reads the baseline from text when given, names (not paths) a returned-text right side, and sends its live text back on Accept before the outcome (`ProgrammaticAccept`). `lattice-agent::review_text`. Tests: host text-review accept (edits returned, nothing written) and reject; agent round-trips. | ✅ |
| LL.11b | `<leader>ar` rewrite operator (`src/llm/rewrite.rs`), instruction prompt, hidden session (`ChatSession::wait_turn_end`), review through `review_text`, accept applied over a `NeverGrows` tracked range as one edit, a changed span reviewed again against its current text (the deliberate confirm; a `Confirm` effect sent from the task would be dropped, since off-keystroke effects reach only host-applied variants), deleted span / closed source show the session, refusals (read-only by `mutates`, blockwise, composed view, `llm.exclude`, empty), fence unwrap, diffs queued one at a time, a macro replays a rewrite. Tests: `lattice-ui-tui/src/app/llm_rewrite.rs` end to end against a mock server (11). | ✅ |
| LL.11c | `.` repeats a rewrite (design §11 names it the operator `.` should reach). The host records `.` only from a mutating effect (`effect_mutates`), and a rewrite's effect is its prompt, so this needs a host change design §10 does not list: honour `OperatorSpec::repeatable`. Dhruva chose this (A) on 2026-11-07 over "no `.`" and "`.` resends without a prompt" (which would send unseen, against §5): `.` re-runs the operator, its prompt pre-filled with the last instruction. Built as `Editor::record_change`: a mutation, or a `repeatable` operator's effect that did something, becomes `last_change`; a prompt that effect opened has its submit remembered (`last_change_prompt`, the `last_insert` analogue) and `.` opens it pre-filled. Yank's `repeatable: true`, unread until now, is corrected to `false`. Tests: `.` repeats with the instruction, a fresh rewrite opens empty, `<Esc>` in the repeated prompt sends nothing, `.` after `yy` repeats the change before. | ✅ |

**Noted for follow-up:** a programmatic diff takes the focus when it opens. A rewrite's answer
arriving while the user types sends the remaining keys to the diff. The tests hold the answer to
avoid it; openDiff behaves the same. A UX question for the diff subsystem, not this slice.


**Files:** `src/llm/rewrite.rs` (on `lattice_agent::review_diff`).

**Tests:** accept applies, reject leaves the buffer untouched; source changed while waiting marks the
diff stale and accepting needs confirmation; source closed moves the result to a session buffer;
blockwise and multi-file multibuffer refused up front; read-only refused; `.` and macro repeats queue
one diff at a time; a model reply wrapped in a code fence is unwrapped.

### LL.12 — Anchored append ✅

**Landed:** `Document::append_to_tracked` and `AppendOutcome` in core; the actor's
`AppendToTracked` turn, `RopeDocumentHandle::append_to_tracked` and the runtime `Document`
default (`Unknown`); `DocumentSnapshot::append_version`; the OWC adopt gate extended to appends
in any buffer, and inactive panes' carets anchored as tracked ranges while away
(`owner-write-caret.md` §9). Tests: `lattice-core/tests/anchored_append.rs` (12, a proptest
among them) and `lattice-host/tests/anchored_append_caret.rs` (4). Benches:
`document_edit_p99_us::append_to_tracked`, `runtime::apply_edit_round_trip_during_appends`.
A save made mid-stream is invalidated by the next amend (`dirty()` would otherwise lie).
**Handed to LL.13:** the write-through race in `owner-write-caret.md` §9 (closed by LL.13a).

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

### LL.13 — Inline output ✅

Carved in two when the sink's writer exposed a race with the host's keys (decided with
Dhruva: rebase in the actor, not route writes through the editor).

**LL.13a — Writes positioned before an append ✅.** The document actor rebases the host's
caret-positioned edit and its write-through over appends made since the version the host's caret
is in (`Document::apply_edit_based_on` / `set_selections_based_on`, a bounded append log); the host
records `pre_append_version` after its own edits; `AppendOutcome::Undone` refuses a stream's next
chunk after any undo or redo, so `u` while streaming cancels with nothing landing after it.
`transform_position` fixed on two rows that put a caret on different text (`owner-write-caret.md`
§10). Tests: `anchored_append.rs` (+11, a second proptest), `buffer.rs` (+3),
`anchored_append_caret.rs` (+3, the race reproduced by bare `dispatch`). Bench:
`document_edit_p99_us::rebase_over_appends`.

**LL.13b — The `below` destination ✅.** `src/llm/sink.rs`: an ask's hidden chat session hands its
reply text to a writer through a per-turn `TextTap`; the writer appends at most once a frame into a
tracked range at the end of the span's last line, opening with a newline and holding trailing ones
back, and wakes the editor after each write. Stops on `Collapsed` / `Undone`; a failure partway ends
in `[interrupted]`, one before any text writes nothing. Read-only (from `BufferOptionsResolved`, no
new host query) and composed sources fall back to a new chat with a note; a second answer at one
point is refused. Host: `run_tick_pending` adopts an owner write, so the caret moves with streamed
text on the wake. Tests: `lattice-ui-tui/src/app/llm_inline.rs` (11, every Inline row of the design's
edge table plus the screen contract), `sink.rs` units (2). Bench:
`dispatch_publish::keystroke_while_streaming`.

**Files:** `src/llm/sink.rs` (document-span sink, one write per frame interval).

**Bench:** sustained streaming (chat and inline) beside typing against the keystroke ratchet; moved from LL.6, which has no streaming producer.

**Tests:** every Inline row of the design's edge-case table: typing above, typing at the point,
deleted anchor, `u` while streaming, read-only fallback, second request at one point; partial text
gets an "interrupted" marker; nothing left behind on an immediate failure; the view follows only when
the caret is at the end; only streamed lines change on screen.

### LL.14 — Other destinations and running requests ✅

Carved into three, each its own commit:

**LL.14a — Echo, register, `:llm-last` ✅.** `src/llm/sink.rs` `Quiet::{Echo, Register}` and
`LlmInline::start_quiet`: the ask's hidden session runs the request and the complete reply goes out
as one effect through the inline bus, `Effect::Echo` with the first line and a count, or
`Effect::Yank` into the register typed before the operator (`AskSource.register`; unnamed for
`:llm-ask`), linewise when several lines. `LlmChats::last_ask` names the latest ask's session while
it is kept; `:llm-last` displays it. A request whose resolved `tools` is non-empty, aimed at
`below`/`echo`/`register`, opens a chat with a note. No host change; no renderer change (both
effects exist). No bench: the work is one effect per answer, nothing per key or per frame. Tests:
`lattice-ui-tui/src/app/llm_inline.rs` (+5), `sink.rs` units (+2).

**LL.14b — `:llm-requests`, `:llm-stop` ✅.** `src/llm/modes/requests.rs` `llm-requests-mode`
(major, `ReadOnly` + implies `read-only-mode`): `*llm-requests*` renders the registry snapshot,
running in start order then finished newest first, and repaints on `RequestStateChanged` (bursts
coalesced, unchanged text skipped), waking through the inline bus. `dd` (major layer) stops the
row's request with `LlmChats::stop_request`, which interrupts through the owning session so waiters
hear "interrupted" (inline marker, quiet echo); an unowned request is cancelled outright. Sessions
carry `source` and `destination`: a hidden ask's `RequestTarget.buffer` is its source buffer and its
label says where the answer goes. `:llm-stop` stops this buffer's one request, else the only live
one, else opens the list. No host or renderer change. No bench: a repaint per state change, nothing
per key. Tests: `llm_inline.rs` (+4), `requests.rs` units (3). **LL.14c — The modeline ✅.** `src/llm/modeline.rs`: a `Scope::Global` `llm` element, right zone
ahead of the LSP badge, and a forwarder that folds `RequestStateChanged` into the running count and
publishes `ModelineElementUpdate` only when it moves (`llm`, `llm N`, hidden at zero); the host's
existing modeline wake repaints off-keystroke. The descriptor is registered by `llm-mode`'s first
activation, not at install: `lattice_ai::install` runs before the host builds the modeline service
(`editor_boot.rs`), so a lookup there finds nothing. No host or renderer change (both peers paint
pushed elements from the shared resolver). Tests: `llm_inline.rs` (+2: shown and cleared without a
key, store and painted row; counted), `modeline.rs` unit. *A restored session never resumes a
request* holds by construction: there is no session restore, and the registry is in memory only.

**LL.14d — Hidden chat says it is done; `:q` warns ✅.** Two generic host primitives, decided (A)
and (A) and added to design §10's exceptions. `lattice_mode::QuitGuards` (Phase-A service): owners
register a cheap check, `do_quit` without `!` consults them after the dirty guard; the llm layer's
counts sessions with a turn in flight (`LlmChats::in_flight`), not registered requests: a request
registers only after its key and settings resolve off-thread, and a `:q` in that window would drop
it (caught by the quit test under load; `quitting_right_after_asking_is_refused_too` pins it). `BufferStore::is_displayed` (default `true`, the quiet answer): the host's
`BufferRegistry` holds the current tab's shown buffers, refreshed in `publish_render_state` with an
allocation-free compare when unchanged; `LlmChats::set_done_hook` echoes "has answered" / "failed"
through the inline bus for a shown chat's finished turn only when its buffer is not displayed.
Tests: `llm_inline.rs` (+4: hidden notifies without a key, visible stays quiet, `:q` needs `!`,
and right after the ask),
`llm_chat_buffer.rs` (quit test now asserts the request guard, not the dirty one, then quits once
stopped; displayed set across replace, split, focus, `:only`), `buffer_registry` unit, `quit_guard`
units (2). Bench: `dispatch_publish` keystroke/steady-state unchanged within noise.


**Files:** `src/llm/sink.rs` (echo, register), `src/llm/modes/requests.rs`, `src/llm/commands.rs`
(`:llm-requests`, `:llm-stop`, `:llm-last`), `src/llm/modeline.rs`.

**Tests:** `dd` cancels from the list; `:llm-stop` with zero, one and many requests, opening the list
when ambiguous; `:llm-last` after echo, register and rewrite; hidden buffer keeps streaming and notifies
without a keypress; tools-enabled request aimed at `below` moves and says so; `:q` warns and restore
never resumes a request; modeline repaints off-keystroke.

### LL.15 — Context and inspect ✅

**Files:** `src/llm/context.rs`, `src/llm/modes/context.rs`, `<leader>ac`, `:llm-context`, `:llm-inspect`.

**Tests:** attached buffer and file text read at send time; `dd` removes; excluded file refused; inspect
matches what the mock server receives byte for byte and shows no key; a closed attached buffer is
reported, not silently dropped.

**Landed.** `src/llm/context.rs` (items, send-time read, the inspect rendering, the operator and
`:llm-context` / `:llm-context-add` / `:llm-inspect`), `src/llm/modes/context.rs` (`*llm-context*`
with `dd`, `*llm-inspect*`; both repaint from a `watch` counter and wake through the inline bus).
`LlmChats::prepare` is the send's preparation factored out of `start_request`, so the chat preview
and the send build the same request. Context is per buffer, copied into sessions made from it;
spans are tracked ranges (linewise grow at edges). An unreadable item fails the request. Tests:
`lattice-ui-tui/src/app/llm_context.rs` (5, against a recording mock: span edit and file read at
send, inspect equals received bytes in a buffer and as a chat preview, no key shown, `dd` removes
and the next request goes without it, excluded refused, closed buffer refuses the chat's request),
units in `context.rs` (3) and `modes/context.rs` (1). Not built: a `context on/off` setting (design
§4 lists it among overridable values); removing items is the way to send without them.

### LL.15b — The prefix is an option ✅

Asked for mid-plan: `<C-c>.` as an alternative to `<leader>a`. `llm.prefix` (one value, global,
validated as keys), and `LlmKeys` in `modes/llm_mode.rs` binds the four operators and seven command
chords (`l m C i R L k`) into `llm-mode`'s layer on its first activation, rebinding on
`OptionChanged` (the TOML is read after boot). The prefix alone shows which-key; a transient on
the bare prefix was chosen against for now, since it needs vim-style `timeoutlen` resolution and a
start-operator effect, both generic host work (design §2). Known limit, documented: a buffer
binding `<C-c>` alone shadows `<C-c>.` chords. Tests: `lattice-ui-tui/src/app/llm_ask.rs` (3:
default bound, `:set` moves operators and commands and which-key's continuations list all eleven,
an unparsable value is refused and the chords stay).

## Tools

### LL.16 — ToolBroker, loop and read tools ✅

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

**Landed.** The loop is `src/llm/tool_loop.rs` (`loop` is a keyword), plugged into
`RequestRegistry::start_loop` through a `Rounds` trait: one request id across exchanges, moving
through `AwaitingTool`. Calls and exact arguments live in the session (`Conversation::tool_calls`),
and `history::messages` projects them, answering every sent call (a stopped one with an error), so
the continuing request and a later prompt's request are one projection. The broker offers a fixed
set per request: wire names `server__tool`, selectors `server.*` / `server.name` / kind groups for
lattice's own tools (`llm.tools` now accepts `*`). Read tools: `read_file`, `list_directory`,
`search` (new `regex` and `ignore` deps on the `llm` feature), confined to the project root and
outside `llm.exclude`. `EditorAccess` gained `buffer_text`, `open_texts` and `read_text_file`, and
`agent-integration.md` §2 now matches the source. New global option `llm.tool_rounds` (25). The
Anthropic adapter joins a prompt to a preceding tool-result message so roles alternate. Tests:
`tests/llm_tool_loop.rs` (4), the two named review-focus files (1 + 2), units in `broker.rs` (3),
`lattice.rs` (2), `history.rs` (2), `anthropic.rs` (1), and `lattice-ui-tui/src/app/llm_tools.rs`
(2: unsaved text is what a read returns through the real install, a read outside the project is
refused). Bench: `llm_tool_history`.

### LL.17 — Edit tools through review ✅

**Files:** `src/llm/tools/lattice.rs`, `crates/lattice-agent/src/editor_access.rs`.

**Landed:** `edit_file` / `write_file` on `LatticeTools::with_review` (offered only when the
host has a diff bus), each a `review_text` against the file as the user has it, applied to
that text: `EditorAccess::apply_buffer_text` (one minimal span, refused if the buffer moved)
or a disk write refused if the file changed. `CallContext` on `ToolSource::call` carries the
session's diff tag, its trust and a review callback; `RoundStatus` on `Rounds::after_tools`
lets a round set `AwaitingReview`. Tags start at `ORIGIN_BASE` (2⁶²), clear of IDE
connection ids; an undecided review dropped (stop, chat close) sends
`Effect::CloseAllSessionDiffs`. One review gate shared with the rewrite operator.
`TextReviewRequest` gained `origin_session`. `<C-t>` (`action:llm-chat-trust`, Normal and
Insert) toggles per-chat trust, shown in the headerline, dropped on close. Tests:
`lattice-ui-tui/src/app/llm_edits.rs` (8, end to end), `differing_span` unit test, the
broker's review/ask split. Bench: `llm_edit_span`.

**Tests:** edit opens a diff and the verdict reaches the model; reject returns a usable refusal; `<C-t>`
trust applies without a diff, resets per session; review surfaces as `AwaitingReview`; stale target; several
edits queue; closing the chat rejects its pending diffs through `close_session_diffs`.

### LL.18 — Exec and permissions ✅

**Files:** `src/llm/tools/lattice.rs`, `src/acp/permission_mode.rs` moved to `src/permission/` on neutral types.

**Landed:** `run_command` (kind `Execute`, always listed, offered only when `lattice.exec` is
selected) through `lattice_core::process::run_cancellable_capped` (new: per-stream cap with
drain, own process group, timeout 120 s / max 600 s, 30 000 bytes per stream). The broker's
`Ask` path asks through `CallContext.ask` (`Answer::{Once, ForChat, Deny}`), with
`ToolSource::describe` giving the question its detail (`$ cmd`); with no asker it refuses.
The chat's asker pushes a pending permission on its store; a `Withdraw` guard answers deny
if the call is dropped. "Allow for this chat" is a per-session set, cleared on close.
`permission::{PermissionSources, install}` replaces the ACP-only popup wiring: ACP adds its
store, llm adds `chats.stores()`; `:ai-permission` moved with it. Prerequisite fix
`a62349d8`: `<CR>` in the popup was swallowed by the host's Help-kind follow-link gate, so
the menu could never be answered; it is now `help-mode`'s binding. Also: the `llm` feature
enables `tokio/process` (its `api_key_command` needed it; only `acp` had been pulling it
in), and `lattice-ai`'s llm integration tests are gated on `llm`. Tests:
`lattice-ui-tui/src/app/llm_exec.rs` (7, end to end through the popup), the broker's
`an_ask_tool_runs_only_when_allowed`, `capped_output_keeps_cap_plus_one_and_lets_the_child_finish`,
the ACP permission-mode suite on `PermissionSources`.

**Tests:** exec asks, defer leaves it pending, deny reaches the model; cancel kills the process tree; output is
capped; `:opencode-acp` permissions unchanged; a project cannot enable exec.

## Session extras

### LL.19 — Fork and retry ✅

**Files:** `src/llm/modes/chat.rs`, `src/conversation/model.rs`, `:llm-retry`.

**Landed.** `Conversation::fork_at(turn)` (the prefix before the question, its reasoning data
and tool-call arguments, and the question's text) and `drop_last_reply`;
`projection::turn_at_line` (one layout pass, as `gf`'s `source_at_line`); `LlmChats::fork` /
`retry`; `gb` and `:llm-fork` share `fork_effect`; `:llm-retry`. `TranscriptBinding.prompt`
seeds the fork's prompt in the seeding replace, taken once from `ChatSession::prompt_seed`
so a reopened buffer does not refill it. `prepare` names another provider's reasoning once
per chat (`ProviderKind::reasoning_tag`). Tests: `tests/llm_fork_retry.rs` (5),
`lattice-host/tests/llm_chat_buffer.rs::gb_forks_a_chat_with_the_question_in_its_prompt`,
model and projection unit tests. Bench: `llm_fork`.

**Tests:** fork copies history to that turn, with the prompt ready, and leaves the original untouched,
including its tool calls; retry uses current settings; switching provider drops reasoning data and says so;
retry while streaming is refused.

### LL.20 — Export and session files ✅

**Files:** `src/llm/session_file.rs`, `:llm-export`, resume command.

**Landed.** `src/llm/export.rs` (`markdown`: the projection, every block open; `org`: from
the model), `src/llm/session_file.rs` (`encode` / `decode`, `LlmSessionFiles` with its own
inbound bus, `:llm-export`, `:llm-save`, `:llm-resume`; `!` overwrites, as `:w`). The resume
command is `:llm-resume`; saving is its own verb, `:llm-save`, rather than an export format,
since the two files have different readers and trust. `Settings::text` (inverse of `set`) so
a file keeps effective values; `settings::restrict` (the project filter, for files);
`LlmChats::saved_settings` / `resume` / `create_seeded`. Tests: `tests/llm_session_file.rs`
(8, incl. runtime responsiveness, proven to fail with the decode inline), export and
checksum unit tests, `lattice-host/tests/llm_chat_buffer.rs::save_export_and_resume_go_through_real_files`.
Bench: `llm_session_file`.

**Tests:** `.md` round trip; `.org` conversion including fences; session resume restores tool ids; a tampered
or newer-version file is refused with a message; a session file cannot widen trust; no key on disk.

### LL.21 — Gemini ✅

**Files:** `src/llm/provider/gemini.rs`, fixtures.

**Tests:** the LL.3 suite, including the split-at-every-byte check, on Gemini fixtures.

**Landed.** `GeminiProvider` (`streamGenerateContent?alt=sse`, key in `x-goog-api-key`,
`parametersJsonSchema` for tools, results merged into one `user` turn of `functionResponse`
parts named from their calls); `ProviderKind::Gemini` (kind `gemini`; a provider named
`gemini` defaults to it; a key is required). Thought signatures ride on an empty reasoning
block anchored to their call (design §3 "A signature on a call"); the projection and org
export skip empty reasoning. No end marker on the wire: `finish()` is `Done` only after a
`finishReason`, else `Truncated`; `STOP` after calls is `ToolUse`. Fixtures
`gemini_text_thinking.sse`, `gemini_tool_calls.sse` (an id-less call; CRLF framing is covered by `tests/llm_gemini.rs`, whose mock replies use `\r\n`, since `.gitattributes` checks fixtures out LF). Tests: both LL.3
files extended (every fixture split at every byte, truncation, unknown chunks, in-band and
blocked-prompt errors, garbage, endless line), exact-body build tests, config tests, and
`tests/llm_gemini.rs` (a tool loop whose second request carries the signature on its call).
Bench: `llm_decode/gemini`.

## MCP (last)

### LL.22 — `rmcp` gate: `LatticeServer` over the existing WebSocket ✅

**Why:** one implementation. **A gate:** if it cannot pass, stop and tell Dhruva. The fallback is
`rmcp` client-only, with `dispatch.rs` and `protocol.rs` kept.

**Files:** `Cargo.toml` (`rmcp` under the `mcp` feature), `src/mcp/lattice_server.rs`, `src/mcp/server.rs`;
delete `src/mcp/{dispatch,protocol}.rs`.

**Tests:** `tests/mcp_ws_roundtrip.rs` unchanged and green; the catalogue equals today's by name and schema;
`selection_changed` and `at_mentioned` still reach the client; a blocking `openDiff` does not stall the read
loop; bad token still 401; **a real `claude` session driven by hand**, with the result recorded in the commit.

**Landed; the gate passed.** `rmcp` 3.6.0 (`server` feature) under
`mcp`; `src/mcp/lattice_server.rs` (`LatticeServer`, `ToolContext`, the catalogue as the
contract's JSON); `server.rs` adapts the socket (`WsTransport`) and fires teardown when the
socket ends; `dispatch.rs` and `protocol.rs` deleted. **One deviation, decided by Dhruva
2026-11-08:** `mcp_ws_roundtrip.rs` is not byte-for-byte unchanged. rmcp refuses an
`initialize` without `capabilities`/`clientInfo` and any request before `initialize`; the
test's handshake gains those two fields, and the `openDiff` test initializes before calling
(the second edit is the same principle, found after the decision). `messages.filter` holds
`rmcp` at `warn` unless named (rmcp logs each notification at `info`). Tests: roundtrip (6),
`tests/mcp_lattice_server.rs` (version pinned for a newer client, `tools/list_changed`, the
wire catalogue, `selection_changed`/`at_mentioned` after the handshake, unknown tool,
unreadable frames), `lattice_server` unit tests (catalogue equals the contract), the filter
floor. Bench: `mcp_call`.

**The hand-driven session (Dhruva, 2026-11-08, `claude` 2.1.319, GPUI).** It first failed to
connect, before any MCP frame: `claude` asks for the `mcp` WebSocket subprotocol and lattice
never echoed it (fixed separately, `e0de5d79`; pre-LL.22). `:'<,'>claude-send` was refused
with E481 (fixed separately, `f4edbb8f`). Then: `/ide` connects; `:'<,'>claude-send` puts the
`@file` mention in claude's prompt; two edits through `openDiff`, one `:diff-accept`ed and one
`:diff-reject`ed, each reaching claude; `getDiagnostics` answers; `:claude-code-stop`
disconnects. `getDiagnostics` is the only IDE tool claude offers its model: the CLI keeps the
rest of the catalogue for itself, as with VS Code. **Follow-ups found, not LL.22's:**
`openDiff` opens the file without a visible diff although accept/reject work (host-side
display, shares no code with this slice); `:'<,'>claude<Tab>` does not complete after a range
prefix.

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
