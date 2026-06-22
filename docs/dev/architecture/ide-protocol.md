# IDE protocol host — lattice as a Claude Code agent peer

Lattice speaks the **Claude Code IDE protocol** as the *IDE side*: an external
`claude` CLI connects over loopback and drives the editor — reading the
selection / open buffers / diagnostics, opening files, and proposing edits as
interactive diffs the user Keeps or Rejects. The goal is to develop lattice
*from within lattice*, reusing the diff system and terminal that already exist.

This is the lattice-native analog of `claude-code-ide.el` (Emacs) and of the
VS Code / JetBrains integrations — it implements the same WebSocket + MCP
contract those editors expose.

## 1. Peer protocol, not an extension substrate

`lattice-ide` is a **network peer**, architecturally identical in spirit to
`lattice-lsp` (an LSP *client*): a loopback JSON-RPC connection to an external
process. It runs no agent in-process, executes no third-party code in lattice's
address space, and adds no scripting surface. It therefore does **not** violate
"WASM is the only extension substrate — no Lua / vimscript / elisp" (that rule
governs in-process foreign-code execution), and is **orthogonal to the WASM
plugin host** (design.md Phase 7, unbuilt) — it depends on none of it.

## 2. Mode-owned, minimum host touch (Steer A)

`lattice-ide` is a **mode that owns its full surface**; the App stays a thin host
exposing generic primitives (`feedback_mode_owns_its_surface`). The acid test —
*a new provider crate adds zero `Editor::` methods and zero new `Action`
variants* — is met:

- **Server + lifecycle:** `ide-mode`'s `on_activate` spawns the WebSocket server
  as a tokio task on a dedicated runtime; the returned `Guard` aborts it on
  deactivation. The crate's `register_ide_*` fns register `IdeServerHandle` into
  `ServiceRegistry` and the mode into the `ModeRegistry` at boot.
- **Commands:** ex-commands (`:ide-start`/`:ide-stop`/`:claude`/`:claude-send`)
  route to **mode-owned action handlers** (`ActionHandler: Fn(&ActionContext) ->
  Option<Effect>`), not host `Editor::do_x`. Handlers read `buffer_id`/`cursor`/
  `services`/`events` from `ActionContext` and return **existing** `Effect`s.
- **Writes** (`openFile`/`saveDocument`/`close_tab`) reuse existing effects
  (`Effect::OpenBufferAt`, save, close-buffer) — no new host write path.
- **Reads** are served on the WS task from the published
  `Arc<ArcSwap<RenderState>>` snapshot + a buffer-registry read handle + the LSP
  diagnostics cache, all handed to the mode at boot as services.

**The single new host primitive** is a generic **tick-callback registry**: any
mode registers a per-tick drain closure. This is reusable infrastructure (it
generalizes the host's existing hardcoded `option_change_rx` / `lsp_log_event_rx`
/ … drains, which are the smell), not an ide-specific touch point. `ide-mode`
registers its `IdeInbound` drain through it. Rejected alternatives: an
ide-specific `Editor::drain_inbound_ide` (fails the acid test, no merit gain over
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

`ide-mode` is a **minor mode layered on that terminal buffer** — it does **not**
introduce a new `BufferKind` (which would mean renderer kind-arms + buffer-
registry plumbing, fighting both minimum-host-touch and no-kind-specific-logic).
`ide-mode` adds: the protocol server, the headerline status row (running / port /
connected — the async-buffer-status convention, `project_async_buffer_status_in_headerline`),
and diff affordances. `:claude` spawns `claude` in a terminal buffer with the
discovery env injected and activates `ide-mode` on it.

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

1. **Reads** — answered on the WS task from the published snapshot; zero tick
   involvement, no renderer round-trip.
2. **Writes** — an `IdeInbound` mpsc channel drained per-tick via the mode's
   registered tick callback; the drain applies the matching `Effect` and resolves
   a `oneshot` reply. `O(pending)` `try_recv`, same bounded shape as LSP's
   existing inbound drains. All I/O + JSON parse happens off-thread on the WS
   task; only the bounded apply touches the editor thread.
3. **`openDiff` (blocking)** — reuse the diff subsystem: register a session with
   the agent's proposed text as a `StaticSource` against the on-disk baseline as
   an `OnDiskSource`, then `bind_completion(oneshot<DiffOutcome>)`. The agent's
   `tools/call` blocks *on its own WS task* awaiting the oneshot, which fires on
   `:diff-accept` / `:diff-reject`. The diff recompute already runs in
   `spawn_blocking`; the renderer is never blocked — only the agent connection.

**Graceful degradation is structural:** every WS-thread failure logs-and-skips; a
dropped host receiver yields a JSON-RPC error to the agent, never a hang or panic.

## 6. Paramount-goal alignment

- **#1 perf:** protocol parse/serialize runs off the UI thread; the only main-loop
  cost is the bounded per-tick `IdeInbound` drain dispatched through the
  tick-callback registry. No per-frame protocol work.
- **#2 extensibility:** a new peer-protocol axis (alongside LSP), mode-owned;
  capability-gated by loopback bind + token. Revisit a capability token when the
  WASM host lands so a plugin could mediate agent access.
- **#3 strict vim:** the IDE buffer keeps vim grammar in Normal mode (it's a
  terminal buffer); `ide-mode` contributes only a minor-mode layer.
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
- **ide-specific host drain / event→Invocation writes** — see §2.

See the slice plan for sequencing:
`docs/dev/operations/slice-plans/ide-protocol.md`.
