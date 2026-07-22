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

## Slice AUX-1 — Permission surfacing for non-edit operations ✅ (closed by PU-B)

**Status:** CLOSED by PU-B (2026-07-12). AUX-1 landed the model + supervisor
branch + oneshot + inline `render_conversation` arm, but its allow/deny handlers
were bound to nothing (inert text). PU-B replaced that with the `*ai-permission*`
popup menu: PU-B.2a deleted the dead scaffold + moved resolution to the agent's
`option_id`; PU-B.2b built the menu (`ai-permission-mode`, `:ai-permission`,
`<CR>`-by-cursor); PU-B.3 auto-opens it keystroke-free on a pending request +
queues concurrent requests. The inline block remains as a non-interactive record.
See the PU-B sub-slices below.

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

---

## Post-landing fixes

Three defects surfaced on first real use of the AUX slices. All three were
integration-seam bugs: every unit test passed, because none of the seams was
covered end to end.

### F.1 — Sticky headerline deleted the prompt line (TUI only) ✅

AUX-2 registered the first sticky headerline on a buffer whose cursor lives on
the **last** line, which exposed a latent double-reservation. The host reserves
the sticky row once, in the scroll budget (`Editor::ensure_cursor_visible`'s
`effective_height`); `compose_pane_lines` then reserved it *again* by truncating
the visible-line window (`visible.truncate(len - sticky_count)`). The fill loop
below the sticky pre-pass already caps at `height`, so the truncation only ever
deleted a real document line — always the prompt.

The buffer model was untouched, so `<CR>` still submitted while the prompt row
painted as an empty `~` marker with the caret parked on it.

Fixed by removing the truncation in `crates/lattice-ui-tui/src/render.rs`.
GPUI never had the bug (it pushes sticky rows, then fills to `viewport_height`,
dropping only genuine overflow), so no peer change was needed — parity verified,
not assumed. Guarded by
`render::tests::sticky_headerline_does_not_clip_the_last_document_line`.

### F.2 — Headerline never repainted, so tokens/cost never appeared ✅

The full data path was correct: opencode emits `usage_update`, it deserializes,
and it reaches `ConversationStore` (all three now covered — see
`usage_update_from_opencode_wire_json` and the live e2e
`opencode_supervisor_end_to_end`). The drain, however, published its repaint
wake (`ConversationProjected`) only when the transcript **text** changed.

Status, usage, cost and queue length live *only* in the headerline and never in
the transcript, so a `usage_update` re-projected to byte-identical text, bumped
`headerline_version`, and woke nobody. `ConversationProjected` is the only event
wired to `virtual_rows_wake` (`install.rs`), so the headerline froze at its last
text-driven repaint — permanently showing `Ready`.

Fixed by publishing `ConversationProjected` unconditionally after each
re-projection. The invariant is pinned by
`usage_only_update_moves_headerline_but_not_transcript`.

### F.3 — Caret did not follow the prompt during streaming ✅

The prompt-focus tick callback re-parked the caret only on `did_clear` (a new
*user* turn), so it fired after a send but never while streamed agent output
grew the transcript above the prompt and pushed it down. The host clamps the
caret across an owner-write edit but does not move it, leaving the caret
stranded in the read-only transcript.

Fixed by re-parking whenever the prompt line moved, gated on modal state:
`should_repark_caret(did_clear, text_changed, in_insert)`. The mode now tracks
Insert by subscribing to `Event::ModalModeChanged`. In Normal the user is
reading or navigating and the cursor stays put.

**Known limitation (accepted):** the tick callback cannot tell whether the
conversation buffer is still active, so if the user switches to another buffer
*and* enters Insert there while the agent streams, the re-park would move that
buffer's cursor. Switching buffers is a Normal-mode action, so the Insert gate
suppresses the common case. A proper fix needs an active-buffer signal the host
does not currently expose to modes.

### F.4 — Headerline provider leaked across re-activation ✅

`CONV_HEADERLINE_PROVIDER_ID` is a fixed tag, `register()` refuses to replace a
live id, and the return value was discarded — so a re-opened `:opencode` kept
the stale provider. `AiConversationGuard` now owns the registration and
unregisters on drop (mode owns its full surface), and re-registration clears any
stale entry first.

### F.5 — Caret does not ride streamed content ✅

F.3 fixed the symptom with a modal-state flag and a tick callback. That is a
mechanism bolted beside the content, and it was superseded by slice OWC below,
which deletes it. (Both F.5 and OWC shipped together.)

### Testing gap this exposed

Every one of F.1–F.4 lives at a seam between two components that were each
individually well tested. Specifically: no TUI test had ever registered a
virtual-row provider, and no test drove `usage_update` from wire JSON through to
a rendered headerline. Unit coverage of both halves of a seam is not coverage of
the seam.

---

## Follow-on slices

Design refs: [owner-write-caret.md](../../architecture/owner-write-caret.md),
[popup-api.md](../../architecture/popup-api.md),
[acp-ux-enhancements.md](../../architecture/acp-ux-enhancements.md) §5.3–§5.4.

Sequenced so each lands green and independently useful. OWC is first because it
fixes what is actively wrong; PU-A is the largest and riskiest; TCF depends on
nothing and can be pulled forward if PU-A stalls.

### Slice OWC — Owner-write caret survival ✅

**Design ref:** owner-write-caret.md.
**Depends on:** nothing. **Deletes:** F.3's mechanism.

| File | Change |
|---|---|
| `crates/lattice-core/src/document.rs` | `apply_edit_batch` transforms `self.selections` across each `AppliedEdit`. |
| `crates/lattice-core/src/{buffer,position}.rs` | `transform_position(Position, &AppliedEdit) -> Position` (§4.1 rule), pure + total. |
| `crates/lattice-host/src/dispatch.rs` | Write the caret through to the active document's selections at end of input dispatch (one site). Adopt the document's primary head on an owner-write version bump (per-buffer `last_seen_text_version`). |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | Pure deletion: `should_repark_caret`, `in_insert`, the `ModalModeChanged` subscription, `_modal_subscription`, the prompt-focus tick callback and `drain_flag`. `reproject` is already minimal (see owner-write-caret.md §5) and is not touched. |

Tests: owner-write-caret.md §8. The `apply_targeted_edit`-never-moves-the-cursor
and keystroke-no-double-move regressions are the ones that catch a wrong design.

**Risk:** touches `lattice-core`'s edit path, which everything sits on. Mitigated
by the transform being pure and the host adopting only for edits it did not issue.

### Slice PU-A — Generic popup primitive ✅

**Design ref:** popup-api.md. **Depends on:** nothing. **Ships no user feature.**
**Sub-sliced in:** [pu-a-generic-popup.md](archive/pu-a-generic-popup.md) (PU-A.1a ✅;
1b/2/3/4 planned). The pre-PU-A input/caret defects landed separately in
[popup-input-caret.md](archive/popup-input-caret.md).

| File | Change |
|---|---|
| `crates/lattice-core/src/ui/popup.rs` | Add `PopupFocus { Steal, Passive }`. |
| `crates/lattice-host/src/dispatch.rs` | `open_popup_buffer(BufferId, PopupPlacement, PopupFocus)`. Rework `open_popup` / `open_floating_popup` into callers. Fix State-A dismissal restoring `BufferKind::Document` (popup-api.md §5). |
| `crates/lattice-host/src/state.rs` | `PrevPaneState` gains `modal: ModalState`; `dismiss_popup` restores it. `PopupFocus::Steal` sets `ModalState::Normal` on open (without it the popup's bindings never fire — Insert eats the keystroke). Rename `prev_pane_for_help` → `prev_pane_for_popup`. |
| `crates/lattice-host/src/popup.rs`, `crates/lattice-help/` | Move `PopupSnapshot` + `HelpMetadata` + `popup_back_stack` into `lattice-help`. |
| `crates/lattice-grammar/src/effect.rs` | Add `Effect::OpenPopup { buffer, placement, focus }`. Rename `CloseHover` → `DismissPopup`. |
| `crates/lattice-ui-tui/src/{render.rs,app/dispatch.rs}` | Title from buffer name, not `help.title`. New effect arm. |
| `crates/lattice-ui-gpui/src/{lib.rs,window.rs}` | Same, in lockstep (exhaustive match ⇒ compile error if missed). |
| `wit/types.wit`, `crates/lattice-plugin-host/src/boundary_effect.rs` | `effect` variant + `to_wit` / `from_wit` arms. |

Tests: popup-api.md §7. The Help regressions (`:help`, `q`/`<Esc>`, `<C-o>`
back-stack, floating hover) are the acceptance gate — this slice must be
invisible to the user.

**Risk:** highest of the four. It refactors a working, widely-used surface and
ships nothing new. Land it green and separately so a regression bisects cleanly.

### Slice PU-B — ACP permission menu ✅ (2b-iv digit accelerators deferred)

**Design ref:** acp-ux-enhancements.md §5.3, popup-api.md §4. **Depends on:** PU-A.
**Closes:** AUX-1 ✅. Decomposed into three landable-green sub-slices
(2026-07-12) — the doc framed it as one, but it touches the WIT contract, the
supervisor oneshot contract, and a new mode, so it slices like PU-A did for
clean bisection.

**Confirmed decisions (Dhruva, 2026-07-12):**
- **Resolve by agent `option_id`, not the 4-way `PermissionOutcome` bucket.** The
  menu presents the agent's actual option list (any arity, wire order) and
  resolves by `option_id`; the supervisor's `pick_option_by_kind` reverse-map is
  dropped. Respects the ACP protocol's dynamic options (heuristic #1: best
  long-term fit; correct for agents offering ≠4 options).
- **Digit accelerators bound `1`–`9` statically** in the permission Major-mode
  keymap (scoped to the `*ai-permission*` buffer), plus `<CR>`-by-cursor always
  available (the file-tree/oil `entry_at_line` model) for completeness and >9
  options. No dynamic per-request rebinding — no such template exists
  (`completion_popup_layer_bindings` binds a *fixed* set, not `1..=N`).
- **Opening mechanism:** buffer creation goes through the **declarative**
  creation seam — a drain / **tick callback** (PU-B's is the first `lattice-ai`
  tick-callback consumer) returns `Effect::OpenSyntheticBuffer { name, mode_id }`
  (the host applies it via the mode-owned create path), then projects the
  request and returns `Effect::OpenPopup { buffer, Centered, Steal }`.
  (A tick callback returns `Vec<Effect>` and has no `&mut Editor`, so it uses
  the effect front-end, not the imperative `ModeActivator::ensure_named_document`
  seam — see `design.md` §5.10.5.) `OpenPopup` keeps its `{ buffer: BufferId, .. }`
  shape.

#### PU-B.1 — `Effect::OpenPopup` plumbing ✅
The deferred PU-A.3b, landed now that PU-B.2/.3 are its consumers (no longer dead
surface). `Effect::OpenPopup { buffer: BufferId, placement: PopupPlacement, focus:
PopupFocus }` in `lattice-grammar`; both renderer effect arms call the existing
`Editor::open_popup_buffer` primitive (PU-A.1b-ii); WIT `types.wit` gains
`popup-placement`/`popup-focus` enums + `open-popup(open-popup-payload)` (BufferId
↔ u32, the `apply-edit-payload` precedent); `boundary_effect.rs` to_wit/from_wit +
round-trip test; added to the four non-mutating classifier lists (2 host + 2 TUI).
No host apply arm needed — `apply_effect_host`'s catch-all pushes it to
`out.effects` for the renderer. Host-tested (register buffer → apply effect →
assert popup state).

#### PU-B.2a — resolution model + AUX-1 scaffold removal ✅
The cleanly-separable, fully-invisible half of B.2 (no menu yet):
| File | Change |
|---|---|
| `crates/lattice-ai/src/acp/conversation.rs` | Oneshot payload + `resolve_permission(id, option_id: PermissionOptionId)` carry the agent's chosen option; block status derives from that option's `kind` via a new `Conversation::permission_option_kind` (fail-closed → Denied on unknown/`#[non_exhaustive]` kind). `PermissionOutcome` enum removed. |
| `crates/lattice-ai/src/acp/supervisor.rs` | `AskUser` branch forwards `Selected(option_id)` directly on `rx.await`; `pick_option_by_kind` deleted (orphaned). |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | Deleted the unreachable `action:ai-conv-allow`/`-deny` handlers + `permission_handler` + specs + `current_permission_id` (field/Default/drain) + the stale `(a)/(A)/(r)/(R)` hints. |
| `crates/lattice-ai/src/acp/mod.rs` | Drop the `PermissionOutcome` re-export. |

Green: `lattice-ai` 156 passed. `resolve_permission` now awaits its consumer (the mode).

#### PU-B.2b — `ai-permission-mode` menu ✅ (digit accelerators → PU-B.2b-iv, deferred)
| File | Change |
|---|---|
| `crates/lattice-ai/src/acp/permission_mode.rs` (new) | `ai-permission-mode`: `on_activate` projects the oldest pending request into `*ai-permission*`; binds `1`–`9` + `<CR>`-by-cursor + `Esc`/`q` dismiss; handler resolves via `store.resolve_permission(id, option_id)`. |
| `crates/lattice-ai/src/acp/commands.rs` | `:ai-permission` opens the menu (returns `Effect::OpenPopup`). |
| `crates/lattice-ai/src/acp/install.rs` | Register the new mode. |

Sequenced into steps (all decisions confirmed 2026-07-12):

**PU-B.2b-i — content-agnostic popup render — PARTIALLY SUPERSEDED by
`popup_focused` migration (2026-07-22).** Investigation at the time revealed
`BufferKind::Help` is not just a *render* gate but the whole popup *focus + input*
model: `active_buffer_id()` special-cased `Help → popup_buffer`,
keymap selection reads `active_buffer_id()`, so a `Document`-kind popup
would never receive its mode's keys — the coupling spanned ~12 `dispatch.rs` sites +
both renderers. The `popup_focused` field decouples the focus-detection axis:
State-B now reads `editor.popup_focused` / `ad().popup_focused` instead of
`active_buffer == Help`. `active_text()` / `active_cursor()` use the generic
`popup_buffer_content()` instead of the kind-gated `popup_help()`. Content identity
(`pane_tree.active().buffer == Help`) remains untouched — those are genuine kind
checks, not focus-state leaks. `BufferKind::Help` remains "the popup-surface kind"
for content identity; the `popup_focused` migration removed the conflation without
generalising the buffer kind itself. A future slice may still rename `Help → Popup`.

**PU-B.2b-ii — name-based `Effect::OpenPopup` + popup-buffer primitive ✅.** Reshaped
`Effect::OpenPopup { buffer: BufferId }` → `{ name, mode_id, placement, focus }`
(data-only; emitters have no services to supply an id — keeps the effect vocabulary
the host boundary, like `OpenSyntheticBuffer`). New `Editor::open_popup_named` ensures
an idempotent-by-name popup buffer via `ensure_named_popup_buffer` (a new
`SyntheticDocVariant::Help` → `BufferData::Help`, so the popup renderer draws it, with
`mode_id` as major so its keymap owns the buffer), then `open_popup_buffer`. Dismiss
tears the buffer down (`dismiss_stale_popup_registry`), so each open is fresh and the
mode re-projects on activation. Updated `effect.rs`, `wit/types.wit`, `boundary_effect.rs`
+ round-trip, both renderer arms (`open_popup_named`). Tests: `open_popup_named_*` +
routing; grammar 212, host lib + boundary 13 + GPUI green.

**PU-B.2b-iii — the mode ✅.** `permission_mode.rs` (`ai-permission-mode`, Major):
`on_activate` reads `ConversationStore::oldest_pending_permission` (new; turns/blocks
are wire-ordered) and owner-writes the projection (title, numbered options, optional
description after the options so the cursor offset stays fixed) into `*ai-permission*`;
keymap binds `<CR>` → select-at-cursor (file-tree/oil `entry_at_line` model),
`Esc`/`q` → `Effect::DismissPopup`; the select handler maps `cursor.line -
FIRST_OPTION_LINE` → `PermissionOption` and calls `store.resolve_permission(id,
option_id)` (PU-B.2a). `:ai-permission` (commands.rs) returns `Effect::OpenPopup {
name, mode_id, Centered, Steal }`. Mode + actions registered in `install.rs`; the
stale `:ai-allow`/`:ai-deny` keymap comment removed. Tests: projection↔cursor-offset
contract, oldest-pending ordering; `lattice-ai` 159 green, full TUI builds.

#### PU-B.2b-iv — `1`–`9` digit accelerators 📝 (deferred; detailed spec)

**Goal.** Let the user press `1`/`2`/`3`… to select option N directly (the universal
numbered-menu convention — Claude Code's own prompt, `fzf`, `gh`), instead of only
`<CR>`-by-cursor. Polish, not a blocker: `<CR>`-by-cursor is fully functional.

**Root cause (verified — the blocker).** In lattice a leading digit **is a vim
count**. `crates/lattice-host/src/input.rs:662` (`compute_normal_action`) hoists a
bare `1`–`9` to `Action::PushDigit` **before** the keymap trie is consulted:
```rust
let prefix_resolves_chord = matches!(prefix_action, Some(ref a) if !matches!(a, Action::None));
if !prefix_resolves_chord
    && let KeyKind::Char(c) = chord.key
    && let Some(digit) = c.to_digit(10)
    && (digit > 0 || pending_count > 0)
{ return Action::PushDigit(digit as u8); }        // ← digit never reaches lookup_normal
```
So a mode that binds `"1"` never sees it (confirmed: no mode binds a bare digit).
The hoist already has ONE escape hatch — `!prefix_resolves_chord` (an `<C-x>2`-style
chord's literal second key wins). The fix adds a second, per-buffer, condition.

**Approach — a per-buffer `CountPrefix` option (property, not `BufferKind` — the
standing rule; note `TranslateContext` already carries `active_buffer: BufferKind`
which must NOT be keyed on).** Default `true` (vim everywhere); a numbered-menu
buffer resolves it `false`, which skips the hoist so bare digits fall through to
`lookup_normal`, where the mode's `"1"`–`"9"` MajorMode bindings resolve. Reusable by
any future in-buffer chooser (quickfix, `:ls`-picker) — the general primitive, not a
permission-menu special case.

| File | Change |
|---|---|
| `crates/lattice-config/src/core_options.rs` (~`:249`, next to `NoFile`) | Add `pub CountPrefix: bool = true;` (mode-driven like `ReadOnly`/`NoFile`, not user-customizable). |
| `crates/lattice-config/src/lib.rs` (`:130`) | Export `CountPrefix` in the options re-export list. |
| `crates/lattice-ai/src/acp/permission_mode.rs` (`options()`) | Add `lattice_config::CountPrefix = false` alongside `ReadOnly`/`NoFile`. |
| `crates/lattice-host/src/input.rs` | `TranslateContext` (`:23`) gains `pub count_prefix: bool`; its build site resolves `CountPrefix` for the active buffer (`resolved_options.get_typed::<CountPrefix>()`); `translate_normal` (`:589`) + `compute_normal_action` (`:619`) take a `count_prefix: bool` param; the hoist condition (`:662`) gains `&& count_prefix`; call site (`:411`) passes `ctx.count_prefix`. |
| `crates/lattice-ai/src/acp/permission_mode.rs` (keymap + handlers) | Bind `"1"`–`"9"` (Normal) → `action:ai-perm-1 … -9`; register the 9 actions; 9 handlers = `select_index_handler(menu, i)` reusing the resolve-and-dismiss path factored out of `select_at_cursor_handler`. Cap at 9; >9 options fall back to `<CR>`. |

**Tests.**
- `permission_menu_digit_selects_option` (lattice-ai / host integration): `2` in the
  menu → resolves `options[1].option_id` + dismisses.
- `count_prefix_off_lets_digit_reach_keymap` (host unit on `compute_normal_action`):
  with `count_prefix=false` a bare `2` is NOT `PushDigit`; with `true` it is.
- **Regression (load-bearing — input hot path):** `counts_still_work_in_document`
  — `3j`, `2dw`, `5gg` in a normal buffer (default `count_prefix=true`) behave
  exactly as today. Reuse the existing count tests; add one asserting the default.
- `0` unaffected: with `count_prefix=false` and no in-progress count, `0` still
  falls through (mode doesn't bind it → harmless line-start/no-op in a read-only menu).

**Heuristic mapping.** UX: pure win (adds the accelerator; `<CR>` unchanged; other
buffers untouched since default-on). Paramount #3 (vim grammar): makes "digits are
counts" an opt-out *option*, not a hard-wired branch — grammar intact. Heuristic #1:
`CountPrefix` is the genuinely-general primitive; special-casing the permission buffer
in `input.rs` would be the forbidden kind-branch. Standing rule (property not kind):
satisfied — `input.rs` learns nothing about permission menus.

**Cost.** Moderate: touches `lattice-config` + the input hot path (`translate_normal`
runs per keystroke) + the mode. The regression test is mandatory before landing.

**Honest gap:** no end-to-end test that a live `<CR>` in the popup resolves through
the host (the `za`-through-host class of gap TCF also noted). Covered by construction:
`open_popup_named` makes it a `Help`-kind Steal popup so `active_buffer_id()` → the
popup buffer → its `ai-permission-mode` keymap fires; the handler + resolution are
unit-tested.

#### PU-B.3 — auto-open + queue ✅
Full-queue auto-open (confirmed). PU-B's is the first `lattice-ai` tick-callback
consumer.
- **Coordinator** (`PermissionMenuCoordinator`, a service): `menu_open`
  (AtomicBool) gates the queue — the tick callback opens only when no menu shows;
  `deferred` (HashSet) holds `Esc`-deferred ids the callback skips.
- **Auto-open tick callback** (registered at boot via `boot.tick_callback`, body
  `permission_mode::auto_open_tick`): when `!menu_open` and an oldest
  non-deferred `Pending` request exists → set `menu_open` optimistically (emit
  once, not every tick) and return `Effect::OpenPopup { name, mode_id, Centered,
  Steal }`.
- **Keystroke-free**: `run_tick_pending` runs on the actor's `async_landed`
  `select!` arm (`editor_actor.rs:616`), NOT only on keypress. Added
  `boot.wake_on_event::<ConversationUpdated>()` so a pending permission (which
  `push_permission_request` publishes) wakes the actor even when `*ai:opencode*`
  isn't focused — the menu opens on its own.
- **Queue advance**: the mode's guard `Drop` (dismiss tears down the buffer →
  removes the active mode → drops the guard) clears `menu_open`, so the next
  pending request opens on close (the `open_next_queued_show_message_request`
  precedent). `Esc` defers via the dismiss handler (adds the id to `deferred`);
  the inline block keeps rendering it and `:ai-permission` reopens it.
- **Reopen-safety**: `Editor::open_popup_named` dismisses any open popup first,
  so the idempotent ensure can't hand back a just-removed buffer.

Tests: `auto_open_*` (emit-once/gate, skip-deferred, queue-advance, no-op).
`lattice-ai` 163, host lib 677 green. **Closes AUX-1.**

### Slice TCF — Tool-call folds ✅

**Design ref:** acp-ux-enhancements.md §5.4. **Depends on:** nothing.

| File | Change | Status |
|---|---|---|
| `crates/lattice-ai/src/acp/conversation.rs` | `Block::ToolCall` gains `kind`, `input`, `output` (pretty-printed raw JSON, stored as `String` so `Block` stays `Eq`). `update_tool_status` → `merge_tool_update` folding every `Some` field from `ToolCallUpdateFields`. | ✅ |
| `crates/lattice-ai/src/acp/conversation_mode.rs` | `render_conversation` becomes a thin wrapper over a single-pass `project_conversation(conv) -> (String, Vec<ConversationFold>)`, so fold line ranges can never drift from the rendered text. A detailed tool call emits indented `input:`/`output:` rows under its `▸ summary [status]` head; the fold spans the head through the last detail row. | ✅ |
| `crates/lattice-ai/src/acp/tool_fold.rs` (new) | `ToolCallFoldSource` + `ReasoningFoldSource` (`FoldSource`), `closed: true`, distinct `ProviderId` namespaces per buffer. `identity = hash("ai:tool", tool_call_id)` / `hash("ai:reasoning", ordinal)` — carries expansion state across a transcript growing above the call. Registered/unregistered via `FoldOverlayServiceHandle` in `AiConversationMode::on_activate` / guard `Drop`, mirroring `DiffMode`. | ✅ |
| `crates/lattice-ai/src/acp/conversation.rs` | Reasoning blocks are the second fold consumer (`ReasoningFoldSource`); the `Block::Reasoning` doc comment is now accurate. | ✅ |

**Landed decisions:**

- **Two fold sources, not one** (Dhruva, 2026-07-11). Tool-call and reasoning
  folds are the same closed-by-default, identity-keyed behaviour, but ship as
  two independently-registered `FoldSource`s (the `HunkFoldSource` /
  `UnchangedFoldSource` precedent) so each deregisters on its own id. Both read
  the shared `project_conversation` layout and filter to their `ConversationFold`
  kind.
- **No host wiring.** `maybe_reparse_syntax` already calls `recompute_folds()`
  on every text-version bump, so the drain's owner-write re-projection triggers
  a fold recompute automatically. TCF adds only the two overlay registrations.
- **No renderer / GPUI peer change.** Folds are already a cross-renderer
  primitive (`lattice_core::Fold`); no new `Effect`/`DiffSignKind` variant, so
  the TUI and GPUI peers consume the folds identically with no lockstep edit.
  Closed folds elide detail rows at the cell layer (paramount #1). No new bench:
  fold recompute rides the existing `fold_recompute` bench path.

**Test coverage.** The novel logic — the `project_conversation` layout, fold
ranges, identities, the streaming-stability property (text growing above a call
leaves its identity unchanged), and detail rendering — is covered by pure unit
tests in `conversation_mode.rs` + `tool_fold.rs`. The generic
`FoldSource → recompute_folds → editor.folds` seam is already pinned host-side
by `recompute_folds_for_inactive_baseline_stashes_unchanged_folds` (any
`FoldSource` flows identically through the adapter). The one seam a new test
does **not** cover is `on_activate` actually registering the sources through the
service — verified by construction (the `FoldOverlayServiceHandle` lookup type
matches the boot registration, avoiding the ServiceRegistry TypeId pitfall) and
by mirroring `DiffMode::on_activate` exactly. A `za`-through-host integration
test over the `*ai:opencode*` buffer is the honest remaining gap.

Ingest capture (`Block::ToolCall` fields + `merge_tool_update`) landed first —
it stopped discarding wire data before folds rendered.
