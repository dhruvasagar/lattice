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
| **AU‑3** | Modal input: editable prompt tail, Insert-relocates-to-prompt, Enter sends, `Ctrl-C` interrupt (`AiCmd::Interrupt`); user prompt folded into transcript as a User turn | ✅ done |
| **AU‑4a** | Diff review + approval: agent→client `request_permission` routed to the supervisor; reads auto-run, file-edits → `review_diff` → verdict → response, un-reviewable mutating ops denied (fail closed) | ✅ done |
| **AU‑4b** | `[diff]` Edit-block reflection in the transcript | ⏸ deferred (redundant with `Block::ToolCall`; see note) |
| **AU‑5** | Trust-mode toggle: per-session `auto_accept` + `<C-t>` chord | ✅ done |

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

- **Generic host primitive (not provider-specific) — as built:** a static
  mode declaration `Mode::editable_tail() -> Option<EditableTail>` (`lattice_mode`)
  + the read-only gate in `Editor::apply_edit_blocking` / `apply_edit_batch_blocking`.
  `EditableTail { trailing_lines, first_line_min_byte }` is expressed relative to
  the buffer end (not an absolute `Range`), so it needs no per-buffer seeding and
  stays valid as the owner appends content above it. The gate: a keystroke edit on
  a resolved-`ReadOnly` buffer is rejected unless the active major mode declares a
  tail and `tail.permits(start_line, start_byte, live_line_count)`. One cold-path
  bool short-circuit for normal buffers; the mode-registry lookup + snapshot only
  run on read-only buffers. Any mode can declare a tail (the comint pattern) —
  future `*scratch*`/REPL buffers reuse it.
  > This replaced the originally-planned buffer-local `EditableRange(Range)`: a
  > per-buffer slot would have to be updated as the transcript grows, but the
  > drain task that grows it runs off-thread with only the runtime document
  > handle — it cannot write host `buffer_locals` (documented on `ModeContext`).
  > The end-relative static declaration sidesteps that entirely. See the
  > "Enforcement point" note below for why the gate moved off `run_read_only_motion`.
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
  `<conversation>\n> <prompt>`; the drain seeds `{transcript}> ` once, then
  re-projects ONLY the transcript zone, preserving the user's in-progress prompt.
  No per-frame tail update is needed — `suffix_edit`'s replace range already ends
  at `text_end(last)` (the start of the prompt line), and the static
  `editable_tail()` tracks the prompt as the last line regardless of transcript
  growth. The supervisor also folds each sent prompt into the transcript as a
  User turn (ACP agents don't echo it back), so "you: …" appears on Enter.
- **Interrupt plumbing:** `AiCmd::Interrupt` + `AiClientHandle::interrupt`;
  supervisor sends ACP `session/cancel` for the active turn without ending the
  session.

`ActionContext` exposes `{buffer_id, cursor, services, events}` only (no direct
buffer text) — the `send` handler reaches text through `BufferStoreHandle`.

**Enforcement point (corrected + resolved 2026-07-10).** The design's
originally-located point (`run_read_only_motion`, `dispatch.rs:30612`) was
**incomplete**: that runner only handles Normal-mode command invocations, is
reached only when a mode declares `Mode::invocation_runner()` (which
`AiConversationMode` / `MessagesMode` / `AiLogMode` do **not** — only
help/oil/file-tree/terminal register runners), and never sees Insert-mode typing
(which flows through `do_insert_text`). Investigation confirmed there was in fact
**no read-only enforcement at all** on the edit path for read-only Document
buffers — every `resolved_option::<ReadOnly>` read in the tree is in tests.

The real single chokepoint for all keystroke edits (Insert typing *and* Normal
operators) is `Editor::apply_edit_blocking` / `apply_edit_batch_blocking`
(`dispatch.rs`). The gate lives there (AU‑3a): a keystroke edit on a
resolved-`ReadOnly` buffer is rejected unless the active buffer's major mode
declares an `EditableTail` (`lattice_mode`) and the edit lands inside it. Owner
projections write through the runtime document handle directly and bypass the
gate — the standing "owner writes bypass" rule — so `*messages*` / `*ai:log*`
keep streaming while their history is now correctly keystroke-protected (a latent
gap closed). This also retired the planned per-buffer `EditableRange` buffer-local
+ its off-thread-write problem: the tail is a **static mode declaration**
(`trailing_lines` + `first_line_min_byte`, relative to the buffer end), consulted
directly from the mode registry, invariant as the transcript grows. Verified
against `multibuffer_is_a_regular_buffer.rs` + the full host suite (746 green).

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
- [x] `EditableTail::permits` unit tests (in-prompt allowed, history + marker
  rejected, tracks the live line count) — AU‑3a.
- [x] `EditableTail` + read-only edit gate in `apply_edit_blocking` /
  `apply_edit_batch_blocking`; consulted from the mode's `editable_tail()`
  declaration (no per-buffer seeding) — AU‑3a.
- [x] `AiCmd::Interrupt` + `AiClientHandle::interrupt` + supervisor
  `session/cancel`; connection driver restructured to `cx.spawn` the prompt turn
  so a mid-turn cancel is delivered concurrently — AU‑3b.
- [x] Mode keymap (`i`/`a`/`o`/`A`/`I`/`O` → focus-prompt, `<CR>` → send, `<C-c>`
  → interrupt) + `action_handlers()` bodies + `action:ai-conv-*` command shells;
  keymap/handler/tail contract tests — AU‑3c.
- [x] Projection: `<transcript>\n> <prompt>` layout; drain re-projects only the
  transcript zone, preserving the prompt; supervisor folds the sent prompt into
  the transcript as a User turn — AU‑3c.
- [x] TUI + GPUI: **no renderer work** — the prompt is plain buffer text and no
  new `Effect` / decoration variant was introduced, so both renderers draw it
  through the standard Document path.
- [x] `cargo test` (mode + host + ai acp, all green); clippy clean; committed
  (AU‑3a / AU‑3b / AU‑3c). Note: did **not** run `cargo fmt` — the repo's
  committed style is not default-rustfmt-clean; hand-match surrounding style.

**Exit criteria.** Normal-mode motions roam the whole buffer; Insert always lands
in the prompt; history is unmutable; Enter sends + user turn appears; `Ctrl-C`
interrupts without ending the session; both renderers. ✅

> **Follow-up (not blocking AU‑3):** the end-to-end host-boot integration test
> (drive `i` → type → `<CR>` on a live `*ai:opencode*` and assert the transcript
> + cleared prompt) is deferred; AU‑3 lands with unit coverage of the gate
> (`permits`), the mode surface (keymap/handlers/tail), and the store
> (`push_user_text`). The interaction is covered by construction — the gate is
> the single edit chokepoint and the mode declares the tail the gate reads.

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

**What landed (AU‑4a, 2026-07-10).** The agent→client `session/request_permission`
direction, which the connection never handled before. `Connection::spawn`
registers `on_receive_request::<RequestPermissionRequest>`, forwarding
`(request, responder)` over a new unbounded channel (mirroring the notification
path) and answering `Cancelled` itself if the receiver is gone. The supervisor's
`drain_permissions` spawns a per-request task (so a long review never blocks the
next request or a Stop/Interrupt); `resolve_decision` → `classify_permission`:
- Read-class kinds (Read/Search/Fetch/Think/SwitchMode) auto-run.
- A tool call carrying a `ToolCallContent::Diff` opens a `review_diff` (the shared
  primitive MCP's openDiff uses) and gates the response on the verdict.
- **Everything else is denied (fail closed)** — a background security review
  flagged the original auto-allow fallback as a permission bypass (the agent could
  run arbitrary commands with no consent). Trust mode (AU‑5) is the opt-in that
  turns the denied set into auto-allow. A command-confirmation surface for
  non-file operations is a follow-up.

The diff bus is pulled from the service registry in `acp::install` exactly as MCP
does. Classification / option-picking / diff-extraction / the trust fold are pure
and unit-tested. `origin_session` tags diffs by the per-process session index.

**AU‑4b (deferred — the `[diff]` Edit-block).** The design called for pushing a
`Block::Edit { path, session, status }` on review-start and transitioning it on
verdict, with a `[diff]` reopen affordance. On execution this proved **redundant
with `Block::ToolCall`**: an agent file-edit already streams as a tool call
(AU‑1 maps `SessionUpdate::ToolCall` → `Block::ToolCall` with a running→ok/err
status), so a separate Edit block double-represents the same operation. Per
heuristic #1 (don't add surface without a concrete merit win) the separate block
is deferred; the review is already visible via the ToolCall block + the diff view
that opens for it + AU‑5's mode echo. If real opencode usage shows the explicit
accept/reject verdict (distinct from execution status) or a diff-reopen
affordance is worth the redundancy, revisit — likely by annotating the existing
ToolCall block rather than pushing a second block. `Block::Edit` stays in the
model (AU‑1) unused for now.

**Exit criteria (met for AU‑4a).** An agent edit opens lattice's diff view; the
permission response is gated on the verdict (accept → allow, reject → deny);
reads auto-run; un-reviewable mutating ops are denied. No renderer work (the
diff view is the existing `DiffSession` surface).

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

**What landed (2026-07-10).**
- [x] `AiState.auto_accept` (default false = review mode) + `AiCmd::SetAutoAccept`
  + `AiClientHandle::set_auto_accept` / `toggle_auto_accept`.
- [x] Supervisor owns an `Arc<AtomicBool>` the per-request permission tasks read
  live; `SetAutoAccept` updates it + republishes `AiState`; a new `Start` resets
  it (trust never carries across sessions).
- [x] `resolve_decision(trusted, request, origin)` folds trust over the
  classification (pure, unit-tested): trust → auto-allow; else classify.
- [x] Mode binds `<C-t>` (Normal) → `action:ai-conv-toggle-trust`; the handler
  flips the flag and echoes the new mode.
- [x] Trust-bypass + toggle-handle tests; combos build; clippy clean; committed.
- [~] **Headerline mode indicator deferred.** The echo is the visible reflection;
  a `review`/`auto` headerline row would be new renderer surface (both peers) and
  is deferred — the same no-new-renderer-surface posture as AU‑3/AU‑4. `AiState`
  already republishes `auto_accept`, so a future modeline/headerline element can
  read it with no supervisor change.

**Exit criteria (met).** Trust mode auto-grants without the diff gate; `<C-t>`
flips it and echoes the state; review mode (AU‑4a) is unchanged when off. ✅

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
