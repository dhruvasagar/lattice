# Agent conversation UI — slice plan

Implements `docs/dev/architecture/agent-ui.md`. Design owns the *what/why*; this
file owns the *when/order*. Every slice lands green (build + test + fmt + clippy)
and is independently reviewable.

**Feature-gating:** everything here lives behind `feature = "acp"` in `lattice-ai`
(the conversation UI is ACP-only). The transport-neutral `:ai-log` /`AiLogger`
substrate is untouched except that conversation sources stop flowing into it
(AU-1).

## Roadmap

| Slice | Deliverable | Status |
|-------|-------------|--------|
| **AU‑1** | `Conversation` model + supervisor mapping + `ConversationUpdated`; conversation sources leave `AiLogger` (completes the conversation/trace split) | ✅ done |
| **AU‑2** | `ai-conversation` major mode + read-only projection (turn headers, inline tool-call status; decoration-based in-place status + reasoning fold deferred); `:opencode` opens `*ai:opencode*` via generic `Effect::OpenSyntheticBuffer`; both renderers | ✅ done |
| **AU‑3** | Modal input: editable prompt tail, Insert-relocates-to-prompt, Enter sends, `Ctrl-C` interrupt (`AiCmd::Interrupt`) | 📝 planned |
| **AU‑4** | Diff review + approval: `request_permission` → `review_diff` → verdict → response; `[diff]` edit blocks; reads auto-run | 📝 planned |
| **AU‑5** | Trust-mode toggle: per-session `auto_accept` + chord | 📝 planned |

---

## AU‑1 — Conversation model + supervisor mapping

**Goal.** A structured, off-thread `Conversation` store the supervisor feeds from
ACP `SessionUpdate`s, publishing `ConversationUpdated`. Conversation sources
(`AgentText` / `Reasoning` / `ToolCall`) stop flowing into `AiLogger`; trace
sources (`Client` / `Lifecycle`) keep flowing there.

**Files.**
- Create `crates/lattice-ai/src/acp/conversation.rs` — the model + store + event.
- Modify `crates/lattice-ai/src/acp/supervisor.rs` — feed the store; narrow
  `agent_log_entry` to trace-only sources.
- Modify `crates/lattice-ai/src/acp/mod.rs` — `pub mod conversation;` + re-exports.
- Modify `crates/lattice-ai/src/acp/handle.rs` — expose a `Conversation` snapshot
  handle alongside `AiState` (so the mode can read it).

**Interfaces (produced).**
- `Conversation { turns: Vec<Turn> }`, `Turn { role: Role, blocks: Vec<Block> }`.
- `enum Role { User, Assistant }`.
- `enum Block { Text(String), Reasoning(String), ToolCall { id: String, name: String, status: ToolStatus, detail: String }, Edit { path: PathBuf, status: EditStatus } }`
  (`Edit` carries no diff-session ref yet — AU‑4 adds it).
- `enum ToolStatus { Pending, Running, Ok, Err }`, `enum EditStatus { Proposed, Accepted, Rejected }`.
- `struct ConversationStore` behind `Arc<ArcSwap<Conversation>>`; `apply(&SessionUpdate)` mutates + publishes.
- `struct ConversationUpdated { session: SessionKey }` — typed bus event via
  `lattice_protocol::register_event!` (mirrors `AiLogPushed`).
- `fn conversation_entry(&SessionUpdate) -> Option<ConvMutation>` — pure, unit-testable,
  mirrors `agent_log_entry`'s shape; `#[non_exhaustive]` catch-all.

**Steps.**
- [ ] Write failing test: `apply(AgentMessageChunk("hi"))` then `apply(AgentMessageChunk(" there"))` yields one Assistant turn with a single `Text("hi there")` block (streaming extends last block).
- [ ] Write failing test: `apply(ToolCall{id,name})` pushes a `ToolCall{status:Running}` block; `apply(ToolCallUpdate{id, status:Ok})` updates *that* block in place (matched by id), not a new block.
- [ ] Write failing test: `apply(AgentThoughtChunk("thinking"))` extends the last `Reasoning` block.
- [ ] Implement `Conversation`/`Turn`/`Block`/enums + `ConversationStore::apply` + `conversation_entry`.
- [ ] Register `ConversationUpdated` via `register_event!`; publish in `apply`.
- [ ] Modify `drain_notifications`: route conversation sources into the store, trace sources into `AiLogger`; narrow `agent_log_entry` to `Client`/`Lifecycle` only (or a new `trace_log_entry`).
- [ ] Update the existing supervisor tests that asserted `AgentText` records land in `AiLogger` — they now assert the `Conversation` store instead (behavior moved, not lost).
- [ ] `cargo test -p lattice-ai --features acp`; fmt; clippy; commit.

**Exit criteria.** Conversation-source `SessionUpdate`s land in the store (asserted
via snapshot); trace sources still land in `AiLogger`; `ConversationUpdated` fires;
all four feature combos still build; existing `:ai-log` trace tests pass.

---

## AU‑2 — `ai-conversation` mode + read-only projection

**Goal.** An `ai-conversation` major mode that projects the `Conversation` store
into a read-only `*ai:opencode*` buffer, mirroring `AiLogMode`'s
subscribe-and-project. `:opencode` starts the agent AND opens the buffer.

**Files.**
- Create `crates/lattice-ai/src/acp/conversation_mode.rs` — the mode + drain task
  + projection (text + decorations).
- Modify `crates/lattice-ai/src/acp/mod.rs` — `pub mod conversation_mode;`.
- Modify `crates/lattice-ai/src/acp/install.rs` — register the mode; the
  `:opencode` handler opens the conversation buffer after `start`.
- Modify `crates/lattice-ai/src/acp/commands.rs` — `:opencode` emits an
  effect that opens `*ai:opencode*` (buffer-open is a generic host primitive;
  the command owns the binding).
- Reference: `crates/lattice-agent/src/log/modes.rs` (`AiLogMode` pattern),
  `crates/lattice-host/src/modes.rs` (`DocumentFolds`, buffer-locals).

**Interfaces (produced).**
- `struct AiConversationMode;` with `mode_id()`; `impl Mode`.
- `fn conversation_buffer_name(&SessionKey) -> String` → `*ai:opencode*` (single
  session in v1; provider-qualified name slot).
- Projection: `fn render_conversation(&Conversation) -> (Rope, Vec<Decoration>, Vec<Fold>)`
  — pure, unit-testable (text + tool-call-status decorations + reasoning folds).

**Steps.**
- [ ] Write failing test: `render_conversation` of a 2-turn conversation produces
  expected text with a `you`/`opencode` virtual-row header per turn and a
  tool-call status glyph.
- [ ] Write failing test: a `Running`→`Ok` `ToolCall` status change re-projects to
  a `✓` decoration on the *same* content line (no content-line rewrite).
- [ ] Implement `render_conversation` (text + decorations + folds).
- [ ] Implement `AiConversationMode::on_activate`: subscribe to
  `ConversationUpdated`, spawn a drain task that re-projects via
  `apply_edit_batch_blocking` (debounced, off-thread), collapse reasoning folds by default.
- [ ] Wire `:opencode` to start + open `*ai:opencode*`.
- [ ] TUI + GPUI: confirm the conversation renders through the Document path;
  add any new decoration kind to BOTH renderers in this slice.
- [ ] Host test: booting + `:opencode` opens a listed `*ai:opencode*` buffer that
  passes the `multibuffer_is_a_regular_buffer.rs` spirit (motions/scroll/`:ls`).
- [ ] `cargo test` (lattice-ai acp + host + tui + gpui); fmt; clippy; commit.

**Exit criteria.** `:opencode` opens a live-streaming read-only `*ai:opencode*`
buffer; tool-call status updates in place; reasoning folds collapsed; both
renderers draw it; no UI-thread projection work; grep shows no `BufferKind`
branch added.

---

## AU‑3 — Modal input (the terminal REPL)

**Goal.** An editable prompt region at the buffer tail; Insert relocates the
cursor there; Enter sends; `Ctrl-C` interrupts the turn.

**Mode-ownership design (researched 2026-07-09).** Uses the `DiffMode` template
(`crates/lattice-diff/src/mode.rs`): the mode declares `Mode::keymap()` entries
and `Mode::action_handlers()` closures; the host walks every registered mode at
boot, so **zero `Editor::` methods and zero `Action` variants are added**. Split:

- **Generic host primitive (not provider-specific):** a buffer-local
  `EditableRange(Range)` (mirrors `DocumentFolds` in `lattice-host/src/modes.rs`)
  + one exception in the read-only gate. Today `run_read_only_motion`
  (`dispatch.rs:30612`) rejects Insert/operators in a `ReadOnly` buffer with
  "buffer is read-only"; the exception: if the buffer carries an `EditableRange`
  and the edit/cursor is inside it, allow the edit. One cold-path
  `range.contains` check; no hot-path cost for normal buffers. Any buffer kind
  can carry it (the comint pattern) — future `*scratch*`/REPL buffers reuse it.
- **Mode-owned (`lattice-ai/acp`):**
  - `AiConversationMode::keymap()` → `KeymapEntry`s: `{mode: Normal, chord: "i"/"a"/"o"/"A"/"I"/"O", command: "action:ai-conv-focus-prompt"}`; `{mode: Insert, chord: "<CR>", command: "action:ai-conv-send"}`; `{mode: Insert, chord: "<C-c>", command: "action:ai-conv-interrupt"}`.
  - `AiConversationMode::action_handlers()` → three `ActionHandler` closures
    (`Fn(&ActionContext) -> Option<Effect>`), bodies in `lattice-ai/acp`:
    - `focus-prompt`: returns an `Effect` placing the cursor at the end of the
      prompt line and entering Insert (reuse an existing cursor-move + EnterMode
      effect; do NOT add a new `Action`).
    - `send`: reads the prompt text via
      `ctx.services.get::<BufferStoreHandle>()?.handle_for(ctx.buffer_id)` (the
      last line after the `> ` marker), calls `AiClientHandle::prompt(text)`
      (`ctx.services.get::<AiClientHandle>()`), returns a clear-edit `Effect`
      that resets the prompt region.
    - `interrupt`: `ctx.services.get::<AiClientHandle>()?.interrupt()`, returns
      `None`.
- **Projection change (`conversation_mode.rs`):** the buffer layout becomes
  `<conversation>\n> <prompt>`; the drain task re-projects ONLY the conversation
  zone (above the `> ` prompt line), preserving the user's in-progress prompt,
  and (re)sets the `EditableRange` buffer-local to the prompt tail after each
  re-projection (positions shift as the conversation grows).
- **Interrupt plumbing:** `AiCmd::Interrupt` + `AiClientHandle::interrupt`;
  supervisor sends ACP `session/cancel` for the active turn without ending the
  session.

`ActionContext` exposes `{buffer_id, cursor, services, events}` only (no direct
buffer text) — the `send` handler reaches text through `BufferStoreHandle`.

**Enforcement point (located 2026-07-09).** `ReadOnly` is NOT enforced in
`apply_edit_blocking` / `apply_edit_batch_blocking` (neither checks it), nor via
an `option_cache` field. It is enforced at the **keystroke router** (the
TUI/GPUI dispatch path documented at `lattice-ui-tui/src/app/dispatch.rs:91`,
forwarding to `Editor::run_read_only_motion`, `dispatch.rs:30612`): a
resolved-`ReadOnly` buffer routes motion/operator/insert invocations there, where
non-motion commands echo "buffer is read-only". The `EditableRange` exception
belongs in **that router's read-only branch** (or in `run_read_only_motion`
itself): if the active buffer carries an `EditableRange` and the invocation is an
insert/edit whose target is inside it, fall through to the normal document path
instead of rejecting. **Care:** this router is shared by `*messages*`, ai-log,
help, dashboard, and multibuffer — the exception must be strictly gated on the
`EditableRange` buffer-local's presence so those buffers are unaffected. Verify
with `multibuffer_is_a_regular_buffer.rs` + the messages/ai-log read-only tests
before landing. This is the open item to resolve first when implementing AU-3.

**Files.**
- Create/Modify `crates/lattice-host/src/modes.rs` + `dispatch.rs` — the generic
  `EditableRange` buffer-local + the `run_read_only_motion` exception.
- Modify `crates/lattice-ai/src/acp/conversation_mode.rs` — `keymap()` +
  `action_handlers()`; projection preserves the prompt region + sets the range.
- Modify `crates/lattice-ai/src/acp/handle.rs` + `supervisor.rs` — add
  `AiCmd::Interrupt` + `AiClientHandle::interrupt`; supervisor `session/cancel`.

**Interfaces (produced).**
- Buffer-local `EditableTail(pub Range<usize>)` (or a `prompt_region` accessor) +
  dispatcher check: Insert/operator edits outside the tail are rejected in an
  owner-written buffer.
- `AiCmd::Interrupt`; `AiClientHandle::interrupt(&self)`.
- Mode keymap: `i`/`a`/`o`/`A`/`I`/`O` → relocate-to-prompt-then-Insert; `<CR>` in
  Insert → send + clear region; `<C-c>` → interrupt.

**Steps.**
- [ ] Write failing test: entering Insert in `*ai:opencode*` moves the cursor into
  the prompt region regardless of prior Normal-mode cursor position.
- [ ] Write failing test: an Insert edit targeting a history line is rejected (no
  mutation); an edit in the prompt region lands.
- [ ] Write failing test: Enter in Insert sends the prompt text via a captured
  handle and clears the region.
- [ ] Implement the editable-tail buffer-local + dispatcher enforcement.
- [ ] Implement the mode keymap (relocate-on-insert, send-on-enter, interrupt).
- [ ] Implement `AiCmd::Interrupt` + supervisor `session/cancel`.
- [ ] TUI + GPUI: cursor placement + prompt region render in both.
- [ ] `cargo test`; fmt; clippy; commit.

**Exit criteria.** Normal-mode motions roam the whole buffer; Insert always lands
in the prompt; history is unmutable; Enter sends + user turn appears; `Ctrl-C`
interrupts without ending the session; both renderers.

---

## AU‑4 — Diff review + approval

**Goal.** Agent edits arrive as `session/request_permission`; in review mode the
supervisor opens a `review_diff` session and gates the permission response on the
verdict; `Edit` blocks show `[diff]`. Reads auto-run.

**Files.**
- Modify `crates/lattice-ai/src/acp/supervisor.rs` — handle `request_permission`:
  edits → `review_diff` (from `lattice-agent`) → await verdict → allow/deny;
  reads → auto-allow.
- Modify `crates/lattice-ai/src/acp/conversation.rs` — `Edit` block gains a
  `session: Option<DiffSessionRef>` + `status` transitions on verdict.
- Modify `crates/lattice-ai/src/acp/conversation_mode.rs` — `[diff]` decoration
  on `Edit` blocks; activating it opens the diff view (generic host effect).
- Reference: `crates/lattice-agent/src/diff_review.rs` (`review_diff`,
  `DiffReviewRequest`), the MCP `openDiff` path in `crates/lattice-ai/src/mcp/diff.rs`.

**Interfaces (produced/consumed).**
- Consumes `lattice_agent::review_diff` / `DiffReviewRequest` / `DiffOutcome`.
- `Edit { path, session: Option<DiffSessionRef>, status: EditStatus }`.
- Supervisor: `handle_permission(req) -> PermissionResponse` (async; awaits verdict
  in review mode).

**Steps.**
- [ ] Write failing test (mock connection): an edit `request_permission` opens a
  diff request; accept → `Allow` response + `EditStatus::Accepted`; reject →
  `Deny` + `Rejected`.
- [ ] Write failing test: a `read_file` `request_permission` auto-allows without a
  diff request.
- [ ] Implement `handle_permission` in the supervisor (review path, verdict-gated).
- [ ] Add `DiffSessionRef` to `Edit`; transition status on verdict; re-project.
- [ ] `[diff]` decoration + open-diff effect in the mode.
- [ ] TUI + GPUI: `[diff]` affordance in both.
- [ ] `cargo test`; fmt; clippy; commit.

**Exit criteria.** An agent edit opens lattice's diff view; accept writes + marks
`Accepted`, reject denies + marks `Rejected`; the permission response is gated on
the verdict; reads auto-run; both renderers show `[diff]`.

---

## AU‑5 — Trust-mode toggle

**Goal.** A per-session `auto_accept` flag; when on, `request_permission`
auto-grants and edits apply without the diff gate. A mode chord flips it.

**Files.**
- Modify `crates/lattice-ai/src/acp/handle.rs` / `supervisor.rs` — `auto_accept`
  in the session state; `AiCmd::SetAutoAccept(bool)`.
- Modify `crates/lattice-ai/src/acp/conversation_mode.rs` — a chord toggles it +
  echoes state; headerline shows the mode (`review` / `auto`).

**Interfaces (produced).**
- `AiState { ..., auto_accept: bool }`; `AiCmd::SetAutoAccept(bool)`;
  `AiClientHandle::set_auto_accept(bool)`.
- Mode chord (e.g. `<leader>ta` or a named action) → toggle + echo.

**Steps.**
- [ ] Write failing test: with `auto_accept=true`, an edit `request_permission`
  auto-allows without opening a diff request.
- [ ] Write failing test: the toggle chord flips `auto_accept` and echoes the state.
- [ ] Implement `auto_accept` in session state + `handle_permission` branch.
- [ ] Implement the toggle chord + headerline indicator.
- [ ] TUI + GPUI: headerline mode indicator in both.
- [ ] `cargo test`; fmt; clippy; commit.

**Exit criteria.** Trust mode auto-applies edits without the diff gate; the chord
flips it and the state is visible; review mode (AU‑4) unchanged when off.

---

## Cross-cutting discipline (every slice)

- **Feature gate:** all new code behind `feature = "acp"`; verify all four combos
  still build at each slice.
- **Off the UI thread:** projection + streaming via `apply_edit_batch_blocking`
  (debounced); no `run_tick_pending` / `refresh_*` inside `Render::render`.
- **Both renderers in lockstep:** any new decoration / virtual-row / headerline
  field lands in TUI *and* GPUI in the same slice
  (`grep -rn "<NewKind>" crates/lattice-ui-gpui/` must be non-empty).
- **Mode owns its surface:** keymap at `MinorMode`/`MajorMode` layer, handler
  bodies in `lattice-ai/acp`; zero `Editor::` method or host `Action` variant added.
- **Diagnostics to `debug!`:** per-chunk streaming logs never `info!`.
