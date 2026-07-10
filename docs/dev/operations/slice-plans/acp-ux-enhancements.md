# ACP UX enhancements — Slice Plan (AUX)

Sequencing for surfacing permission requests for non-edit operations (AUX-1),
cost/token usage metadata (AUX-2), explicit processing status (AUX-3), and
queue status (AUX-4).

- **Design:** [`acp-ux-enhancements.md`](../../architecture/acp-ux-enhancements.md)
  (data model, flow, rejected alternatives, test plan).
- **Builds on:** `agent-ui.md` (conversation buffer, Conversation store,
  `ai-conversation` mode), `ai-agent-protocol.md` (supervisor, ACP wire),
  `headerline.md` (headerline provider trait).
- **Audit references:**
  - `crates/lattice-ai/src/acp/conversation.rs` — `Block` enum, `Conversation::apply`.
  - `crates/lattice-ai/src/acp/supervisor.rs` — `classify_permission`, `handle_permission`.
  - `crates/lattice-ai/src/acp/conversation_mode.rs` — `render_conversation`.
  - `crates/lattice-ai/src/acp/handle.rs` — `AiState`, `AiClientHandle`.
  - `crates/lattice-host/src/modeline.rs` — built-in modeline elements.

Status icons: 📝 planned · 🚧 in progress · ✅ landed.

Every slice ships its doc/bench/test trio (CLAUDE.md four-artefacts rule).
TUI + GPUI parity moves in lockstep on every renderer-touching slice.

---

## Slice AUX-0 — Design doc + slice plan ✅

Create this architecture fragment and slice plan. (This slice.)

**Landed:** `docs/dev/architecture/acp-ux-enhancements.md` (design),
`docs/dev/operations/slice-plans/acp-ux-enhancements.md` (slice plan).

---

## Slice AUX-1 — Permission surfacing for non-edit operations

**Design ref:** §3.2 (`Block::Permission`), §3.3 (`pending_permissions` map),
§4.1 (flow, `classify_permission` third branch), §5.3 (keymap).

**Impact surface:**

| File | Change |
|---|---|
| `crates/lattice-ai/src/acp/conversation.rs` | Add `Block::Permission` variant, `PermissionStatus` enum. Add `pending_permissions` map to `ConversationStore`. Add `push_permission_request()` and `resolve_permission()`. `PermissionStatus::Pending` → `render_conversation` renders numbered options. |
| `crates/lattice-ai/src/acp/supervisor.rs` | Add `PermissionDecision::AskUser` variant. `classify_permission`: `Execute`/`Delete`/`Other` → `AskUser` (instead of `Deny`). `handle_permission`: `AskUser` creates oneshot, calls `store.push_permission_request()`, awaits `rx`. |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | `render_conversation`: match `Block::Permission`, render `◌`/`✓`/`✗` prefix, options list, status tag. Keybindings for `a`/`A`/`r`/`R` → `AiAllow` / `AiAllowAlways` / `AiDeny` / `AiDenyAlways`. Actions wired to `store.resolve_permission()`. |
| `crates/lattice-ai/src/acp/actions.rs` | Register `AiAllow`, `AiAllowAlways`, `AiDeny`, `AiDenyAlways` ex-commands + actions. |

**Tests:**

| Test | Type | What it asserts |
|---|---|---|
| `permission_block_created_for_execute` | Unit | `ConversationStore::push_permission_request` creates a `Block::Permission` with status `Pending`. |
| `permission_block_resolved_allowed` | Unit | `resolve_permission` updates block status to `Allowed` and sends through the oneshot. |
| `classify_permission_ask_user` | Unit | Non-edit, non-read tool kind → `AskUser`. |
| `classify_permission_denied_not_ask_user_in_trust_mode` | Unit | `auto_accept=true` → `AutoAllow` even for `Execute`. |
| `render_permission_block_pending` | Unit | `render_conversation` with `Block::Permission { Pending }` → includes `◌` prefix and option numbers. |
| `render_permission_block_resolved` | Unit | `Block::Permission { Allowed }` → includes `✓`. |
| `ai_allow_action_resolves_permission` | Integration | Keybind `a` in `ai-conversation` mode with pending permission → block resolves, permission response sent. |
| `ai_allow_noop_without_pending_permission` | Integration | `AiAllow` fires with no pending permission → no-op, no crash. |

---

## Slice AUX-2 — Cost/token metadata display ✅

**Design ref:** §3.1 (`UsageSnapshot` in `Conversation`), §4.2 (accumulation,
headerline display).

**Impact surface:**

| File | Change |
|---|---|
| `crates/lattice-ai/src/acp/conversation.rs` | Add `UsageSnapshot` struct, `Cost` struct. Add `usage: Option<UsageSnapshot>` to `Conversation`. Match `SessionUpdate::UsageUpdate(u)` in `Conversation::apply` → set `self.usage`. |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | Add `ConversationHeaderline` (`Headerline` impl), register via `VirtualRowRegistrar` in `on_activate`, bump version in drain. Format as `"CPU: 31.4K/200K · $0.045 USD"`. |
| `crates/lattice-mode/src/activator.rs` | Add `VirtualRowRegistrar` trait. |
| `crates/lattice-host/src/editor_boot.rs` | Create `VirtualRowProviderRegistry` in Phase A, register as `Arc<dyn VirtualRowRegistrar>` service. |
| `crates/lattice-host/src/virtual_rows_worker.rs` | `impl VirtualRowRegistrar for VirtualRowProviderRegistry`. |

**No supervisor changes** — `SessionUpdate` already flows through
`Conversation::apply`.

**Tests:**

| Test | Type | What it asserts |
|---|---|---|
| `usage_update_stored` | Unit | `Conversation::apply` with `UsageUpdate(53000, 200000, Some(Cost(0.045, "USD")))` → `self.usage == Some(UsageSnapshot { used: 53000, size: 200000, cost: Some(Cost(0.045, "USD")) })`. |
| `usage_update_overwrites` | Unit | Two `UsageUpdate`s → latest values stored. |
| `usage_update_no_cost` | Unit | `UsageUpdate(used, size, None)` → `self.usage.cost == None`. |
| `headerline_shows_tokens` | Integration | After `usage_update`, headerline provider returns string containing `"31.4K"` and `"200K"`. |
| `headerline_shows_cost` | Integration | With cost → headerline contains `"$0.045"`. |
| `headerline_empty_without_usage` | Integration | No `usage_update` received → headerline omits token/cost segment. |

---

## Slice AUX-3 — Explicit processing status ✅

**Design ref:** §3.1 (`SessionStatus` enum), §4.3 (derivation, rendering).

**Impact surface:**

| File | Change |
|---|---|
| `crates/lattice-ai/src/acp/conversation.rs` | Add `SessionStatus` enum. Add `status: SessionStatus` to `Conversation`. Add `set_status(&self, status: SessionStatus)` to `ConversationStore`. |
| `crates/lattice-ai/src/acp/supervisor.rs` | Call `store.set_status(...)` before each `ConversationUpdated` publish: `AgentMessageChunk` → `Thinking`; `ToolCall` with `Pending`/`InProgress` → `Executing { tool }`; dispatching `request_permission` → `AwaitingPermission`; `session/prompt` response received → `Idle`. |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | Extend `HeaderlineProvider`: read `store.snapshot().status`, format status string. Animate-dot `Thinking…` via periodic timer (1s tick republish). |

**Timer note:** The animated `Thinking…` dot cycle is optional polish. Ship
static `"Thinking…"` first; animation is a follow-up.

**Tests:**

| Test | Type | What it asserts |
|---|---|---|
| `status_idle_default` | Unit | New `Conversation` → `status == SessionStatus::Idle`. |
| `set_status_publishes` | Unit | `store.set_status(SessionStatus::Thinking)` → next snapshot shows `Thinking`. |
| `status_thinking_on_message_chunk` | Integration | Supervisor receives `AgentMessageChunk` while processing → `store.status == Thinking`. |
| `status_executing_on_tool_call` | Integration | `ToolCall` with `Pending` → `status == Executing { tool: "edit" }`. |
| `status_awaiting_permission` | Integration | Supervisor dispatches `request_permission` → `status == AwaitingPermission`. |
| `status_idle_on_stop` | Integration | `session/prompt` response (`end_turn`) → `status == Idle`. |
| `headerline_shows_status_text` | Integration | Status is `Thinking` → headerline contains `"Thinking"`. |

---

## Slice AUX-4 — Queue status ✅

**Design ref:** §4.4 (queue architecture, `QueuedPrompt`, drain).

**Impact surface:**

| File | Change |
|---|---|
| `crates/lattice-ai/src/acp/handle.rs` | Add `queue_len: Arc<AtomicUsize>` + `queue_len()` to `AiClientHandle`. Add `queue_len: usize` to `AiState`. |
| `crates/lattice-ai/src/acp/supervisor.rs` | Add `VecDeque` queue, `prompt_in_flight` flag, and `prompt_done_tx/rx` channel to supervisor loop. `AiCmd::Prompt` queues when in-flight, sends immediately otherwise. Completion signals drain the next queued prompt. Queue cleared on `Stop`/`ChildExited`/`Start`. |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | Extend `ConversationHeaderline` with `queue_len: Arc<AtomicUsize>`. `render()` appends `"⌛ N queued"` when > 0. |

**Queue cap:** Safety limit of 64 entries on the queue (bounded `mpsc::channel(64)`).

**Tests:**

| Test | Type | What it asserts |
|---|---|---|
| `prompt_queued_when_running` | Integration | Send `prompt()` while prompt active → returns immediately, queue_len becomes 1. |
| `queue_drained_on_completion` | Integration | First prompt completes → queued prompt auto-sent. |
| `queue_len_decremented` | Integration | After draining one queued prompt → queue_len decrements. |
| `queue_full_rejected` | Unit | Over 64 queued prompts → next `prompt()` returns `Err(QueueFull)`. |
| `queue_lost_on_disconnect` | Integration | Agent disconnects → queued prompts get `Err(Disconnected)`. |
| `headerline_shows_queue_count` | Integration | queue_len=2 → headerline contains `"⌛ 2 queued"`. |
| `headerline_hides_when_empty` | Integration | queue_len=0 → headerline omits queue segment. |

---

## Dependencies between slices

```
AUX-0 (docs)
  │
  ├── AUX-1 (permissions)
  │     └── AUX-3* (AwaitingPermission status — consumes
  │          the AskUser branch added by AUX-1)
  │
  ├── AUX-2 (usage)        — independent of AUX-1, AUX-3, AUX-4
  │
  ├── AUX-3 (status)       — independent of AUX-1, AUX-2, AUX-4
  │
  └── AUX-4 (queue)        — independent of AUX-1, AUX-2, AUX-3
```

AUX-2, AUX-3, and AUX-4 are independent and can land in any order or in
parallel. AUX-1 is independent of them but AUX-3's `AwaitingPermission`
status value depends on AUX-1's `classify_permission` → `AskUser` branch
(noted as AUX-3*). AUX-3 can ship the `Thinking`/`Executing`/`Idle` states
without AUX-1; `AwaitingPermission` is additive once AUX-1 lands.

---

## Execution order (recommended)

1. **AUX-2 (usage)** — smallest surface, pure data model + headerline. Quick
   win, familiarizes the team with the `Conversation::apply` path and
   `HeaderlineProvider`.
2. **AUX-3 (status)** — medium surface. Establishes the
   supervisor-publishes-status pattern and headerline integration.
3. **AUX-4 (queue)** — medium surface. Queue is pure client-side, no ACP wire
   changes. Dependent only on `AiClientHandle` and driver task.
4. **AUX-1 (permissions)** — largest surface (new block variant, oneshot
   plumbing, keymap, supervisor branch). Land last for maximum context on the
   patterns established by earlier slices.

Or in parallel: AUX-2 ‖ AUX-3 ‖ AUX-4 for independent teams; AUX-1 follows.
