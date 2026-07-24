+++
title = "ACP UX enhancements — permission surface, usage metadata, status, and queue"
+++


> **Status:** design fragment (v0.1). Slices sequenced in
> `docs/dev/operations/slice-plans/acp-ux-enhancements.md`.
>
> **Builds on:**
> - [`ai-agent-protocol.md`](../ai-agent-protocol/) — ACP wire contract, session lifecycle,
>   supervisor architecture.
> - [`agent-ui.md`](../agent-ui/) — conversation buffer, `Conversation` store,
>   `ai-conversation` major mode, editable tail.
> - [`agent-integration.md`](../agent-integration/) — `EditorAccess` port, topology decision.
> - [`headerline.md`](../headerline/) — generic sticky header row trait.
> - [`modeline.md`](../modeline/) — modeline element system.
>
> **Audits:**
> - `crates/lattice-ai/src/acp/conversation.rs` — `Block` enum, `Conversation::apply`.
> - `crates/lattice-ai/src/acp/supervisor.rs` — `classify_permission`, `handle_permission`.
> - `crates/lattice-ai/src/acp/conversation_mode.rs` — `render_conversation`, editable tail.
> - `crates/lattice-ai/src/acp/handle.rs` — `AiState`, `AiClientHandle`.
> - `crates/lattice-host/src/modeline.rs` — built-in modeline element registration.
> - `.cargo/registry/.../v1/client.rs` — `UsageUpdate`, `Cost`, `PermissionOption` schema.

---

## 1. What is missing

Lattice already speaks ACP fluently. The supervisor spawns an agent, drives
`session/prompt`, streams `SessionUpdate` notifications into a structured
`Conversation` store, and renders them into the `*ai:opencode*` buffer. The
diff-review path (`session/request_permission` for edit ops) is live and wired
through `review_diff` → `ProgrammaticDiffBus` → verdict.

Four UX gaps remain, listed in the order a user encounters them during a
session:

**Gap 1 — Non-edit permission requests are silently denied.**
The `classify_permission` function (`supervisor.rs:436`) safe-lists read-only
tool kinds (`Read`, `Search`, `Fetch`, `Think`, `SwitchMode`) and routes `Edit`
with a diff to `Review`. Everything else (`Execute`, `Delete`, `Other`) falls
through to `Deny` — the agent is told "Reject" and the user never sees the
request. This is correct for safety but creates a jarring UX: the user's agent
says "I need to run a command" and nothing happens.

**Gap 2 — Cost and token metadata is not surfaced.**
The ACP `usage_update` notification (`used` tokens, `size` context window,
optional `cost` amount/currency) is silently dropped by the catch-all
`_ => {}` in `Conversation::apply` (`conversation.rs:127`). Users have no
visibility into context pressure or cumulative spending.

**Gap 3 — Processing status is not explicitly displayed.**
The conversation buffer shows tool-call status per-block (`[running]`,
`[completed]`, etc.) but has no global "what is the agent doing right now?"
indicator — idle vs. streaming text vs. executing a tool vs. awaiting the
user's permission decision. This is disorienting in a thin terminal: the user
sees a static buffer and cannot tell whether the agent is thinking, stuck, or
done.

**Gap 4 — Queued prompts are invisible.**
The ACP protocol does not support concurrent `session/prompt` calls within one
session. If the user types a new message while the agent is still responding,
the client must queue it. Today Lattice has no queue: a second prompt during
an active turn either races or is silently dropped. The user gets no feedback
that their message was queued and will be sent when the current turn finishes.

---

## 2. Paramount-goal alignment

> **UX (higher court):** a user watching their agent work should never wonder
> "is it doing anything?" Four unambiguous states — idle, thinking, working,
> awaiting input — plus a visible queue when they type ahead. Permission
> requests for non-file operations are surfaced as inline prompts in the
> conversation, not denied silently. Cost and context pressure are visible so
> the user can decide to compact or start a new session.

> **Paramount #1 (perf):** status is a headerline string (virtual row 0), not
> a per-frame recomputation. Queue state is a counter, not a live inspection.
> Usage metadata arrives as a `SessionUpdate` notification and is accumulated
> cheaply. No new per-frame work, no UI-thread blocking.

> **Paramount #3 (everything-is-a-buffer):** the conversation IS a Document.
> No new `BufferKind`, no panel. Status lives in the generic headerline trait
> (`headerline.md`), not a bespoke widget. The permission inline prompt renders
> as blocks in the existing conversation buffer, not a popup or separate dialog.

> **Paramount #4 (async):** the permission awaiting channel is a oneshot in the
> store, polled by the supervisor task; the mode never blocks. The queue is an
> `mpsc::UnboundedSender` in the handle, drained by the driver task.

---

## 3. Data model changes

### 3.1 `Conversation` struct — new fields

```rust
pub struct Conversation {
    pub turns: Vec<Turn>,
    // NEW:
    pub usage: Option<UsageSnapshot>,       // latest usage_update
    pub status: SessionStatus,              // derived global status
}

pub struct UsageSnapshot {
    pub used: u64,              // tokens currently in context
    pub size: u64,              // total context window
    pub cost: Option<Cost>,     // cumulative session cost
}

pub struct Cost {
    pub amount: f64,
    pub currency: String,       // ISO 4217
}

pub enum SessionStatus {
    Idle,
    Thinking,                   // streaming text
    Executing { tool: String }, // tool_call in_progress
    AwaitingPermission,         // permission request pending
}
```

`status` is set by the supervisor when it sends `SessionUpdate`s — every
`AgentMessageChunk` → `Thinking`, every `ToolCall` with status `Pending` →
`Executing`, every `request_permission` dispatch → `AwaitingPermission`.
`Idle` is set when the `session/prompt` response arrives (`StopReason`).

### 3.2 `Block` enum — new variant

```rust
pub enum Block {
    Text(String),
    Reasoning(String),
    ToolCall { id: String, title: String, status: ToolStatus },
    Edit { path: String, status: EditStatus },
    // NEW:
    Permission {
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
        status: PermissionStatus,
    },
}
```

`PermissionStatus`:
```rust
pub enum PermissionStatus {
    Pending,     // awaiting user decision
    Allowed,     // user allowed (optionId stored)
    Denied,      // user denied
}
```

### 3.3 `ConversationStore` — pending permission map

```rust
pub struct ConversationStore {
    inner: Arc<Mutex<Conversation>>,
    publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>,
    // NEW:
    pending_permissions:
        Arc<Mutex<HashMap<String, oneshot::Sender<PermissionOutcome>>>>,
}
```

The `ConversationStore` gains two methods:

```rust
impl ConversationStore {
    /// Push a Permission block and register the oneshot for its resolution.
    fn push_permission_request(
        &self,
        session: SessionKey,
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
        responder: oneshot::Sender<PermissionOutcome>,
    );

    /// Called by the mode when the user makes a choice.
    fn resolve_permission(
        &self,
        id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), ()>;
}
```

### 3.4 `AiState` — new fields

```rust
pub struct AiState {
    pub running: bool,
    pub provider: Option<&'static str>,
    pub session: Option<SessionKey>,
    pub auto_accept: bool,
    // NEW:
    pub status: SessionStatus,
    pub usage: Option<UsageSnapshot>,
    pub queue_len: usize,
}
```

`AiState` is published via `AiStateChanged` (existing event path, rename from
`AiState` republish) after every meaningful transition. The headerline
producer, the modeline element, and any other consumer react to the same event.

---

## 4. Feature-by-feature design

### 4.1 AUX-1 — Permission surfacing for non-edit operations

**Flow:**

```
Agent                               Supervisor                       ConversationStore
  │                                      │                                    │
  │  session/request_permission           │                                    │
  │  (tool: Execute, kind: bash)         │                                    │
  ├─────────────────────────────────────►│                                    │
  │                                      │  classify_permission()             │
  │                                      │  → PermissionDecision::AskUser     │
  │                                      │                                    │
  │                                      │  store.push_permission_request(    │
  │                                      │    id, title, options, tx)        │
  │                                      ├───────────────────────────────────►│
  │                                      │                              Block::Permission
  │                                      │                              Push { id, Pending }
  │                                      │                              Publish ConversationUpdated
  │                                      │◄───────────────────────────────────┤
  │                                      │                                    │
  │                                      │  rx.await  ─── BLOCK ──           │
  │                                      │      │                            │
  │  User sees "Allow agent to run:      │      │                            │
  │    cargo test?" inline               │      │                            │
  │  User presses "allow"                │      │                            │
  │                                      │      │                            │
  │                              tx.send(AllowOnce)                          │
  │                                      │◄─────┤                            │
  │                                      │  store.resolve_permission(id,     │
  │                                      │    PermissionOutcome::AllowOnce)  │
  │                                      ├───────────────────────────────────►│
  │                                      │                              Block::Permission
  │                                      │                              { status: Allowed }
  │                                      │                              Publish ConversationUpdated
  │                                      │                                    │
  │  responder.respond(Selected)         │                                    │
  │◄─────────────────────────────────────┤                                    │
  │                                      │                                    │
```

**Key design decisions:**

- **`classify_permission` gains a third branch:** `PermissionDecision::AskUser`.
  Non-read, non-edit operations route here instead of `Deny`.
- **The oneshot channel is owned by the `ConversationStore`**, not by a separate
  map on the supervisor. This keeps the blocking-future next to the data it
  renders, and avoids a second lock.
- **The mode does not directly resolve the oneshot.** Instead it calls
  `store.resolve_permission(id, outcome)` which both updates the block in the
  model AND sends through the channel. Single atomic operation.
- **Trust mode (`auto_accept`) still bypasses.** When `auto_accept` is set,
  even non-edit ops get `AutoAllow` — the user has opted in.

**Permission block rendering** (`render_conversation`):

```
you:
run cargo test

opencode:
  ◌ Allow agent to run: cargo test?  [pending]
      1: Allow once    (a)
      2: Always allow  (A)
      3: Reject        (r)
```

`◌` (U+25CC, dotted circle) indicates a pending permission. On resolution the
block updates in place (decoration update, not content rewrite):

```
  ✓ Allow agent to run: cargo test   [allowed]
```

`✓` (U+2713) for allowed, `✗` (U+2717) for denied.

The inline block renders the request and its resolution for the transcript
record. It is **not** the interaction surface — see §5.3.

Superseded: earlier revisions of this document specified a transient keymap on
permission-block focus (`a` / `A` / `r` / `R`) plus `:ai-allow` / `:ai-deny`
ex-commands. That design hardcoded the four `PermissionOptionKind` values. The
wire does not work that way: `session/request_permission` carries an
**agent-supplied `Vec<PermissionOption>`**, each with its own `option_id`
(an opaque `String`), human-readable `name`, and a `kind` that is only a *hint*.
A fixed four-chord keymap cannot express an option list it does not know, and
those chords would exist in the conversation buffer at all times for an
interaction that is inherently momentary.

### 4.2 AUX-2 — Cost/token metadata

**Accumulation:**

`Conversation::apply` gains a match arm:

```rust
SessionUpdate::UsageUpdate(u) => {
    self.usage = Some(UsageSnapshot {
        used: u.used,
        size: u.size,
        cost: u.cost.map(|c| Cost {
            amount: c.amount,
            currency: c.currency,
        }),
    });
}
```

`usage` is the *latest* snapshot, not a cumulative log. The ACP spec says
`usage_update` is cumulative session cost and current context
size — the latest value subsumes all earlier ones.

**Display — headerline:**

The `ai-conversation` mode registers a `HeaderlineProvider` (per
`headerline.md` trait) that reads the `Conversation` usage snapshot and
returns a formatted string:

```
CPU: 31.4K/200K · $0.045   │ Working: cargo test (step 2/4)
```

The headerline is the right home because:
- It is visible at all times (pinned above the scrollback).
- It is per-buffer, not per-pane (the modeline is per-pane and shared).
- Following the `async_buffer_status_in_headerline` convention (CLAUDE.md).

Format: `used/size` tokens + optional cost, right-aligned after the status
message. Both are omitted when `usage` is `None` (no `usage_update` received).

The provider republishes its header on `ConversationUpdated` events (which
fire after every `ConversationStore` mutation, including `usage_update`).

### 4.3 AUX-3 — Explicit processing status

**Status derivation:**

The `SessionStatus` is derived by the supervisor from the active prompt turn's
state. The rule:

| When | `SessionStatus` |
|------|-----------------|
| No active `session/prompt` | `Idle` |
| Agent message chunks streaming | `Thinking` |
| `ToolCall` with status `Pending` or `InProgress` | `Executing { tool }` |
| `session/request_permission` dispatched to user | `AwaitingPermission` |
| Turn ends (any `StopReason`) | `Idle` |

The supervisor sets `Conversation.status` via `store.set_status(...)` before
each `ConversationUpdated` publish.

**Status rendering — headerline:**

The `HeaderlineProvider` renders the status as a prefix string:

| Status | Headerline text |
|--------|-----------------|
| `Idle` | `Ready` (dim) |
| `Thinking` | `Thinking…` (animated dot cycle via periodic republish) |
| `Executing { tool }` | `Working: {tool}` |
| `AwaitingPermission` | `Awaiting your approval…` (accent colour) |

The headerline producer subscribes to `ConversationUpdated` and republishes
its content hash. The renderer re-draws only when the hash changes.

### 4.4 AUX-4 — Queue

**Architecture:**

The queue lives on the **client side** (Lattice), not in the ACP protocol.
ACP only supports one `session/prompt` at a time per session.

```rust
// In AiClientHandle:
struct Inner {
    state: AiState,
    // NEW:
    prompt_queue: mpsc::UnboundedSender<QueuedPrompt>,
    queue_len: Arc<AtomicUsize>,
}

struct QueuedPrompt {
    parts: Vec<ContentBlock>,
    responded: oneshot::Sender<PromptResult>,
}
```

**Flow:**

1. User submits a prompt while `state.running == true`.
2. `AiClientHandle::prompt` pushes the message into the `mpsc` channel and
   increments `queue_len`.
3. The supervisor's driver task (`supervisor_loop`) reads from the channel
   after each `session/prompt` response (in the `-> Idle` transition).
4. It sends the next queued prompt, decrements `queue_len`, and publishes
   `AiStateChanged`.
5. The user sees immediate feedback: `⌛ 1 queued` in the headerline.

**Queue is lost on disconnect.** Queue entries are not persisted — they live
in the `UnboundedSender` memory. If the agent disconnects, remaining queued
prompts get `Err(Disconnected)`.

**Display — headerline:**

When `queue_len > 0`, the headerline appends:

```
⌛ 1 queued   │ Working: cargo test
```

The queue count decrements in real time as each prompt is dispatched.

---

## 5. Cross-cutting concerns

### 5.1 One headerline producer, four consumers

All four features (status, usage, queue, plus existing info) are rendered
through **one** `HeaderlineProvider` registered by the `ai-conversation` mode.
This avoids four independent header slots competing for the same row.

```
HeaderlineProvider (ai-conversation mode)
  ├─ SessionStatus     → "Thinking…"
  ├─ UsageSnapshot     → "31.4K/200K · $0.045"
  ├─ queue_len         → "⌛ 1 queued"
  └─ (existing)        → "trust mode" indicator
```

The provider assembles a single formatted string. It listens to
`ConversationUpdated` (for status/usage) and `AiStateChanged` (for queue_len)
and republishes when any input changes.

### 5.2 Cross-renderer discipline

The headerline is a generic `HeaderlineProvider` trait (`headerline.md`) that
both renderers (TUI + GPUI) consume identically. The `HeaderlineProvider` for
`ai-conversation` mode returns a string; both renderers paint it as a sticky
virtual row. No renderer-specific branches.

Permission blocks are `Block` variants rendered by `render_conversation` — a
pure function that produces `(content_lines, decorations)`. Both renderers
consume the same projection. The TUI and GPUI peers move in lockstep.

### 5.3 Permission menu

A permission request is a momentary interaction over a dynamic option list. It
gets a momentary surface: a popup whose buffer *is* the menu. See
[popup-api.md](../popup-api/) for the surface; this section covers only what
`lattice-ai` contributes.

**The buffer.** On the first `Block::Permission { status: Pending }`, the mode
registers a synthetic `*ai-permission*` buffer with major mode
`ai-permission-mode` and projects the request into it: title, optional
description, and one line per `PermissionOption`, numbered in wire order.

```
Allow `cargo test`?

  1  Allow once
  2  Allow always
  3  Reject once
  4  Reject always

  Esc  decide later
```

The lines are generated from the agent's list. Four options is the common case,
not the contract.

**The keymap.** `ai-permission-mode` binds `1..=N` at activation, where `N` is
the number of options in *this* request — the same shape as
`completion_popup_layer_bindings`, which computes its bindings when the
completion popup appears. Nothing is bound in the conversation buffer. When the
popup buffer is dismissed the bindings cease to exist, because the buffer does.

`<CR>` on an option line resolves it too, so the menu also works by cursor —
the file-tree / oil `entry_at_line` model.

**Resolution.** The handler maps the chosen line to its `PermissionOption` and
calls `store.resolve_permission(id, option_id)`, which fires the
`oneshot::Sender<PermissionOutcome>` the supervisor is parked on
(`handle_permission`, `PermissionDecision::AskUser`). The supervisor answers ACP
with `RequestPermissionOutcome::Selected(option_id)`. The popup dismisses; focus
and modal state return to the prompt (see popup-api.md §5 — restoring Insert is
a fix the primitive must carry).

**Opening.** The drain flags a newly-pending id; the mode's tick callback returns
`Effect::OpenPopup { buffer, Centered, PopupFocus::Steal }`. The agent is blocked
awaiting the answer, so stealing focus costs the user nothing they could
otherwise be doing with the agent — and `Esc` defers without answering.

**Deferral and queueing.** `Esc` dismisses the popup and leaves the request
`Pending`; the inline block keeps rendering it, and `:ai-permission` reopens the
menu for the oldest pending request. Concurrent requests queue and the next opens
on resolve, following the LSP `showMessageRequest` precedent
(`lsp.rs::open_next_queued_show_message_request`).

**Trust mode.** Unchanged: `<C-t>` auto-allow short-circuits in
`resolve_decision` before `AskUser`, so no block is pushed and no popup opens.

### 5.4 Tool-call expansion

Tool calls render collapsed and expand to show their detail.

**Ingest currently discards the detail.** `Conversation::apply` keeps only
`{ id, title, status }` from `SessionUpdate::ToolCall`, and only `status` from
`ToolCallUpdate`. The ACP schema carries `kind`, `raw_input`, `raw_output`,
`content: Vec<ToolCallContent>` (text / **diff** / terminal), and `locations` —
on both the initial call *and* on updates, since detail commonly arrives
incrementally. `Block::ToolCall` grows those fields; `update_tool_status`
generalises to a `merge_tool_update` that folds every `Some` field.

**Expansion is a real fold, not a projection toggle.** `render_conversation`
always emits the detail rows beneath the summary line. A `ToolCallFoldSource`
(`FoldSource`, registered through `FoldOverlayServiceHandle` exactly as
`DiffMode::on_activate` registers its hunk sources) returns one `Fold` per tool
call over its detail range, `closed: true` by default.

The streaming-coherence problem solves itself: `Fold::identity` exists precisely
to "carry closed-state across recomputes … so that adding or removing lines
elsewhere in the buffer doesn't reopen this fold". Set
`identity = hash(tool_call_id)`. The transcript may grow arbitrarily above a
tool call on every token; its expansion state is keyed to the call, not to a
line range.

Consequences worth naming:

- `za` toggles. No new chord, no line→block span map, no mode-owned expansion
  set — the fold *is* the state, and it is per-buffer, where it belongs.
- Folded rows are elided at the cell layer, so a collapsed transcript costs the
  renderer nothing per hidden detail line (paramount #1).
- The `Block::Reasoning` doc comment claims reasoning is "folded by default in
  the projection". It is not — the projection merely prefixes each line with
  `│`. Reasoning becomes a second `FoldSource` consumer, and the comment becomes
  true.

---

## 6. Error handling

| Failure mode | Behaviour |
|---|---|
| `usage_update` with zero-size context window | Accept but display `? / ? tokens` |
| Permission block dropped before user acts | `oneshot` receiver drops → `Cancelled` response, block shows `Denied` |
| Queue overflow (unlikely, unbounded channel) | Unlikely in practice; cap at 64 as safety net |
| `resolve_permission` for already-resolved block | Log + skip; idempotent |
| Headerline provider panics | Catch + fall back to empty string; log error |

---

## 7. Testing

- **Model (unit):** `Conversation::apply` with `UsageUpdate` → `usage` field set.
  `Conversation::apply` with `ToolCall` + `ToolCallUpdate` → status transitions.
  `ConversationStore::push_permission_request` → block created + sender registered.
  `ConversationStore::resolve_permission` → block updated + channel fired.
- **Projection (unit):** `render_conversation` with `Permission` block → expected
  text + decorations. `render_conversation` with `usage` → headerline includes
  token/cost text.
- **Supervisor (integration):** mock ACP transport → send `request_permission`
  for non-edit op → assert `AskUser` path. Send `usage_update` → assert usage
  stored. Send second `session/prompt` while first active → assert queued.
- **Queue (integration):** drive two `prompt` calls → first starts, second
  queued. Headerline shows `⌛ 1 queued`. On first completion, second auto-sends.
- **Permission menu (integration):** a `Pending` block opens the `*ai-permission*`
  popup with one line per agent-supplied option. Pressing `2` resolves the
  request with that option's `option_id` and dismisses the popup; focus and
  modal state return to the prompt. `Esc` dismisses and leaves the block
  `Pending`; `:ai-permission` reopens it. Two concurrent requests queue; the
  second opens when the first resolves.
- **Permission menu (unit):** the keymap binds exactly `1..=N` for an N-option
  request — a three-option and a five-option request both bind correctly, so
  nothing depends on `PermissionOptionKind` having four values.
- **Tool-call folds (integration):** a tool call renders collapsed; `za` expands
  it to show `raw_input` / `raw_output` / diff content. A streamed chunk that
  grows the transcript above an expanded call leaves it expanded (fold identity
  is keyed on `tool_call_id`, not on the line range).
