# Agent conversation UI

A terminal-native, buffer-native chat surface for the ACP agent (opencode
today; any ACP provider tomorrow). The user reads the conversation as
scrollback in Normal mode and composes prompts in Insert mode — the whole
"agent edits my code" loop lives in one buffer, rendered by both renderers
through the ordinary Document path. This is the lattice analog of opencode's
TUI, but expressed as vim modal editing over a Document rather than a bespoke
full-screen widget.

Design revision: v0.1 (initial). Slice sequencing lives in
`docs/dev/operations/slice-plans/agent-ui.md`.

## Why this shape

The ACP supervisor already receives structured `SessionUpdate`s from the agent
(message chunks, thought chunks, tool calls, tool-call updates, permission
requests). Today it **flattens all of them to plain-text log records** in the
`AiLogger` ring and surfaces them through `:ai-log`. That single buffer does two
unrelated jobs — it is both the conversation and the diagnostic trace (see
`docs/dev/operations/slice-plans/agent-integration.md`, "No agent-output-surface
redesign"). This feature performs that split:

- **Conversation sources** (`AgentText` / `Reasoning` / `ToolCall`) feed a new
  structured `Conversation` store, projected into the `*ai:opencode*`
  conversation buffer.
- **Trace sources** (`Client` / `Lifecycle` — handshake, permission requests,
  decode failures, session lifecycle) stay in the `AiLogger` ring, and `:ai-log`
  becomes trace-only: the `:lsp-server-log` analog.

Building the conversation UI *is* the split; there is no separate follow-up.

## Paramount-goal alignment

> **UX (higher court):** the terminal feel — Normal-mode scrollback, Insert-mode
> prompt — is muscle memory from every REPL/agent TUI. Tool-call status and
> streaming update in place (decorations / virtual rows), so only the edited
> region changes per frame; no whole-viewport restyle, no flicker.
> **Paramount #1 (perf):** the buffer is a *projection* of an off-thread model;
> the mode's drain task re-renders via the debounced `apply_edit_batch_blocking`,
> never on the UI thread. Element fan-out stays O(viewport) — virtual-row
> headers and decorations, not per-token widgets.
> **Paramount #3 (everything-is-a-buffer):** the conversation IS a Document with
> an `ai-conversation` major mode. No `BufferKind` branch, no bespoke panel. Both
> renderers draw it for free. The mode owns its full surface — keymap,
> subscriptions, decorations, buffer production, and the action-handler bodies.
> **Paramount #4 (async):** the `Conversation` store is mutated only by the
> supervisor task; the mode subscribes to `ConversationUpdated` exactly as
> `AiLogMode` subscribes to `AiLogPushed`. Nothing blocks the editor thread.

## Data model

A structured conversation, owned by the ACP supervisor and read by the mode:

	struct Conversation {
		turns: Vec<Turn>,
	}

	struct Turn {
		role: Role,            // User | Assistant
		blocks: Vec<Block>,
	}

	enum Block {
		Text(String),          // streamed assistant / user text
		Reasoning(String),     // streamed thinking; folded by default
		ToolCall {
			name: String,
			status: ToolStatus, // Pending | Running | Ok | Err
			detail: String,     // args summary / result / error
		},
		Edit {
			path: PathBuf,
			session: DiffSessionRef, // the review_diff session, if any
			status: EditStatus,      // Proposed | Accepted | Rejected
		},
	}

The supervisor maps `SessionUpdate` onto the model:

- `AgentMessageChunk(text)` → extend the last assistant `Text` block (or open one).
- `AgentThoughtChunk(text)` → extend the last `Reasoning` block.
- `ToolCall(tc)` → push a `ToolCall` block, status `Running`.
- `ToolCallUpdate(u)` → update the matching block's `status`/`detail` in place.
- `session/request_permission` for an edit → push an `Edit` block, status
  `Proposed`, and open the review flow (below).

Streaming is append-to-last-block; the model never rewrites earlier turns. The
store lives behind a snapshot-able handle and publishes `ConversationUpdated`
(typed bus event, mirroring `AiLogPushed`) after each mutation.

## The `ai-conversation` major mode

Owned by `lattice-ai/acp` (mode owns its surface), registered when the ACP
transport installs. It mirrors `AiLogMode`:

- `on_activate` subscribes to `ConversationUpdated` and spawns a drain task.
- The drain task re-projects the `Conversation` snapshot into the buffer via
  `apply_edit_batch_blocking` (debounced; off the UI thread). Owner writes bypass
  the read-only Insert/operator enforcement, as synthetic buffers already do.
- Structure is expressed with existing primitives, not new render paths:
  - **virtual-row turn headers** (`you` / `opencode`),
  - **in-place tool-call status** (`▸ edit parse.rs  running` → `✓`), updated by
    re-projecting the changed block's decoration, not by rewriting content lines,
  - **fold regions** for `Reasoning` blocks (collapsed by default),
  - a **`[diff]` decoration** on `Edit` blocks that links to the review session.

The buffer is `*ai:opencode*` (synthetic name slot, `:ls`-visible, read-only from
the user's perspective — history is never user-editable).

## Modal input — the terminal REPL

The prompt is a **marked editable region at the tail** of the buffer, after the
last turn. The read-only/editable boundary is carried by the *vim modal state*,
not a hard text barrier:

- **Normal mode** — the cursor roams the entire buffer; scroll, search, yank,
  `gg`/`G` over the whole conversation. Reading is unrestricted; no edit lands.
- **Insert mode** — the `ai-conversation` mode binds the insert-entering chords
  (`i` / `a` / `o` / `A` / …) to first **relocate the cursor into the prompt
  region**, so Insert only ever edits the prompt. History is unreachable from
  Insert, so it cannot be mutated.
- **Enter** (Insert) sends the prompt via `AiClientHandle::prompt`, clears the
  region, and the user turn lands in history. **Ctrl-C** interrupts the active
  turn (a new `AiCmd::Interrupt`; `Stop` still ends the session).

This is the one **new reusable primitive**: an editable region at the tail of an
otherwise owner-written buffer (the comint pattern). It is a generic
`Mode::editable_tail()` declaration (`EditableTail`, structural — trailing lines +
first-line min byte, relative to the buffer end), not an
`ai-conversation`-specific render path — future `*scratch*` / REPL buffers declare
their own tail. The host's read-only edit gate
(`Editor::apply_edit_blocking`) consults it uniformly; the renderer needs no
special path, since the prompt is plain buffer text.

## Diff review + approval

The agent proposes a file edit as an ACP `session/request_permission`. Two paths,
selected by the per-session `auto_accept` flag:

- **Review (default).** The supervisor opens a `review_diff` session (the same
  `lattice-agent` primitive the MCP `openDiff` path uses). The conversation's
  `Edit` block shows `[diff]`; activating it opens lattice's diff view. Accept →
  grant the permission and let the write land; reject → deny. The **permission
  response is gated on the diff verdict** — the supervisor awaits the user's
  accept/reject before answering the agent, exactly the openDiff-blocks-on-verdict
  pattern already in the codebase. Read-only tools (`read_file`, `grep`, `list`)
  auto-run without a prompt.
- **Trust.** `auto_accept` on → `request_permission` auto-grants and edits apply
  without the diff gate. A chord in the `ai-conversation` mode flips the flag and
  echoes the state (the Claude Code auto-accept / opencode permission-mode analog).

## Entry point

`:opencode` **starts the agent and opens the conversation buffer** in the current
pane (start + open unified, per the user's decision). The buffer's prompt is the
primary way to talk to the agent; `:ai-prompt` / `:ai-stop` remain as the
headless/scriptable path.

## Data flow

	opencode ⇄ ACP (connection) ⇄ supervisor
	  ├─ conversation sources → Conversation store → ConversationUpdated
	  │                          → ai-conversation mode → *ai:opencode* buffer (off-thread)
	  └─ trace sources        → AiLogger ring → :ai-log buffer (trace-only)
	user (Insert @ prompt) → AiClientHandle::prompt → supervisor → opencode
	agent edit + request_permission → review_diff → diff view → verdict → permission response

## Everything-is-a-buffer checks

- The conversation is a `Document`; no code branches on a new `BufferKind`. It
  must pass `multibuffer_is_a_regular_buffer.rs`'s spirit: motions, scroll,
  cursor, `:ls`, `:b <name>` behave as for any Document.
- The editable-tail is a per-buffer property + a mode keymap, not a kind-gate in
  the renderer.
- The mode owns keymap, subscriptions, decorations, buffer production, and the
  action-handler bodies (`:opencode` open, send, interrupt, trust-toggle) — the
  host gains no `Editor::` method and no new `Action` variant.
- TUI and GPUI move in lockstep: the conversation renders through the Document
  path, so new decoration / virtual-row kinds land in both renderers in the same
  slice.

## Rejected alternatives

- **Plain-text markdown buffer.** Streaming a tool-call from `running`→`✓` means
  rewriting content lines mid-buffer, risking the flicker the keystroke contract
  forbids, and it strains paramount #1 (line rewrites aren't O(viewport)).
  Decorations/virtual-rows update status without touching content lines.
- **Multibuffer of message-excerpts.** The multibuffer substrate is built for
  *source-file excerpts* (search results, narrow); chat messages are not
  file-backed excerpts, and the editable input tail has no natural home in a
  composed buffer. Wrong substrate (heuristic #2).
- **Bespoke agent panel/pane widget.** Violates paramount #3 outright; forfeits
  the both-renderers-for-free dividend and re-implements motion/scroll/yank.

## Error handling

- opencode not installed / spawn fails → `Client` trace record + echo; the
  conversation buffer surfaces an error block.
- Connection drop mid-turn → `Lifecycle` trace + a "agent disconnected" block; the
  prompt region stays usable to restart.
- Malformed `SessionUpdate` → log + skip; never panic on the hot path.
- Diff apply failure → echo error, keep the session; the `Edit` block shows `Err`.

## Testing

- **Model:** `SessionUpdate` → `Conversation` transitions — chunk extends last
  block; tool-call status updates in place; edit opens an `Edit` block.
- **Projection:** buffer text + decorations match a model snapshot; streaming
  appends without rewriting earlier turns.
- **Modal input:** Insert relocates to the prompt; Normal-mode motions roam;
  history is not mutated by any Insert/operator; Enter sends and clears.
- **Diff/approval:** `request_permission` → diff verdict → allow/deny response
  (mock connection); trust mode auto-grants.
- **Renderer parity:** TUI and GPUI both render the conversation buffer (throughput
  + runtime-responsiveness coverage per the provider-test rule).
- **Failure modes:** spawn-fail, disconnect, malformed update — each exercised.
