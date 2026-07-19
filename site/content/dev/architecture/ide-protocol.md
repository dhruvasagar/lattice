+++
title = "IDE protocol host — lattice as a Claude Code agent peer"
+++


Lattice speaks the **Claude Code IDE protocol** as the *IDE side*: an external
`claude` CLI connects over loopback and drives the editor — reading the
selection / open buffers / diagnostics, opening files, and proposing edits as
interactive diffs the user Keeps or Rejects. The goal is to develop lattice
*from within lattice*, reusing the diff system and terminal that already exist.

This is the lattice-native analog of `claude-code-ide.el` (Emacs) and of the
VS Code / JetBrains integrations — it implements the same WebSocket + MCP
contract those editors expose.

## 1. Peer protocol, not an extension substrate

`lattice-claude-code` is a **network peer**, architecturally identical in spirit to
`lattice-lsp` (an LSP *client*): a loopback JSON-RPC connection to an external
process. It runs no agent in-process, executes no third-party code in lattice's
address space, and adds no scripting surface. It therefore does **not** violate
"WASM is the only extension substrate — no Lua / vimscript / elisp" (that rule
governs in-process foreign-code execution), and is **orthogonal to the WASM
plugin host** (design.md Phase 7, unbuilt) — it depends on none of it.

## 2. Mode-owned, minimum host touch (Steer A)

`lattice-claude-code` is a **mode that owns its full surface**; the App stays a thin host
exposing generic primitives (`feedback_mode_owns_its_surface`). The acid test —
*a new provider crate adds zero `Editor::` methods and zero new `Action`
variants* — is met:

- **Server + lifecycle:** boot spawns the `ClaudeCodeServer` supervisor task
  (idle until started) on the shared async runtime and registers its
  `ClaudeCodeServerHandle` into `ServiceRegistry`; `register_claude_code_modes`
  registers `claude-code-mode` into the `ModeRegistry`. The server is
  started / stopped by the `:claude-code-*` ex-commands (and, from I5,
  *ensured* running by `claude-code-mode`'s `on_activate`, with its `Guard`
  stopping it on deactivation).
- **Commands (corrected from I0):** the server-lifecycle ex-commands
  (`:claude-code-start` / `:claude-code-stop`; `:claude` / `:claude-send` land
  in I5/I6) are registered **by the crate** with **bare dashed names** — they
  resolve directly via `id_by_name` on the `:` line, so the command surface
  needs **no host alias-table entry**. Each `apply` closure **captures the
  `ClaudeCodeServerHandle`** and drives it directly (a non-blocking `cmd_tx`
  send), returning an **existing** `Effect` (`Echo`). This — *not* a mode
  `ActionHandler` — is the mode-ownership-compliant route for ex-commands: the
  `:` line rejects `CommandKind::Action` (`excommand.rs`), and an ex-command
  `apply` (`Fn(&ExCommandContext) -> Effect`) is handed no `services`, so it
  cannot reach the `ActionHandlerRegistry`. Capturing the subsystem handle
  keeps BOTH the binding (the command name) AND the handler body in the crate,
  with no new host `Editor::` method and no new `Effect` variant. (A future
  *chord*-bound action would use the `ActionHandler` path; ex-commands can't.)
- **Writes** (`openFile`/`saveDocument`/`close_tab`) reuse existing effects
  (`Effect::OpenBufferAt`, save, close-buffer) — no new host write path.
- **Reads (corrected — crate-owned, not host-owned):** the read state + tool logic
  belong to `lattice-claude-code`, NOT the host. The crate holds a
  `ClaudeCodeReadState` cache fed by subscribing to the *generic* event bus
  (`DocumentOpened` / `DocumentClosed` / `SelectionsChanged`) in the mode's
  lifecycle, and answers on-demand text / path / dirty / selection through the
  *generic* `BufferStore` service (`handle_for(id)` → `Document` snapshot reads,
  wait-free off the editor thread). `getDiagnostics` reads a *generic*
  `DiagnosticsQuery::for_uri` extension; `getWorkspaceFolders` comes from the server
  config. The host exposes ONLY generic primitives (event bus, buffer store,
  diagnostics query). An earlier draft proposed a host-implemented
  `ClaudeCodeReadService` / `HostClaudeCodeReads` trait — **rejected** as a
  mode-ownership violation (it would put the crate's surface in the host). The one
  host-side change for reads is a *generic* `DiagnosticsQuery::for_uri` method any
  mode can reuse.

**The single new host primitive** is a generic **tick-callback registry**: any
mode registers a per-tick drain closure. This is reusable infrastructure (it
generalizes the host's existing hardcoded `option_change_rx` / `lsp_log_event_rx`
/ … drains, which are the smell), not an claude-code-specific touch point. `claude-code-mode`
registers its `ClaudeCodeInbound` drain through it. Rejected alternatives: an
claude-code-specific `Editor::drain_inbound_claude_code` (fails the acid test, no merit gain over
the generic registry — UX and perf are identical) and event→`Invocation` routing
(a fixed-at-subscribe-time invocation can't carry structured write payloads —
path / range / contents — cleanly).

## 3. The IDE buffer is terminal-like (Steer B, everything-is-a-buffer)

The IDE surface — where `claude` runs — **reuses the `BufferKind::Terminal`
substrate** (PTY-backed; `claude` is an interactive TUI that wants a PTY). The
terminal's dual-mode is exactly the requested behavior and is a **per-buffer
property, not kind-branching** (`feedback_buffers_no_special_case`):

- **Normal mode:** full vim interaction — motions, yank, scrollback navigation —
  like any other buffer.
- **Insert mode:** keystrokes pass through to the `claude` PTY (terminal-insert),
  i.e. you type your prompt to the agent.

`claude-code-mode` is a **minor mode layered on that terminal buffer** — it does **not**
introduce a new `BufferKind` (which would mean renderer kind-arms + buffer-
registry plumbing, fighting both minimum-host-touch and no-kind-specific-logic).
`claude-code-mode` adds: the protocol server, the headerline status row (running / port /
connected — the async-buffer-status convention, `project_async_buffer_status_in_headerline`),
and diff affordances. `:claude` spawns `claude` in a terminal buffer with the
discovery env injected and activates `claude-code-mode` on it.

## 4. Wire contract (must match VS Code's, for CLI compatibility)

- **Transport:** WebSocket on `127.0.0.1`, dynamic port 10000–65535. JSON-RPC 2.0;
  MCP protocol version `"2024-11-05"`.
- **Discovery:** lockfile `~/.claude/ide/<port>.lock` (VS Code schema — pid,
  workspace folders, auth token, transport, ide name) **and** env vars
  `CLAUDE_CODE_SSE_PORT=<port>` + `ENABLE_IDE_INTEGRATION=true` injected into the
  IDE terminal's child env.
- **Auth:** handshake header `x-claude-code-ide-authorization: <token>` (token
  from the lockfile). Loopback bind + token gate are the security boundary.
- **MCP methods:** `initialize`, `tools/list`, `tools/call`, `prompts/list`
  (empty); emit `notifications/tools/list_changed` post-init.
- **Tools** — reads: `getCurrentSelection`, `getOpenEditors`,
  `getWorkspaceFolders`, `getDiagnostics`, `checkDocumentDirty`. writes:
  `openFile`, `saveDocument`, `close_tab`. blocking: `openDiff`.
- **Notifications:** `selection_changed`, `at_mentioned`,
  `workspace/didChangeActiveEditor`.

The exact lockfile JSON field set is provisional until verified against a live
`claude` CLI (before the terminal-launch slice).

## 5. Three data paths (chosen for paramount #1 + #4)

1. **Reads** — **crate-owned**. `lattice-claude-code` keeps a `ClaudeCodeReadState`
   cache fed by *generic* event-bus subscriptions
   (`DocumentOpened`/`DocumentClosed`/`SelectionsChanged`) plus on-demand
   `BufferStore` snapshot reads (off the editor thread, wait-free `ArcSwap` — no
   actor round-trip); `getDiagnostics` uses a *generic* `DiagnosticsQuery::for_uri`;
   `getWorkspaceFolders` from the server config. Answered entirely on the WS task —
   zero tick involvement, no renderer round-trip, and crucially **no claude-code
   trait in the host** (mode owns its read surface; the host stays generic). This is
   the same cache I6's `selection_changed` notification reads, so it's produced once.
2. **Writes** — an `ClaudeCodeInbound` mpsc channel drained per-tick via the mode's
   registered tick callback; the drain applies the matching `Effect` and resolves
   a `oneshot` reply. `O(pending)` `try_recv`, same bounded shape as LSP's
   existing inbound drains. All I/O + JSON parse happens off-thread on the WS
   task; only the bounded apply touches the editor thread.
3. **`openDiff` (blocking)** — reuse the diff subsystem, but via a SECOND,
   *host-drained* bus, not the I3 write bus: `openDiff` is blocking AND its open
   is irreducibly `&mut Editor` + lattice-diff types that can't cross the
   `Effect` boundary, so it mirrors BC.8d apply-edit. A
   `lattice_diff::ProgrammaticDiffRequest` (a *generic* diff-subsystem type, so
   the host references no IDE-peer internals) carries `old_file_path` /
   `new_file_contents` / `new_file_path` / `tab_name` + a `oneshot<DiffOutcome>`;
   it rides `ProgrammaticDiffBus` (built with `boot.inbound_raw`, registered as a
   service the IDE peer reads). The host drains it per tick and opens a
   **side-by-side two-pane diff** — original (left, baseline) / proposed (right)
   — by creating two in-memory `Document` buffers and reusing
   `register_pane_group_diff` (so the left buffer *is* the baseline; no
   `StaticSource`/`OnDiskSource` needed), then `bind_completion(response)`. Both
   sides are editable (like `:diffsplit`); Accept writes the proposed side's
   *live* content, so a human can tweak the agent's proposal in the diff and have
   the tweaks persist. The
   agent's `tools/call` blocks *on its own WS task* awaiting the oneshot, which
   fires on `:diff-accept` / `:diff-reject` (or a close-tab cancel → the dropped
   sender surfaces as a reject). On **Accept the host writes the proposed side to
   `old_file_path`** before firing the outcome (the review IS the save), so the
   reply is `FILE_SAVED`; Reject → `DIFF_REJECTED`. The diff recompute already
   runs in `spawn_blocking`; the renderer is never blocked — only the agent
   connection. (Reply shape PROVISIONAL until validated vs a live CLI.)

**Graceful degradation is structural:** every WS-thread failure logs-and-skips; a
dropped host receiver yields a JSON-RPC error to the agent, never a hang or panic.

## 6. Paramount-goal alignment

- **#1 perf:** protocol parse/serialize runs off the UI thread; the only main-loop
  cost is the bounded per-tick `ClaudeCodeInbound` drain dispatched through the
  tick-callback registry. No per-frame protocol work.
- **#2 extensibility:** a new peer-protocol axis (alongside LSP), mode-owned;
  capability-gated by loopback bind + token. Revisit a capability token when the
  WASM host lands so a plugin could mediate agent access.
- **#3 strict vim:** the IDE buffer keeps vim grammar in Normal mode (it's a
  terminal buffer); `claude-code-mode` contributes only a minor-mode layer.
- **#4 async:** server is a tokio task; reads use the snapshot; writes use the
  per-tick drain; the one blocking op blocks only the agent's task. Architectural,
  not by discipline.
- **UX (higher court):** agent edits land as the *same* interactive diff the user
  already drives (Keep/Reject) — no surprise mutations, no flicker; the IDE buffer
  feels like the terminal the user already knows.

## 7. Rejected alternatives

- **New `BufferKind::Ide`** — would add renderer kind-arms + registry plumbing and
  invite kind-branching; reusing Terminal + a minor mode is lower-touch and
  rule-honoring.
- **Native embedded agent** — wants the unbuilt WASM host as a sandbox, owns a far
  larger surface (auth, model API, tool loop), couples lattice to one agent.
  Peer-protocol host reuses an external agent and ships sooner. Revisit
  post-Phase-7.
- **claude-code-specific host drain / event→Invocation writes** — see §2.

See the slice plan for sequencing:
`docs/dev/operations/slice-plans/archive/ide-protocol.md`.
