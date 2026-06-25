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

  - **I4.0 design (captured 2026-06-25 from investigation; implement fresh).**
    **Architecture finding:** openDiff is *blocking* (awaits `DiffOutcome`) AND
    opens a diff — irreducibly `&mut Editor` + carries lattice-diff types
    (`StaticSource`/`OnDiskSource`) that **cannot cross the `Effect` boundary**.
    So it does NOT fit the I3 generic-handler write bus (which maps each request
    to an `Effect`); it mirrors **BC.8d apply-edit** → use the **host-drained**
    inbound (`lattice_mode::inbound::make_inbound_raw`). Shape: a new
    `OpenDiffRequest { original, modified, tab_name, response:
    oneshot<DiffOutcome> }` on a host-drained bus; the host drains it, opens the
    diff session, and `bind_completion(response)`. The agent's openDiff call
    **awaits `response` directly** (NOT the optimistic-ack reply) with **no 5s
    timeout** (the user reviews at their own pace; connection-drop → graceful).
    **REUSE (no new diff machinery):** `StaticSource::new(rope)` (agent text),
    `OnDiskSource::new(path)` (baseline), `DiffSession::bind_completion` /
    `take_completion`, `DiffOutcome::{Accept,Reject}`, the EXISTING
    `do_diff_accept` / `do_diff_reject` teardown (already fires the bound
    oneshot — zero new accept/reject code), `lookup_session_for` / `drop_session`
    (close-tab-mid-diff → drop + the dropped sender surfaces as `Reject`).
    **NEW code (the real work):** (1) a *scratch-buffer* diff-open path — open a
    buffer holding the agent's modified text + register a 2-way session against
    the `OnDiskSource` baseline; modeled on `do_diffsplit` (dispatch.rs:4558,
    ~150 lines of pane/session structure) BUT with a `StaticSource` side instead
    of a second on-disk file (so it is NOT a drop-in reuse of `do_diffsplit`);
    (2) the host-drained inbound wiring (claude-code gets a SECOND bus, alongside
    the I3 handler write bus); (3) `tools/diff.rs` marshalling + the blocking
    await + the VS Code openDiff reply shape (PROVISIONAL until validated vs a
    live CLI). **Sub-steps:** I4.0 = `OpenDiffRequest` + host-drained bus +
    host scratch-buffer diff-open handler (reusing `bind_completion`); I4.1 =
    `tools/diff.rs` + blocking await (no timeout) + reply shape; I4.2 =
    close-tab-mid-diff cancel + tests (Accept/Reject reply, cancel→Rejected, pane
    orientation). **Open decisions for implementation:** original-as-path vs
    original-as-text (the MCP openDiff may send either); pane orientation vs the
    VS Code contract; whether the modified scratch buffer is editable
    (`do`/`dp` apply) or read-only before Accept.

  - **I4 implementation (landed; decisions resolved with Dhruva).** Two UX
    decisions were confirmed before build: **side-by-side (2 panes)** —
    original left / proposed right (UX convention: VS Code / Zed / vimdiff) —
    and **the IDE writes on Accept → `FILE_SAVED`** (the review IS the save).
    The captured `original-as-path-vs-text` question was settled by the wire
    schema: `openDiff` sends `old_file_path` (a path) + `new_file_contents`
    (text), so the baseline is the path's on-disk content and the proposed side
    is the inline text.
    - **I4.0** — `lattice_diff::ProgrammaticDiffRequest` + `ProgrammaticDiffBus`
      (new `lattice-diff/src/programmatic.rs`); the host-drained bus is created
      in `editor_boot` Phase A (`boot.inbound_raw`), its sender registered as a
      service, its receiver seated on `Editor::pending_programmatic_diff_rx` and
      drained in `run_tick_pending` via `drain_inbound_programmatic_diffs`
      (mirrors BC.8d apply-edit). `Editor::open_programmatic_diff` creates two
      in-memory `Document` buffers (baseline = `old_file_path`'s on-disk content;
      proposed = `new_file_contents`, carrying `new_file_path`), `split_active`
      (original→left, proposed→right), and reuses `register_pane_group_diff`
      (slot 0 = baseline, slot 1 = current) — so the left buffer IS the
      baseline (no `StaticSource`/`OnDiskSource` needed). `bind_completion` ties
      the request's oneshot to the session.
    - **I4.1** — the accept-writes-to-disk hook lives in
      `tear_down_single_diff_session`: on `Accept`, the proposed (primary) side's
      LIVE content is written to the recorded `new_file_path` (a host-side
      `programmatic_diff_accept_paths` map, removed on any teardown) BEFORE the
      bound outcome fires, so the `FILE_SAVED` reply is truthful. claude-code:
      new `lattice-diff` dep, `diff.rs` (the `openDiff` tool — send + await with
      NO timeout + the `FILE_SAVED` / `DIFF_REJECTED` content envelope, bypassing
      `tool_text_result`), `DispatchContext.diff` + `openDiff` routing, the
      service read in `install`, and the 4th `install_services` arg.
    - **I4.2** — close-tab cancel: `do_buffer_delete` tears down a *programmatic*
      diff session for the closed participant (scoped via the accept-paths map so
      regular `:diff` is unchanged), dropping the bound sender → the awaiting
      producer rejects (never hangs). Tests cover Accept (file written) / Reject
      (unchanged) / cancel (sender dropped) / user-edits-to-the-proposed-side
      persist on Accept + the side-by-side orientation + `openDiff` reply shapes.
    - **Architecture note (BC.3b preserved):** the request type is a *generic
      diff-subsystem* type in `lattice-diff` (the `DiffSession::bind_completion`
      doc already names "AI proposal flows" / "WorkspaceEdit previews" as
      consumers), so the host's new `pending_*_rx` + drain reference NO
      claude-code internals; claude-code reads the bus as a service, like the
      diagnostics handle.
    - **Reply shape is PROVISIONAL** until validated against a live `claude` CLI.
    - **Both diff sides are editable by design** (Dhruva, 2026-06-25): like
      `:diffsplit`, neither side is read-only. Accept/reject/save target the
      proposed (right) side; the user may also edit the baseline (left) if they
      want — the diff recomputes either way.

- **I5 — Terminal launch + the IDE buffer.** `:claude` spawns the `claude` CLI
  in a `BufferKind::Terminal` buffer wired to talk back to this editor.

  - **I5.0 — `SpawnConfig.env` (foundation).** Add `env: Vec<(String,String)>` to
    `SpawnConfig` (spawner.rs:24 — confirmed absent; derives Clone/Debug, no
    `Default`), thread it into `CommandBuilder` (`cmd.env(k,v)` per pair).
    Construction-site audit (Risk 4): exactly ONE site —
    `Editor::do_terminal_spawn` (dispatch.rs:17210) — gets `env: Vec::new()`.
    **Test (lattice-terminal):** spawn a child with `env`, assert it reaches the
    process (e.g. `sh -c 'printf %s "$VAR"'` echoes the injected value).

  - **I5.1 — `:claude` ex-command (the IDE launch).** A crate-owned bare-dashed
    ex-command in `lattice-claude-code` (like `:claude-code-start`) whose `apply`
    captures the server handle: start the server, obtain the bound **port**,
    spawn `claude` in a Terminal buffer with `env` = `CLAUDE_CODE_SSE_PORT=<port>`
    + `ENABLE_IDE_INTEGRATION=true` (+ `TERM`, inherited otherwise). **KEY
    DECISION (port sequencing):** `ClaudeCodeServerHandle::start()` is a
    non-blocking `cmd_tx` send; the supervisor binds the listener
    asynchronously, so the port is NOT known at `:claude` time. Resolution
    (recommend **A**): pre-bind a `std::net::TcpListener` synchronously when the
    handle starts (`TcpListener::bind("127.0.0.1:0")` → read `local_addr().port()`
    → `set_nonblocking(true)` → hand to the supervisor via
    `tokio::net::TcpListener::from_std`), so `start()` returns the port
    immediately. Rejected: (B) lockfile-only discovery (race — the CLI may start
    before `~/.claude/ide/<port>.lock` exists) and (C) deferred spawn via a tick
    callback polling `ServerState.port` (more moving parts for the same result).
    Because spawning a terminal needs `&mut Editor` and lives in the host, the
    ex-command can't open the terminal directly (an ex-command `apply` gets no
    `&mut Editor`); route the terminal-spawn-with-env through an existing host
    primitive or a new host-applied `Effect` — TBD at implementation (the
    `:claude-code-start` precedent only sends a `cmd_tx`; opening a buffer is the
    new bit, mirror `do_terminal_spawn`). **Confirm with Dhruva before building
    I5.1** (touches the server start API + a host terminal-open path).

  - **I5.2 — mode activation + status + tests + parity.** Activate
    `claude-code-mode` (the minor mode) on the spawned Terminal buffer via the
    mode's `on_activate`. Optional `--ide` CLI flag. **TUI+GPUI parity** for any
    new `Effect`. **Tests:** `:claude` injects both env vars + activates
    `claude-code-mode`; the lockfile exists while running; clean teardown on
    buffer close / `:claude-code-stop`.

- **I6 — Notifications.** `notifications.rs` + event-bus subscriber:
  `selection_changed` (coalesced — fires per cursor move), `didChangeActiveEditor`,
  `at_mentioned` via `:claude-send`/`@`. **Tests:** `Event::SelectionsChanged` →
  frame to all conns; dead-conn pruning; coalescing.

- **I7 — Status + hardening.** ✅ (hardening + teardown) · 🗒 (status segment deferred).
  - **Done:** loopback-only bind (`127.0.0.1` — structural; nothing to "refuse"
    elsewhere); token gating (`x-claude-code-ide-authorization`, constant-time);
    lockfile RAII-unlink on stop; **idempotent restart** (`start()` returns the
    existing port, I5.1a); **conn-close on stop** (NEW — `RunningServer` holds a
    `broadcast::Sender<()>` shutdown signal; dropping it on stop makes each
    connection's read-loop `select!` close the socket, so `:claude-code-stop`
    disconnects the agent instead of leaving it functional); `*messages*`
    lifecycle log (the server `info!`s start/stop). **Tests (ws_roundtrip):**
    `stop_closes_live_connections`, `start_writes_lockfile_and_stop_removes_it`,
    `wrong_token_is_rejected`, malformed-frame-no-panic (dispatch unit test),
    idempotent-restart (server unit test). The status DATA is exposed via
    `handle.snapshot()` (running/port) + `handle.connection_count()`.
  - **Status segment ✅ (`status.rs`):** the `claude-code` modeline element on the
    agent terminal — `claude :PORT [· N conns]` when running, hidden when
    stopped. Mode-owned: `claude-code-mode::on_activate` registers the descriptor
    (idempotent; here not at install — the `ModelineServiceHandle` isn't
    registered until after the Phase-B install list) + its buffer, the Guard's
    Drop unregisters. An off-thread publisher republishes per-buffer **only on
    change** via the generic ML.3 bus path (`publish_typed ModelineElementUpdate`,
    role `modeline.mode_item`) — same as `lattice-lsp::modeline`, so no renderer
    change. Live conn-count via a per-connection `ConnGuard` (Drop decrements +
    wakes); `fire()` uses `notify_one` (permit-stored, no lost wake). Tests:
    `status_content` (stopped/running/conn-count) + a publisher-pushes-on-register
    integration test. **Render not visually verified headless** — the only thing
    a live-CLI / GUI session would confirm.

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

I0 ✅ · I1 ✅ · I2 ✅ · I3 ✅ · I4 ✅ · I5 ✅ · I6 ✅ · I7 ✅

The whole I-series is functionally complete end-to-end: `:claude` launches the
agent, which attaches and drives the editor through the read/write/openDiff
tools, receives selection/at-mention notifications, and the agent terminal shows
a `claude-code` modeline status segment (running/port/conns). The only remaining
open item is PROVISIONAL wire-shape validation against a live `claude` CLI (tool
replies + notification payloads) — plus the optional `--ide` CLI flag.

**I5 landed (2026-06-25):** I5.0 `SpawnConfig.env` + the env-reaches-child
integration test; I5.1a server pre-bind (`start() -> Option<u16>`, sync
`std::net::TcpListener` bind handed to the supervisor via `from_std`,
idempotent); I5.1b `Effect::SpawnTerminal { cmd_line, env, activate_minor }`
in the grammar Effect vocab, host-applied in the effect handler
(`do_terminal_spawn` gained an `env` param; `activate_mode_by_id` for the minor
mode) + the TUI×3 / GPUI×1 no-op parity arms; I5.1c the `:claude` ex-command
(start → port → emit `SpawnTerminal` with `CLAUDE_CODE_SSE_PORT` +
`ENABLE_IDE_INTEGRATION` + `activate_minor:"claude-code-mode"`); I5.2 tests
(`:claude` emits the right effect + starts the server; `start()` returns a port
+ idempotent). Green across grammar/host/terminal/claude-code + workspace +
GPUI build. **Not done (optional / deferred):** the `--ide` CLI flag; richer
running/port/conns status is I7.
