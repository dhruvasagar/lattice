# IDE protocol host — slice plan

Design fragment: `docs/dev/architecture/ide-protocol.md` (the *what* and *why*;
this file owns the *when* and *in what order*).

Feature: lattice as the IDE side of the Claude Code agent protocol. Mode-owned
(`ide-mode` minor mode on a reused `BufferKind::Terminal` buffer); one new generic
host primitive (tick-callback registry); writes via existing Effects; `openDiff`
via the diff subsystem. See the design fragment §2/§3/§5 for the rationale.

## Sequencing

- **I0 — Docs seed.** ✅ This slice plan + the design fragment.

- **I1 — Crate + generic tick-callback registry + WS/MCP skeleton.**
  New `crates/lattice-ide/` + workspace member + `tokio-tungstenite`. The one
  generic host primitive: a **tick-callback registry** (host service; `register`
  + per-tick `run_all`, wired into `run_tick_pending`). `lattice-ide`: `lockfile.rs`
  (writer + RAII unlink), `auth.rs` (token + constant-time header check),
  `transport.rs` (WS accept + auth), `protocol.rs` (MCP envelopes; lift
  `lattice-lsp::jsonrpc` into `lattice-protocol` first — see Risk 3), `dispatch.rs`
  (`initialize`/`tools/list`/`prompts/list` + `tools/list_changed`; tools stubbed),
  `server.rs`, `error.rs`. `ide-mode` registered (Manual activation for now) +
  `:ide-start`/`:ide-stop` action handlers. **Tests:** tick-callback registry
  register/run; lockfile round-trip + unlink-on-Drop; auth accept/reject;
  `initialize` handshake; `tools/list` enumerates; malformed frame → error, no
  panic.

- **I2 — Read tools.** `snapshot.rs` (`IdeReadState`) + a boot publisher (from
  `RenderState`/`BufferRegistry`/diagnostics cache, registered as a read service)
  + `tools/reads.rs`: `getCurrentSelection`, `getOpenEditors`,
  `getWorkspaceFolders`, `getDiagnostics`, `checkDocumentDirty`. **Tests:**
  snapshot→result per tool; absent buffer / no-diagnostics → empty (not error).
  *(I0–I2 = the walking skeleton: a real `claude` CLI can attach via a manual
  `:ide-start` + lockfile and read editor state — validate the contract here.)*

- **I3 — Write tools.** `inbound.rs` (`IdeInboundRequest` + replies + `IdeInboundBus`)
  + `tools/writes.rs` (`openFile`/`saveDocument`/`close_tab`). `ide-mode` registers
  an `IdeInbound` drain via the I1 tick-callback registry; the drain returns
  existing Effects (`OpenBufferAt`, save, close) and resolves oneshots. **Tests:**
  request→Effect mapping; oneshot round-trip; unknown path/tab → `ok=false`;
  dropped receiver → graceful error to agent.

- **I4 — openDiff (blocking).** `tools/diff.rs` + diff-subsystem registration
  (`StaticSource` agent-text vs `OnDiskSource` baseline, `bind_completion`); the
  `DiffOutcome` forwarder resolves the agent's oneshot on `:diff-accept`/`reject`.
  **Tests:** Accept/Reject → correct reply; `close_tab` mid-diff → session dropped
  + Rejected; pane orientation matches the VS Code contract.

- **I5 — Terminal launch + the IDE buffer.** Add `env: Vec<(String,String)>` to
  `SpawnConfig` (spawner.rs:24 — confirmed absent; thread into `CommandBuilder`).
  `:claude` spawns `claude` in a `BufferKind::Terminal` buffer with
  `CLAUDE_CODE_SSE_PORT`+`ENABLE_IDE_INTEGRATION` injected, activates `ide-mode` on
  it, and starts the server. `--ide` CLI flag optional. **TUI+GPUI parity** for any
  new effect. **Tests:** `SpawnConfig.env` reaches the child; `:claude` injects both
  vars + activates `ide-mode`; lockfile exists while running.

- **I6 — Notifications.** `notifications.rs` + event-bus subscriber:
  `selection_changed` (coalesced — fires per cursor move), `didChangeActiveEditor`,
  `at_mentioned` via `:claude-send`/`@`. **Tests:** `Event::SelectionsChanged` →
  frame to all conns; dead-conn pruning; coalescing.

- **I7 — Status + hardening.** Headerline/modeline status (running/port/conns) +
  `*messages*` log; loopback-only bind enforced; token gating audited; clean
  teardown (lockfile unlink, conn close) on `:ide-stop`/quit; idempotent restart.
  **Tests:** status reflects state; non-loopback refused; lockfile removed on stop;
  never panics on the WS thread.

## Risks / decisions (carry into the slices)

1. Lockfile JSON schema provisional — verify against a live `claude` CLI before I5.
2. `tokio-tungstenite` — first WS dep; pin a version (the repo is dep-careful).
3. Lift `lattice-lsp::jsonrpc` into `lattice-protocol` (pre-I1) so `lattice-ide`
   reuses the JSON-RPC types without an `ide→lsp` crate edge.
4. `SpawnConfig.env` touches a shared struct (derives Clone/Debug, no `..Default`)
   — audit all construction sites in I5.
5. Single active agent connection in v1 (VS Code assumes 1 IDE↔1 agent); reject
   extras with a clear close reason.

## Status

I0 ✅ · I1 🗒 · I2 🗒 · I3 🗒 · I4 🗒 · I5 🗒 · I6 🗒 · I7 🗒
