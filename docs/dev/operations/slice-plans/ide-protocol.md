# IDE protocol host — slice plan

Design fragment: `docs/dev/architecture/ide-protocol.md` (the *what* and *why*;
this file owns the *when* and *in what order*).

Feature: lattice as the IDE side of the Claude Code agent protocol. Mode-owned
(`claude-code-mode` minor mode on a reused `BufferKind::Terminal` buffer); one new generic
host primitive (tick-callback registry); writes via existing Effects; `openDiff`
via the diff subsystem. See the design fragment §2/§3/§5 for the rationale.

## Sequencing

- **I0 — Docs seed.** ✅ This slice plan + the design fragment.

- **I1 — Crate + generic tick-callback registry + WS/MCP skeleton.** ✅
  Landed as five green sub-commits I1.0–I1.4 (jsonrpc lift → tick-callback
  registry → crate skeleton → boot wiring + WS round-trip → docs).
  New `crates/lattice-claude-code/` + workspace member + `tokio-tungstenite`. The one
  generic host primitive: a **tick-callback registry** (host service; `register`
  + per-tick `run_all`, wired into `run_tick_pending`). `lattice-claude-code`: `lockfile.rs`
  (writer + RAII unlink), `auth.rs` (token + constant-time header check),
  `transport.rs` (WS accept + auth), `protocol.rs` (MCP envelopes; lift
  `lattice-lsp::jsonrpc` into `lattice-protocol` first — see Risk 3), `dispatch.rs`
  (`initialize`/`tools/list`/`prompts/list` + `tools/list_changed`; tools stubbed),
  `server.rs`, `error.rs`, `commands.rs`. `claude-code-mode` registered (Manual
  activation for now) + `:claude-code-start`/`:claude-code-stop` **ex-commands**
  (crate-owned, bare-named; `apply` closures capture the server handle — design §2
  corrects I0's "mode action handler" framing, which is infeasible for
  ex-commands). **Tests:** tick-callback registry
  register/run; lockfile round-trip + unlink-on-Drop; auth accept/reject;
  `initialize` handshake; `tools/list` enumerates; malformed frame → error, no
  panic.

- **I2 — Read tools (crate-owned read state).** `lattice-claude-code` owns a
  `ClaudeCodeReadState` cache fed by *generic* event-bus subscriptions
  (`DocumentOpened`/`DocumentClosed`/`SelectionsChanged`) registered in the mode's
  lifecycle; on-demand text/path/dirty/selection come from the *generic*
  `BufferStore` service (`handle_for` → `Document` snapshot, off-thread-safe
  `ArcSwap` reads). `reads.rs` = the 5 tools (`getCurrentSelection`,
  `getOpenEditors`, `getWorkspaceFolders` [from config], `getDiagnostics`,
  `checkDocumentDirty`), routed from `tools/call` in `dispatch.rs` (threaded through
  a `DispatchContext`). The ONLY host-side change is a *generic*
  `DiagnosticsQuery::for_uri` extension (lattice-lsp trait + host impl, reusable by
  any mode) for `getDiagnostics`. **NO** host `ClaudeCodeReadService` trait — rejected
  as a mode-ownership violation (design §2/§5). Sub-slices: I2.0 `DiagnosticsQuery::for_uri`;
  I2.1 `snapshot.rs` cache + subscriptions; I2.2 `reads.rs` + dispatch routing +
  server threading. **Tests:** cache←event per tool; absent buffer / no-diagnostics →
  empty (not error); dispatch routes each tool.
  *(I0–I2 = the walking skeleton: a real `claude` CLI attaches via a manual
  `:claude-code-start` + lockfile and reads editor state — validate the contract here.)*

- **I3 — Write tools.** `inbound.rs` (`ClaudeCodeInboundRequest` + replies + `ClaudeCodeInboundBus`)
  + `tools/writes.rs` (`openFile`/`saveDocument`/`close_tab`). `claude-code-mode` registers
  an `ClaudeCodeInbound` drain via the I1 tick-callback registry; the drain returns
  existing Effects (`OpenBufferAt`, save, close) and resolves oneshots. **Tests:**
  request→Effect mapping; oneshot round-trip; unknown path/tab → `ok=false`;
  dropped receiver → graceful error to agent.
  - **I3 fix (2026-06-25, BC.8c follow-up):** `openFile` mapped to the
    *peer-applied* `Effect::OpenBufferAt`, which the inbound tick path discards
    (BC.8c finding: `drain_tick_callbacks` drops `out.effects`; host
    `handle_effect` no-ops `OpenBufferAt`) — so openFile **never actually
    opened** (the I3 test only asserted the oneshot). Now maps to the
    host-applied `Effect::OpenBufferAtColumn` (BC.8c), which opens host-side on
    the tick path. Also dropped the provisional "`character` as a byte" hack:
    the agent's `character` is VS Code utf16, carried verbatim as a `Utf16Pos`
    (`column = None` when no selection → open without forcing the cursor). The
    host does do_edit + the utf16→byte conversion against the opened line.
    Self-contained in `lattice-claude-code` (no host change — the effect already
    exists). Green: lattice-claude-code 41 lib + 3 ws_roundtrip + 14 BC.2 pins.

- **I4 — openDiff (blocking).** `tools/diff.rs` + diff-subsystem registration
  (`StaticSource` agent-text vs `OnDiskSource` baseline, `bind_completion`); the
  `DiffOutcome` forwarder resolves the agent's oneshot on `:diff-accept`/`reject`.
  **Tests:** Accept/Reject → correct reply; `close_tab` mid-diff → session dropped
  + Rejected; pane orientation matches the VS Code contract.

- **I5 — Terminal launch + the IDE buffer.** Add `env: Vec<(String,String)>` to
  `SpawnConfig` (spawner.rs:24 — confirmed absent; thread into `CommandBuilder`).
  `:claude` spawns `claude` in a `BufferKind::Terminal` buffer with
  `CLAUDE_CODE_SSE_PORT`+`ENABLE_IDE_INTEGRATION` injected, activates `claude-code-mode` on
  it, and starts the server. `--ide` CLI flag optional. **TUI+GPUI parity** for any
  new effect. **Tests:** `SpawnConfig.env` reaches the child; `:claude` injects both
  vars + activates `claude-code-mode`; lockfile exists while running.

- **I6 — Notifications.** `notifications.rs` + event-bus subscriber:
  `selection_changed` (coalesced — fires per cursor move), `didChangeActiveEditor`,
  `at_mentioned` via `:claude-send`/`@`. **Tests:** `Event::SelectionsChanged` →
  frame to all conns; dead-conn pruning; coalescing.

- **I7 — Status + hardening.** Headerline/modeline status (running/port/conns) +
  `*messages*` log; loopback-only bind enforced; token gating audited; clean
  teardown (lockfile unlink, conn close) on `:claude-code-stop`/quit; idempotent restart.
  **Tests:** status reflects state; non-loopback refused; lockfile removed on stop;
  never panics on the WS thread.

## Risks / decisions (carry into the slices)

1. Lockfile JSON schema provisional — verify against a live `claude` CLI before I5.
2. `tokio-tungstenite` — first WS dep; ✅ pinned to `0.24` (default features, no
   TLS — loopback + token gated). `getrandom 0.2` added for the auth token only.
3. ✅ Done in I1.0: lifted `lattice-lsp::jsonrpc` into `lattice-protocol` so
   `lattice-claude-code` reuses the JSON-RPC types without an `ide→lsp` crate edge.
4. `SpawnConfig.env` touches a shared struct (derives Clone/Debug, no `..Default`)
   — audit all construction sites in I5.
5. Single active agent connection in v1 (VS Code assumes 1 IDE↔1 agent); reject
   extras with a clear close reason.

## Status

I0 ✅ · I1 ✅ · I2 ✅ · I3 ✅ · I4 🗒 · I5 🗒 · I6 🗒 · I7 🗒
