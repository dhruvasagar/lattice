# LSP Architecture (developer reference)

This document is the implementer-side companion to
[design.md §5.4](design.md). design.md is the terse,
principle-led canonical text; this is the longer-form "how it
actually works", with concrete pointers into the
`lattice-lsp` crate.

User-facing help lives in
[`../../user/lsp.md`](../../user/lsp.md). The feature
tracking matrix lives in
[`../notes/lsp-features.md`](../notes/lsp-features.md).

---

## 1. Layout

```
crates/lattice-lsp/
├── src/
│   ├── lib.rs            -- public re-exports
│   ├── framing.rs        -- LSP `Content-Length` header parser (pure)
│   ├── jsonrpc.rs        -- JSON-RPC 2.0 typed messages
│   ├── codec.rs          -- tokio AsyncRead/AsyncWrite codec
│   ├── transport.rs      -- child-process spawn + stdio capture
│   ├── config.rs         -- ServerConfig + builtin registry + root resolution
│   ├── capabilities.rs   -- client capability advertise + Capabilities snapshot
│   ├── pending.rs        -- Pending<T> (oneshot wrapper, mirror of runtime's Pending)
│   ├── error.rs          -- LspError enum + classifier
│   ├── actor.rs          -- per-server tokio actor + ServerHandle (editor-facing)
│   ├── position.rs       -- utf-8 ↔ utf-16 ↔ utf-32 column conversion
│   ├── sync.rs           -- DocSync (didOpen / didChange / didClose)
│   └── diagnostics.rs    -- DiagnosticsBus (broadcast for publishDiagnostics)
├── tests/
│   ├── common/mod.rs     -- in-process MockServer fixture
│   ├── handshake.rs      -- actor + handshake integration tests
│   ├── sync.rs           -- DocSync integration tests
│   └── diagnostics.rs    -- diagnostics broadcast integration tests
└── benches/
    └── lsp.rs            -- framing / encode / decode / position conversion
```

Every public item has rustdoc; this document explains how
they fit together.

---

## 2. Three-task topology per server

```
+---------+          +-----------+
| editor  |--cmd-->  |   actor   |
| (App)   |<-evt--   |   task    |
+---------+          +-----------+
                      ^      |
                  in_rx      out_tx
                      |      v
                  +---------+    +----------+
                  |read_loop|    |write_loop|
                  +---------+    +----------+
                       ^              |
                  LspReader       LspWriter
                       |              |
                       |          server stdin
                       |
                  server stdout
```

**`actor` task** owns:

- The pending-requests table (`HashMap<RequestId, oneshot::Sender>`).
- The negotiated `Capabilities`.
- A monotonic JSON-RPC request-id counter.
- The `DiagnosticsBus` (publish side).

**`read_loop` task** owns:

- The `LspReader<BufReader<ChildStdout>>`.
- One `mpsc::UnboundedSender<Message>` to the actor.

**`write_loop` task** owns:

- The `LspWriter<ChildStdin>` (behind a `Mutex` -- shared with
  the handshake path, which fires the initial `initialize`
  request before the loop starts).

**`stderr_drain` task** (separate, fire-and-forget): reads
each line from `ChildStderr` and emits it as
`tracing::warn!(server_id, msg = ...)`.

### Why three tasks instead of one

A single-task design works for typewriter-pace editing but
collapses under burst loads. Two such bursts are typical:

1. **Indexing.** rust-analyzer at startup publishes diagnostics
   for every crate in the workspace, plus `$/progress` events
   throughout. A single-threaded read+write+dispatch loop
   would fall behind.
2. **Fast scrolling.** Semantic-tokens delta requests fire
   per visible-range change. With many edits in flight,
   inbound responses interleave with outbound `didChange`s.

Three tasks let the OS schedule reads and writes on different
cores, and keep the actor task small (no I/O on its critical
path).

---

## 3. Actor command dispatch

```rust
enum ActorCmd {
    // Request / response.
    Request {
        method: String,
        params: Option<Value>,
        reply: oneshot::Sender<LspResult<Value>>,
    },
    Notify { method: String, params: Option<Value> },
    Cancel { id: i64 },

    // Document sync (actor owns the mirror -- see §11).
    OpenDoc { uri: Uri, language_id: String, text: String },
    RecordEdit { uri: Uri, edit: Edit },
    Flush { uri: Uri },
    FlushAll,
    CloseDoc { uri: Uri },

    Shutdown { reply: oneshot::Sender<LspResult<()>> },
}
```

`ServerHandle::request<P, R>` returns `Pending<R>`:

1. Serialise `P` to `serde_json::Value`. Failures are
   short-circuited via `Pending::ready_err`.
2. Send `ActorCmd::Request { method, params, reply: oneshot }`
   on the unbounded mailbox.
3. Spawn a small relay task that awaits the
   `oneshot::Receiver<LspResult<Value>>` and
   `serde_json::from_value::<R>` the result; send through a
   second oneshot wrapped in the returned `Pending`.

Two channels because the actor stores `Value`-bearing senders
(uniform for all methods); the relay does the per-method
type narrowing without contaminating the actor's pending
table.

`ServerHandle::notify` is fire-and-forget. `ServerHandle::cancel`
emits `$/cancelRequest` and resolves the matching pending entry
locally (so the caller doesn't wait for the server's ack).
`ServerHandle::shutdown` runs `shutdown` request → `exit`
notification → wait for child exit (5s timeout) → drain
pending with `LspError::ActorGone`.

---

## 4. Handshake

Before `spawn` returns:

1. Spawn `read_loop` and `write_loop`.
2. Build `InitializeParams` with `workspace_folders`,
   `process_id`, `client_info`, `root_uri`, `trace=Off`,
   `capabilities` from `capabilities::client_capabilities()`.
3. Send via `out_tx`.
4. Loop on `in_rx.recv()` waiting for the matching response.
   Pre-handshake notifications (`window/logMessage`,
   `$/progress`) are routed through
   `handle_pre_handshake_message` which logs and, for
   `publishDiagnostics`, broadcasts via the bus.
5. Decode `InitializeResult`. Failure here surfaces as
   `LspError::HandshakeFailed`.
6. Send `initialized` notification.
7. Build the negotiated `Capabilities` (server-advertised
   `position_encoding` overrides our preference).
8. Return `ServerHandle` to the caller.

Failure at any step tears the actor down (`kill_on_drop` on
the `Child` ensures the server process exits) and returns
`LspError::HandshakeFailed` with a concrete reason.

---

## 5. Document synchronisation (`DocSync`)

`DocSync` is **pure state**. It computes the LSP-shaped
payloads (`DidOpenTextDocumentParams`,
`DidChangeTextDocumentParams`, `DidCloseTextDocumentParams`)
and returns them; it does no I/O, holds no `ServerHandle`, and
takes no locks. The actor that owns the `DocSync` does all the
sending. This separation is what lets the actor's main loop
(§11) be a single-task select! over `cmd_rx + flush_sleep`
without re-entering anything else.

`DocSync::open(uri, language_id, text) -> DidOpenTextDocumentParams` →

- Build a `didOpen` payload with `version=1`.
- Store a `String` mirror of the text + the language id and
  current version.
- The actor ships the returned params on `out_tx` as a
  `textDocument/didOpen` notification.

`DocSync::record_edit(&caps, uri, edit) -> Result<()>` →

- Look up line text from the BEFORE-state mirror at
  `edit.range.start.line` and `edit.range.end.line`.
- Convert lattice's `Position { line, byte }` to LSP
  `Position { line, character }` via the negotiated encoding
  (`position::byte_to_lsp_character`).
- Build a `TextDocumentContentChangeEvent` with the LSP range
  and the new text.
- Apply the edit to the mirror.
- Bump `version`.
- Push to the per-doc `pending` queue.

`DocSync::take_flush_payload(&caps, uri) -> Option<DidChangeTextDocumentParams>` →

- Read the negotiated `TextDocumentSyncKind`:
  - `Incremental` → drain queued events into one `didChange`
    payload.
  - `Full` → drop queued events; payload carries the entire
    mirror with no range.
  - `None` → clear the queue; return `None`.
- Bump the wire-side version (matches mirror version).
- The actor sends the returned params if `Some`.

`DocSync::take_flush_all_payloads(&caps) -> Vec<(Uri, DidChangeTextDocumentParams)>`
is the multi-URI form; the actor iterates and sends each.

`DocSync::close(&caps, uri) -> Option<ClosePayloads>` returns a
struct pairing an optional final `didChange` with the
`didClose`; the actor sends them in order then drops the
mirror.

The mirror is a `String` (not a `Rope`) because the LSP layer
only ever splices one contiguous region per edit; per-line
indexing is rare and bounded by line count.

### Single-writer invariant

The actor task is the **only** writer to its `DocSync`
mirror. Edits arrive as `ActorCmd::RecordEdit` on the
unbounded mailbox -- FIFO with `OpenDoc`, `Flush`, and any
other cmd. There is no shared mutex. Two consequences:

- The mirror cannot diverge from the wire stream; every
  applied edit is also queued for `didChange`, and the queue
  drains in order on flush.
- The UI thread can publish edits at full keystroke rate
  (one `Event::DocumentChanged` per applied edit) without
  ever waiting on the actor or the supervisor.

See §11 for the publish path that feeds RecordEdit.

### Position encoding

The actor reads the negotiated `PositionEncodingKind` from
the `Capabilities` snapshot embedded in the `ServerHandle`.
`utf-8` is preferred (one byte == one code unit; matches
lattice's internal `Position::byte`); `utf-16` is the LSP 3.16
fallback that older servers mandate; `utf-32` is rare but
handled.

`position::byte_to_lsp_character` dispatches once per call:
utf-8 short-circuits to a return; utf-16 walks the prefix
counting `c.len_utf16()`; utf-32 walks counting characters.
The reverse direction (`lsp_character_to_byte`) is used for
ranges arriving FROM the server.

Bench numbers (Background-class):

| Bench | Time |
|---|---|
| `position::utf8_passthrough` | ~1ns |
| `position::utf16_cjk_line` (32 CJK chars) | ~23ns |
| `position::utf16_to_byte_cjk` | ~43ns |

---

## 6. Diagnostics routing

`DiagnosticsBus` is a thin facade over
`tokio::sync::broadcast::Sender<DiagnosticEvent>`:

- One bus per actor, capacity `DIAGNOSTICS_CHANNEL_CAPACITY`
  (256).
- `ServerHandle::subscribe_diagnostics()` returns a
  `broadcast::Receiver<DiagnosticEvent>`.
- The actor's `handle_server_notification`, on
  `textDocument/publishDiagnostics`, deserialises the params,
  builds a `DiagnosticEvent`, and `bus.publish(event)`.
- Lagging consumers drop oldest first -- correct for
  diagnostics, since the latest publish supersedes.

`DiagnosticEvent` carries:

```rust
pub struct DiagnosticEvent {
    pub server_id: Arc<str>,
    pub uri: Uri,
    pub version: Option<i32>,
    pub diagnostics: Arc<[Diagnostic]>,
}
```

`Arc`-wrapped to keep the per-subscriber clone cost O(1).

The editor-side `DiagnosticsLayer` (4.1.d.ii) holds the per-URI
latest event, drops events older than `DocSync::version(uri)`,
and exposes range / severity lookups for the renderer.

---

## 7. Cancellation model

Two scopes:

- **Actor-internal:** the `Pending` resolves with
  `LspError::Cancelled` when `ServerHandle::cancel(id)` is
  called. The actor sends `$/cancelRequest` to the server so
  it can free its scheduling slot (advertised via
  `general.staleRequestSupport.cancel`).
- **Editor-driven supersession:** when a newer same-flavour
  request supersedes a stale one (e.g. another keystroke
  produces a new completion request before the prior returned),
  the editor calls `cancel` on the stale id. This is the
  per-feature dispatch's responsibility -- 4.2 navigation
  features add the supersession boilerplate.

Cancellation is **cooperative**, not enforced: a misbehaving
server can ignore `$/cancelRequest` and run to completion.
The actor still resolves the local `Pending` immediately;
the late response, if any, is logged and discarded.

---

## 7a. Logging (`logging` module)

Layered to mirror emacs's `*lsp-log*` / `*<server> stderr*`
convention on lattice's everything-is-a-buffer surface
(§5.9). One [`LspLogger`] per LSP subsystem; the App holds it,
each actor gets a clone.

### Records and rings

```rust
pub enum LogLevel { Trace, Debug, Info, Warn, Error }
pub enum LogSource { Client, Stderr, LspMessage, LspShowMessage, Trace }

pub struct LogRecord {
    pub timestamp: SystemTime,
    pub server_id: Option<Arc<str>>,  // None = subsystem-wide
    pub level: LogLevel,
    pub source: LogSource,
    pub message: String,
}
```

[`LogRing`] is a bounded `VecDeque<LogRecord>` (default 10 000
records / ring; eviction on push when full). The
[`LspLogger`] holds:

- one global ring (subsystem-wide, `server_id == None`);
- one per-server ring keyed by `Arc<str>`;
- per-server min-level overrides (default `Info`);
- per-server trace toggle (default off).

### Routing rules

`logger.log(server_id, level, source, msg)` routes by:

1. **Trace gate.** If `level == Trace` and `server_id` is set,
   the per-server trace toggle decides:
   - **on**: skip the level filter (deliberate opt-in) and
     append.
   - **off**: short-circuit and return -- no allocation, no
     ring touch.
   Subsystem-wide (`server_id == None`) Trace records honour
   the level filter normally.
2. **Level filter.** Drop iff `level < effective_min(server_id)`
   where `effective_min` is the per-server override (if set) or
   the subsystem default.
3. **Tracing fan-out.** Emit a `tracing::*` event at the
   matching level. Always fires; survives without a subscriber.
4. **Ring push.** Append to the global ring (server_id None) or
   the per-server ring (creating the ring on first emission).

### Where records come from in the actor

| Record source | Where in `actor.rs` | Level | Notes |
|---|---|---|---|
| `Client` -- spawn / handshake / shutdown | `spawn_with_io` (Info: spawn + handshake-complete) | Info | `server_id = None` for spawn (precedes negotiation), then per-server. |
| `Client` -- read_loop / write_loop errors | `read_loop`, `write_loop` | Error | Pipe close, decode failures. |
| `Client` -- decode of `publishDiagnostics` / unhandled methods | `handle_server_notification`, `handle_server_request` | Debug / Warn | Routed through logger; no `tracing::warn!` left. |
| `Stderr` | `stderr_drain` | Warn | One record per stderr line. Yellow in the rendered buffer. |
| `LspMessage` | `window/logMessage` handler | severity from server's `type` field | LSP severity 1=Error, 2=Warning, 3=Info, 4/5=Debug. |
| `LspShowMessage` | `window/showMessage` handler | same as above | Distinct source for differentiation in the buffer view. |
| `Trace` | `read_loop`, `write_loop` interceptors | Trace | Gated by `is_tracing(server_id)`. Body truncated at 240 chars. |

### Trace interceptor

In `read_loop`:

```rust
if logger.is_tracing(&server_id) {
    logger.log(
        Some(&server_id),
        LogLevel::Trace,
        LogSource::Trace,
        format!("← {}", trace_render(&msg)),
    );
}
```

`trace_render` formats the message as
`Request id=... method=... body=...` /
`Notification method=... body=...` /
`Response id=... OK|ERR ...` plus a body excerpt. Cheap; only
runs when trace is on.

`is_tracing` is a single `HashSet<Arc<str>>::contains` --
benchmarked at ~9 ns when the toggle is off. Trace-on emission
costs ~100 ns / record (the lock + push + format dominate).

### Editor consumption

> **Note (B' refactor, in flight).** Buffer ownership for `*lsp*` / `*lsp:<server>:<workspace>*` / `*lsp:<server>:<workspace>:trace*` is migrating to dedicated modes (`LspLogMode` / `LspServerLogMode` / `LspTraceLogMode`) per design.md §5.10.5. Per-instance keying lands in B'.2; mode ownership in B'.3–B'.5; the App-side `drain_lsp_log_events` deletes in B'.6. The text below describes the *shipped* (pre-B') architecture; sections will rewrite slice-by-slice as B' lands. The contract (one publisher, three filtering subscribers; mode owns subscription + append) is fixed.

The buffer-backed views (`*lsp*` /
`*lsp:<server>*` / `*lsp:<server>:trace*`) snapshot the rings
on demand:

```rust
let records = logger.snapshot_global();           // for *lsp*
let records = logger.snapshot_server(&server_id); // for per-server
```

`snapshot_*` clones the records (cheap -- only the message
String is heavy; everything else is `Arc` or `Copy`). The
buffer view is a normal lattice ReadOnly buffer; standard
motions, search, yank all work. The trace buffer benefits
from a custom highlighter (4.1.g): JSON syntax + leading `→`
/ `←` markers picked out.

### Trace toggle vs. trace view

Two ex-commands, separated so a peek at the JSON-RPC stream
doesn't flip the toggle off:

| Command                  | Effect                                                                                                                                                                                                                                |
|--------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `:lsp-trace [server]`    | Pure toggle. Flips `LspLogger::is_tracing(server)`; emits an info echo. No buffer is opened.                                                                                                                                          |
| `:lsp-trace-log [server]`| Opens `*lsp:<server>:trace*` in the active pane via the vertico picker (Phase 3). No arg = picker over every running instance; arg = pre-filter; single match short-circuits the picker and opens directly. Independent of `:lsp-trace`. |

Why the split: `:lsp-trace` was previously a "toggle + open" action. Re-running it to inspect the buffer was the only way to view records, but that also flipped the toggle off. The pure-toggle form lets you turn tracing on, exercise hover/definition/etc., and read the records without losing tracing state.

### LSP commands routed through the picker (Phase 3 + 4.2)

| Command                   | Source              | Action                                                                       | Short-circuit when 1 match                |
|---------------------------|---------------------|------------------------------------------------------------------------------|-------------------------------------------|
| `:lsp-log [server]`       | `LspInstances`      | `OpenLspLog` -- opens `*lsp:<server>*` in active pane                        | yes (`:lsp-log rust` with one rust ws)    |
| `:lsp-trace-log [server]` | `LspInstances`      | `OpenLspTraceLog` -- opens `*lsp:<server>:trace*` in active pane             | yes                                       |
| `:lsp-server-log`         | `LspInstances`      | `OpenLspLog` -- broad picker over every running actor                        | n/a (no prefilter)                        |
| `gd` / `gD` / `gy` / `gI` | `LspLocations`      | `JumpToLspLocation` -- jump to the picked target via `jump_to_file_line_col` | single-result jumps directly (vim convention) |
| `gr`                      | `LspLocations`      | `JumpToLspLocation` -- references list always shows the picker               | n/a (always picker for ≥1 result)         |
| `:lsp-symbols`            | `LspLocations`      | `JumpToLspLocation` -- merged outline; depth-indented display                | n/a                                       |
| `:lsp-workspace-symbol [q]` | `LspLocations`    | `JumpToLspLocation` -- workspace-scoped fan-out via `LspSupervisor::all_running_handles` | n/a                                       |
| `:diagnostics`            | `LspLocations`      | `JumpToLspLocation` -- severity glyph in marginalia (`[E]/[W]/[I]/[H]`)      | n/a                                       |

The picker walks `LspSupervisor::running_actors()` snapshot under the supervisor lock, builds one row per `(workspace_root, server_id)` pair with marginalia `<buffer_count> bufs  <cap_summary>`, drops the lock, and hands the row vec to `Picker::set_lsp_instances`. Candidate `text` encodes the actor key as `"<server_id>\t<workspace_path>"`; `lsp_key_from_text` parses on accept. The opened buffer goes through `App::open_help_in_pane` (Phase 1) so it lives in `BufferRegistry`, splits, and is reachable via `:bn` / `:b N` / the buffer picker.

### Log buffers as in-pane registry buffers

Persistent log views (`*lsp:<server>*` / `*lsp:<server>:trace*`) and other help-shaped views (`:describe-*`, `:diagnostics`) live in the unified [`BufferRegistry`] (design.md §5.9.8) as `BufferData::Help`. The popup-overlay path remains available for transient surfaces (hover, future doc lookups, error toasts). LSP commands route through the in-pane path via the picker accept dispatcher (Phase 3); the remaining migrations (`:describe-*`, `:diagnostics`) are mechanical -- swap `open_help` for `open_help_in_pane` at each call site -- and follow when a session needs them.

### Live-tail (Phase 4)

Log buffers update **live** as records arrive -- no manual refresh, no buffer reopen. The wiring:

1. `LspLogger::set_event_publisher(closure)` installs a publisher (the App wires this at boot to forward to the runtime [`EventBus`]).
2. Every `LspLogger::log` append fires `Event::LspLogPushed { server_id, level, source, message }` via the publisher (after the ring push, with no logger lock held).
3. The App subscribes a channel to `EventKind::LspLogPushed` and stores the receiver on `App.lsp_log_event_rx`.
4. `App::drain_lsp_log_events` -- called once per main-loop tick (right after `drain_pending_definitions`) -- drains the channel, **coalesces** by scope (subsystem / per-server log / per-server trace), and refreshes only the buffers actually open in `BufferRegistry`.
5. `replace_help_buffer_preserving_cursor` rebuilds the help buffer's rope from a fresh `LspLogger::snapshot_*` while preserving the user's cursor + scroll (clamped to the new content's line bounds). The popup hot-path mirror (`App.help_buffer`) syncs too when it points at the same id.

A burst of 50 records on one server resolves to **one** rebuild per scope per drain (the first-and-last assertion in `lsp_log_burst_coalesces_into_one_refresh` proves this); cheap idle behaviour because the refresh path short-circuits on `BufferRegistry::help_with_title` missing.

Trace records (level == `trace` OR source == `trace`) refresh `*lsp:<server>:trace*`; everything else refreshes `*lsp:<server>*`. `server_id == None` records refresh the subsystem-wide `*lsp*` buffer. The trace-toggle gate in `LspLogger::log` still applies -- `trace` records that don't pass the toggle are dropped before the publisher fires, so disabled-trace servers don't generate refresh churn.

### Configuration surface

Documented in [`../../user/lsp.md`](../../user/lsp.md). Wire-level keys:

```toml
[lsp]
log_level    = "info"
log_capacity = 10000

[server.rust]
log_level = "debug"
trace_io  = true
```

`lattice-lsp` doesn't read these; the App's config layer does
and calls `logger.set_default_level(...)` / `set_server_level(
..., Some(level))` / `enable_trace(...)` at startup.

### Why two pipelines (rings + tracing)

In-memory rings serve buffer-backed log views and survive
without any external subscriber. The `tracing` fan-out lets
power users drive `RUST_LOG`-style filtering, JSON log
shipping, OpenTelemetry, etc. Independent: turning one off
doesn't affect the other.

---

## 7b. Multi-buffer / multi-server topology

Two scenarios deserve explicit treatment:

1. **Multiple buffers, separate servers per language.** The
   user has `main.rs`, `main.py`, `main.go` open
   simultaneously; rust-analyzer / pyright / gopls each run
   as their own actor. `:cnext` while in the Python buffer
   walks Python diagnostics, never the Rust ones.
2. **Multiple servers attached to one buffer.** A `.cpp`
   file with both `clangd` (semantic) and a custom linter
   bridge (style); a `.rs` file with rust-analyzer + a
   separate type-narrowing helper. Both servers publish
   diagnostics, both contribute completions, both can
   answer `goto-definition`.

### How buffer isolation is structural

Every piece of LSP state is keyed by URI or by
`(URI, server_id)`. There's no "active buffer" mutable
register that features rewrite -- queries always pass an
explicit URI down. So "no conflict between buffers" is a
property of the data model, not a discipline each feature
must enforce.

| State | Keyed by | Lives in |
|---|---|---|
| Diagnostics | `(Uri, Arc<str>)` server id | `DiagnosticsLayer` |
| Document mirror + version | `Uri` | `DocSync.docs` |
| Pending change queue | `Uri` | `DocSync.docs[uri].pending` |
| Pending requests | JSON-RPC id (server-scoped) | actor's `pending` map |
| Log records | `Option<Arc<str>>` server id (None = subsystem) | `LspLogger` rings |
| Server actors | `(WorkspaceRoot, server_id)` | `LspSupervisor.actors` |
| Per-buffer attachments | `BufferId` | `LspSupervisor.attachments` |

### Per-buffer navigation: `]d` / `[d` vs `:cnext`

| Command | Scope | Implementation |
|---|---|---|
| `]d` / `[d` | Active buffer only. | `layer.diagnostics_for(active_uri)` -> sort by `(line, char)` -> find next past cursor; wrap. |
| `:diagnostics` | Workspace-wide list (everything-is-a-buffer). | `layer.snapshot()` rendered as `<uri>:<line>:<col> <severity> <message>`. |
| `:cnext` / `:cprev` | Walks `:diagnostics`. Vim-style quickfix; jumps across files. | Reuses the `:diagnostics` buffer's cursor. Wraps. |
| `:diagnostics buffer` | Filtered view: active buffer's URI only. | `snapshot()` filtered by URI. |

Per-pane "last-walked" cursor lives on the `PaneState`, not
the supervisor. Two side-by-side panes on different files
have independent navigation positions even when the underlying
diagnostic list is the same.

### Multiple servers per buffer: per-feature merge strategy

The supervisor keeps
`attachments: HashMap<BufferId, Vec<Arc<ServerHandle>>>`. On
buffer open, every `ServerConfig` whose `file_patterns` match
the buffer's path adds itself to the list. Each attached
server gets its own `DocSync` for that buffer, so didOpen /
didChange / didClose fire per server.

When the editor invokes a feature, the supervisor consults
the buffer's attachments and merges per the table:

| Feature | Merge strategy | Rationale |
|---|---|---|
| **Diagnostics** | Layer keyed by `(uri, server_id)`; readers merge across servers via `diagnostics_for(uri)`. (Shipped, 4.1.d.ii.) | Different servers report different problem classes -- semantic vs lint vs spell-check. Show all. |
| **Hover** | `futures::join_all` over attached servers; non-empty responses concatenated, each prefixed with the `server_id`. | Each server may know different things (rust-analyzer knows types, a doc-bridge knows examples). |
| **Goto-definition / declaration / typeDefinition / implementation** | Race to first response; if empty, fall through. Server priority breaks ties. | Definition is single-valued. |
| **References** | Union from every server's response, deduped by `(uri, range)`. | Same reference may show from semantic + syntactic servers. |
| **Document symbols / Workspace symbols** | Union, deduped by `(name, kind, range)`. | -- |
| **Completion** | Each server registers as `gen:lsp:<server-id>` in `lattice-completion`'s pipeline. Score-merging across generators. | The completion engine's existing seam. |
| **Signature help** | First non-empty wins. | Signatures are usually language-specific; merging rarely useful. |
| **Code actions** | Union; each picker entry prefixed `[server-id]`. Captured `server_id` routes resolve / execute back. | User picks; resolve must go to the right server. |
| **Rename** | Each server returns a `WorkspaceEdit`; supervisor merges. Conflicts (same URI + range from two servers) -> keep higher-priority server, log Warn. | Multi-server rename is rare; conflict resolution is a fail-safe. |
| **Formatting** | Single winner: highest-priority server with `documentFormattingProvider` advertised. | Two formatters can't agree on whitespace. |
| **Semantic tokens** | Single server per buffer (highest-priority with `semanticTokensProvider`). Multi-server merging deferred until a real use case appears. | Token-stream merging across servers is hard (overlap rules vary). |
| **Inlay hints** | Union; each hint carries server provenance for tooltip resolution. | Hints rarely conflict on the same anchor; both render if they do. |
| **Folding ranges** | Union deduped by `(start_line, end_line, kind)`; merges with tree-sitter's fold provider via existing `FoldMethod` priority. | Independent sources usually agree on natural blocks; dedup is cheap. |

### Server priority

Single integer per `ServerConfig`. Used by formatting, rename
conflict resolution, and "race to first" features where
multiple non-empty responses tie.

```toml
[server.rust]
priority = 100  # higher = wins ties

[server.clippy-bridge]
priority = 50
```

Default priority 100 if unset. Lattice's curated registry
sets sensible priorities so users typically don't touch them.

### Cancellation in multi-server

Each request to a server gets its own `CancellationToken`.
Superseding (cursor moves -> stale hover request, etc.)
cancels every in-flight request for that buffer; per-server
`$/cancelRequest` notifications fly out in parallel; the
local `Pending<T>`s resolve with `LspError::Cancelled`.
The supervisor's superseding logic doesn't care which servers
are attached -- it cancels them all and issues fresh
requests.

### Crash isolation

A crashed server affects only its own actor + its own
entries in `DiagnosticsLayer` (cleared via
`layer.clear_server(&id)`). Other servers attached to the
same buffer keep working. Supervisor restart-with-backoff
replays `didOpen` only for that server's attachments.

### Why this falls out cleanly

`lattice-lsp`'s primitives are intentionally per-server
(actor + handle + bus + DocSync). Multi-server is a
supervisor concern, not a primitive concern. Adding a
second server for the same language doesn't change any
of the lower layers -- it adds another actor, another
attachment entry, and feature dispatch fans out one
more way.

---

## 8. Crash recovery (4.1.b sketch; full impl 4.4)

Today's actor handles a clean shutdown but doesn't auto-restart
on an unexpected pipe close. The supervisor that wraps the
actor with restart-with-backoff lands in 4.4 alongside the
file-watcher work (which needs a stable supervisor anyway).

Sketch:

1. Detect `read_loop` ending with `Err(_)` or `Ok(None)` while
   the actor isn't in `shutting_down`.
2. Drain pending with `LspError::ActorGone`.
3. Apply backoff: 100ms, 200ms, 400ms, ..., max 5s.
4. Respawn the child + actor with the same `ServerConfig`.
5. Re-issue `didOpen` for every URI the supervisor was
   tracking.

The supervisor lives outside `lattice-lsp` (probably in
`lattice-runtime` or a new `lattice-lsp-supervisor` crate)
because it depends on knowing what the editor expected.

---

## 9. Testing

The `tests/common/mod.rs` fixture spins up an in-process
mock server over a `tokio::io::duplex` pipe. Tests configure
canned per-method responses via `mock.on(method, |params|
...)` and push server-initiated messages via
`mock.push_notification` / `mock.push_request`. The fixture
captures every request / notification / response the actor
sent, exposed via async snapshots.

Why an in-process mock instead of running a real
rust-analyzer:

- **Determinism.** Real servers have indexing time, capability
  variations, and version-specific quirks. The mock is fully
  controlled.
- **Speed.** ~5s for the full integration suite vs ~30s for a
  single rust-analyzer round-trip.
- **No PATH dependency.** Tests run on any machine without
  the language toolchain installed.

A separate opt-in CI job will run integration tests against
real `rust-analyzer` once the foundation is feature-complete
(4.2 / 4.3); those cover behaviours we can't model in the
mock (indexing latency, real diagnostics shape, capability
quirks).

### Bench discipline

The `benches/lsp.rs` cases all sit in Background-class
budgets. The intent is to prove the wire layer never shows up
next to editor work in a flame graph. When a new feature lands,
add a bench for the hot path it introduces.

---

## 10. Editor integration

The `App` holds an `Arc<Mutex<LspSupervisor>>` that maintains:

```text
LspSupervisor
├── Vec<Arc<ServerConfig>>                          -- registry (builtin + user overrides)
├── HashMap<(WorkspaceRoot, ServerId), ServerHandle> -- live actors
├── HashMap<Uri, Vec<(WorkspaceRoot, ServerId)>>     -- which actors care about each URI
├── HashMap<(WorkspaceRoot, ServerId), SubscriptionId> -- per-actor fan-in subs (§11)
├── DiagnosticsLayer                                -- per-URI latest diagnostics
├── Option<Arc<EventBus>>                           -- editor event bus
├── Option<ApplyEditBus>                            -- server-initiated workspace/applyEdit
└── Option<ConfigurationBus>                        -- server-initiated workspace/configuration
```

The supervisor no longer stores per-actor `DocSync`. Each
actor owns its own mirror; the supervisor only knows
*attachment* (which actor cares about which URI) and the
fan-in subscriptions it has to unsubscribe at shutdown.

**Buffer open.** Walk the matching `ServerConfig`s. For each
`(workspace, server_id)` actor: spawn if absent, then call
`ServerHandle::open_doc(uri, language_id, text)` -- the actor
populates its own `DocSync` and emits `didOpen`. New actors
also get a per-actor fan-in subscription (§11).

**Buffer edit.** The dispatcher commits → the App publishes
`Event::DocumentChanged` on the bus carrying every applied
edit (range + inserted text). Each actor's fan-in task picks
up the event and forwards one `ActorCmd::RecordEdit` per
edit straight into the actor's mailbox. The actor coalesces
on a 50 ms debounce and emits a single `didChange`. The UI
thread takes no LSP-related lock.

**Buffer close.** `ServerHandle::close_doc(uri)` -- the actor
flushes any queued changes and emits `didClose` from inside
its loop.

**Server crash.** Today the supervisor logs and removes the
actor; auto-restart + `didOpen` replay lands in 4.4.

---

## 11. Edit-path architecture

The hot edit path is the architecture's tightest constraint:
the UI thread must commit a keystroke within one frame, <8.3 ms p99 (design.md
§8). LSP synchronisation cannot show up on that critical
path. The pipeline below keeps it off entirely.

```
              UI thread                |          tokio runtime
                                       |
keystroke                              |
   │                                   |
   ▼                                   |
apply_edit_blocking(edit)              |
   │  block_on(document.apply_edit)    |
   ▼                                   |
publish_document_changed(applied[])    |
   │  EventBus::publish — single mutex |
   │  on the bus's per-kind bucket     |
   ▼                                   |
event_bus  ────────────────────────────┼─►  per-actor fan-in task (lattice_lsp::fan_in)
                                       |        │  on Event::DocumentChanged:
                                       |        │     for each applied edit:
                                       |        │       ServerHandle::record_edit(uri, edit)
                                       |        │         (mpsc send on actor cmd_tx)
                                       |        ▼
                                       |    actor task (single writer)
                                       |        │  ActorCmd::RecordEdit:
                                       |        │    DocSync::record_edit (own the mirror)
                                       |        │    flush_pending = true
                                       |        │    flush_sleep.reset(now + 50ms)
                                       |        ▼
                                       |    debounce arm fires:
                                       |        DocSync::take_flush_all_payloads
                                       |        out_tx.send(didChange)  -> write_loop -> server
```

### Why this shape

The pre-refactor path took the supervisor mutex on every
keystroke (`App::lsp_record_edit`). When contended (during
a 50 ms App-spawned flush task that held the lock while
writing `didChange` to every server), edits were silently
dropped via `try_lock`. The mirror diverged from the wire
stream; diagnostics, hover, completion, and definition all
became incorrect with no audible failure.

The new path:

- **No shared lock on the hot path.** Publishing on the bus
  takes a brief mutex on the bus's per-kind bucket
  (microseconds). The fan-in receives on its own channel.
  `ServerHandle::record_edit` is an unbounded mpsc send.
  Nothing the UI thread does waits on the actor.
- **Single writer per actor.** Only the actor task mutates
  its `DocSync`. There is no contention to drop edits to.
- **FIFO with `OpenDoc`.** Both flow through the same
  `cmd_tx`, so a buffer's open precedes its first edit on
  every actor that cares about it.
- **Per-actor debounce.** Each actor coalesces on a 50 ms
  idle window via a pinned `tokio::time::sleep` inside its
  `select!`. No App-side timer; no supervisor-wide lock.

### Subscription lifecycle

Per-actor subscriptions are stored in
`LspSupervisor::fan_in_subs` keyed by the same `ActorKey`
the actor uses. Three lifecycle moments:

- **Actor spawn.** `LspSupervisor::open_buffer` and
  `attach_handle` spawn a fan-in via
  `lattice_lsp::fan_in::spawn(handle, bus)` whenever a new
  actor lands in `actors`. The returned `SubscriptionId`
  goes into `fan_in_subs`.
- **Actor crash / drop.** The fan-in detects
  `LspError::ActorGone` from a `record_edit` send and
  exits, dropping its sender. The bus prunes the dead
  subscription lazily on the next publish that hits its
  bucket.
- **Editor shutdown.** `LspSupervisor::shutdown` drains
  `fan_in_subs` and calls `EventBus::unsubscribe` for each
  before dropping the actors. This stops the bus from
  briefly holding a sender pointing at an actor on its way
  out.

### What about scratch / unsaved buffers

`Event::DocumentChanged.path` is `None` for buffers without
an on-disk path. The fan-in skips these (no URI to map). LSP
features only apply to file-backed buffers, so this is the
correct behaviour -- the actor never tracks a URI it didn't
open.

### Cost model

Per applied edit (one publish):

- One `EventBus::publish` -- O(N) over `N = #subscribers on
  the DocumentChanged bucket`. Each subscriber is a single
  `mpsc::send`. Typical N is `1 + #LSP actors`, which in
  practice is 1-3.
- One mpsc receive + URI build + one `record_edit` send per
  attached actor.
- Inside each actor: one `DocSync::record_edit` call (string
  splice + content-change push).
- Debounce timer reset (cheap pinned sleep reset).

The `didChange` write itself is amortised over the debounce
window. Bench numbers: see [`../operations/benchmarks.md`](../operations/benchmarks.md)
under the `lsp_edit_publish_*` rows.

---

## 12. Async-result render-wake

§11 covers the *edit* path (UI → server). This section covers the
*result* path: how an async LSP arrival that changes render-relevant
state gets a frame on screen **without a keystroke in flight**.

### The contract

> Every async LSP arrival that changes render-relevant state MUST fire
> `Editor::async_landed`. Nothing else is required of the arriving
> task -- the actor turns one `notify_one()` into a full
> `run_tick_pending` + `publish_render_state` + cells/overlay wake.

`async_landed` is an `Arc<tokio::sync::Notify>` seated on `Editor`
(`editor_boot.rs`). The editor actor's `run_actor` loop
(`editor_actor.rs`) selects over `cmd_rx` (keystrokes / effects) and
`async_landed.notified()`:

```rust
_ = async_landed.notified() => {
    let signals = editor.run_tick_pending();  // drain caches + event-bus channels
    editor.publish_render_state();            // rebuild snapshot + fire cells_wake
    editor.event_bus.publish_typed(AsyncRenderStatePublished);
    for sig in signals { let _ = signal_tx.send(sig); }
    continue;
}
```

`run_tick_pending` drains the per-feature caches and the event-bus
channels; `publish_render_state` rebuilds the snapshot and wakes the
cells/overlay worker, which fires `paint_request` only on a
content-changing `WorkerDecision`. The `AsyncRenderStatePublished`
event re-wakes cells *after* the `ArcSwap` store, so there is no
read-before-write race. All of this runs on the single-writer actor
thread, never the UI thread (paramount #1).

This is the SAME mechanism a tree-sitter reparse uses: the
`SyntaxHandle` is seeded with a closure that fires
`async_landed.notify_one()` on reparse (`dispatch.rs`). That is why
tree-sitter recolour appears on idle but LSP semantic-token recolour
historically did not (see "the historical gap" below).

### The arrival shapes

| Shape | Examples | Wake wiring |
|---|---|---|
| **Direct-write request task** | `semanticTokens`, `inlayHint`, `foldingRange`, `codeLens`, `documentColor`, `documentLink`, `documentHighlight`, `textDocument/diagnostic` (pull) | The task is spawned on the LSP runtime, writes its result into a `PerBufferCache` via `insert_for`, then **must** `async_landed.notify_one()`. |
| **Channel-delivered action result** | `references` (`gr`), `definition`/`declaration`/`typeDefinition`/`implementation` (`gd`/`gD`/`gy`/`gI`), `hover` (`K`), `signatureHelp`, `completion`, `document`/`workspace` `symbol`, `codeAction` + resolve/apply, `format`, `rename`, `callHierarchy`, `typeHierarchy`, `moniker`, on-type formatting, `selectionRange`, `executeCommand` | The task is spawned on the LSP runtime and delivers its result over an `mpsc` channel drained by a `drain_pending_*` inside `run_tick_pending`. After **each** terminal `tx.send(...)` it **must** `async_landed.notify_one()` (every arm — `NoServers`, `Found`, empty, error). This is the single renderer-agnostic wake: the actor arm turns it into drain + publish + `paint_request` for BOTH peers. Slice AW.1. |
| **Event-bus arrival** | `$/progress`, `publishDiagnostics`, `*/refresh`, log records | A forwarder task subscribes the typed event and fires `async_landed.notify_one()` on each delivery so `run_tick_pending`'s drain runs off-keystroke. Template: the `MultibufferExcerptsReady` forwarder in `editor_boot.rs`. |

> **Do NOT reach for a renderer-specific `paint_request` clone.** `hover` /
> `signatureHelp` historically fired the actor's `paint_request` directly (the
> "X1b" popup-overlay shape). That is wrong: `paint_request` only *repaints*.
> The GPUI peer's paint bridge happens to also call `run_tick_pending`, so the
> popup drained there — but the TUI peer's `Wake::Repaint` redraws WITHOUT
> draining (X1 removed the per-frame `run_tick_pending`), so the popup waited
> for the next keystroke. Every channel-delivered result — popup, picker, or
> jump — fires `async_landed`, and the actor arm (which re-fires `paint_request`
> when a `paint_revision` surface like the popup moves) drives both peers. The
> wake is a **core** concern, not a renderer concern.
>
> This is not LSP-specific. The same rule fixed the **picker** paths
> (`open_picker` init future, `fire_live_picker_query_changed` live re-query,
> and the `bump_live_picker_debounce` *timer* wake), which had the identical
> latent TUI bug (AW.3). Once every async drain-path fires `async_landed`, the
> actor's `async_landed` arm is the **single** drain chokepoint, so the GPUI
> paint bridge was simplified to a pure repaint forwarder (`paint_request` →
> `cx.notify()`) — it no longer runs its own `run_tick_pending`. Both peers are
> now pure consumers of published `RenderState`; the drain lives in exactly one
> renderer-agnostic place. Workers (cells / overlay / virtual-rows) still fire
> `paint_request` directly — that is correct: they publish their content
> *before* firing and need only a repaint, not a channel drain.

### The historical gap (the bug L1 closes)

Before L1, only the popup-overlay shape fired any wake. The
direct-write request tasks wrote their caches silently, and the
event-bus arrivals sat in their channels undrained. Consequence:
semantic-token colour, inlay hints, and `$/progress` were all
invisible until the next keystroke happened to run `run_tick_pending`
+ publish. The `render-thread-discipline-remediation.md` doc that once
tracked this (the deferred "X1b") now lives in `docs/dev/archive/`
(a stale comment in `lattice-ui-tui/src/runtime.rs` still points at its
old path); this section is its permanent home. The L1 fix fired
`async_landed` from the **direct-write** and **event-bus** shapes and is
sequenced in
[`../operations/slice-plans/archive/lsp.md`](../operations/slice-plans/archive/lsp.md)
(slice L1).

### The remaining gap: channel-delivered action results (slice AW.1)

L1 left the **channel-delivered action result** shape unfixed — the row
added above. Every one-shot user request (`gr` references, `gd`/`gD`/`gy`/
`gI` nav, `K` hover, code-actions, rename, format, symbols, signature,
completion, call/type hierarchy, moniker, selection-range) delivers over an
`mpsc` channel drained by a `drain_pending_*` in `run_tick_pending`, but
fired **no** wake (except `hover`, which fired the renderer-specific
`paint_request` clone — see the callout above). So the picker / jump / popup
waited for the next keystroke. "Often, not always" for `gr`: an unrelated
`async_landed` (a syntax reparse, a diagnostics push) landing in the window
would drain the pending result as a side effect. Slice **AW.1** fires
`async_landed` after every `tx.send` on these paths (and converts `hover`
from `paint_request` to `async_landed`), making the wake uniform across all
arrival shapes and both renderers. Sequenced in
[`../operations/slice-plans/archive/lsp-async-wake.md`](../operations/slice-plans/archive/lsp-async-wake.md).

> **`App::apply`-tail `run_tick_pending` (audit, AW.5).** The TUI/GPUI
> keystroke path also runs `run_tick_pending` at the tail of `App::apply`
> (`app/dispatch.rs`). That is a *synchronous fast-path*: a result already
> back by the time the firing keystroke finishes drains on the same
> round-trip (latency win). It is correct to keep — but it must **never** be
> the ONLY drain path for a result. After AW.1 it is not: every async LSP
> result also fires `async_landed`, so an idle arrival (no keystroke in
> flight) drains via the actor arm. If you add a new async result whose only
> drain is the `App::apply` tail, you have re-introduced the AW.1 bug.

### The second gap: a publish must REQUEST a frame (slice L6)

Firing `async_landed` only gets the arriving data *into* `RenderState`
(via `run_tick_pending` + `publish_render_state`). It does **not** by
itself put a frame on screen. Off-keystroke, the only repaint triggers
are the **cells worker** and the **virtual-rows worker**, and each fires
`paint_request` *only* on a content-changing `WorkerDecision`
(`Recomputed` / `Clear`), never on `CacheHit`. So a change confined to a
surface those workers do **not** own -- the modeline LSP-readiness badge
(`lsp ⟳ … %` / `lsp ✓`), a diagnostics **overlay** (gutter / underline /
inline summary), a popup, the minibuffer -- updated `RenderState` but
never repainted until the next keystroke forced an unconditional redraw.
That is the "the `lsp` progress badge / a diagnostic only updates when I
press a key" report: a direct paramount-#4 (asynchronicity) / #1
(no keystroke coupling) violation. The same class explains
"undo → diagnostic disappears": the server re-publishes after the undo's
`didChange`, `DiagnosticsLayer::apply` fires `async_landed`, the publish
lands the diagnostics in the snapshot -- but the gutter/underline are
overlays, so the cells worker returns `CacheHit` and nothing repaints.

The contract's second half:

> A real `publish_render_state` that changes a render-visible surface NOT
> owned by the cells / virtual-rows workers MUST request a paint.

Mechanism: `build_render_state` stamps `RenderState::paint_revision` -- a
content hash over exactly that "everything else" set: the overlay sources
(`lattice-cells/src/version.rs` enumerates them: diagnostics, cursor,
selection, hlsearch, doc-highlights, search matches) plus the chrome the
renderer paints straight from `RenderState` (modeline badge + element
content, minibuffer, popups, echo line, tabs, lifecycle). It is the dual
of the cells `MatrixVersion` (which gates *cell* rebuilds). Identity-
preserving sub-states fold by `Arc` pointer; the rest by value/version,
so a **no-op publish yields a stable revision**. `publish_render_state`
returns whether the revision moved; the actor's off-keystroke arms
(`async_landed`, the inline-diag timer) fire `paint_request` when it did.

Two design points worth their own line:

- **The keystroke path is untouched.** It already repaints on the input
  wake, so firing `paint_request` there would be redundant -- worse, it
  would drive the GPUI paint bridge (`paint_request` →
  `run_tick_pending` → re-publish) into a full extra `build_render_state`
  per keystroke. Gating the paint on *the actor's async arms* (not on
  `publish_render_state` itself) keeps the hot path clean; gating on
  *revision change* keeps the bridge from spinning (a no-op re-publish
  yields the same revision → no further paint).
- **Pointer identity must be meaningful.** `ModelineService::update` /
  `clear` were unconditional `rcu` stores, so the content `Arc` churned
  every publish (`sync_diff_modeline_element` re-applies the diff element
  each `build_render_state`). They are now equivalence-gated, so the
  content pointer is a true "did the modeline change" signal -- and the
  store stops re-allocating an identical map every frame.

---

## 13. Server lifecycle state

Readiness today is implicit: a `ServerHandle` existing means
`initialize` succeeded. There is no first-class "starting / indexing /
ready / failed" signal, so nothing downstream can name *when* a server
became usable, and the user has no visible readiness cue.

### The state machine

```
Spawning ─▶ Initializing ─▶ Ready ──▶ Indexing(%) ──▶ Ready
   │             │            │
   └─────────────┴────────────┴──▶ Failed { reason } ──▶ (restart) Spawning
```

| State | Meaning | Source signal |
|---|---|---|
| `Spawning` | child process spawned, handshake not begun | actor spawn |
| `Initializing` | `initialize` request in flight | handshake start |
| `Ready` | `initialized` sent; capabilities negotiated | handshake complete |
| `Indexing { title, pct }` | a work-done progress token is active | `$/progress` begin/report |
| `Failed { reason }` | handshake or pipe failed | `HandshakeFailed` / `ActorGone` |

`Indexing` is a *projection* of the existing `$/progress` accumulator,
not a separate signal -- when the highest-priority active token ends,
the server falls back to `Ready`. The state lives on the supervisor
(per `(workspace, server_id)`, which already owns actor lifecycle),
published wait-free via an `ArcSwap` snapshot; a typed
`LspServerStateChanged` event fires on each transition (and thus
repaints via the §12 wake).

Why a real state machine, not status-line inference: the readiness
edge ("server just became Ready / finished indexing") is the trigger
for re-requesting semantic tokens / inlay hints / diagnostics on a
freshly-indexed server. Inferring it from token disappearance is lossy
and racy (heuristic #1 -- the genuinely-better primitive).

---

## 14. Status surfaces

Two consumers of §13's state.

### Status line

`LspMode::status_line_items` emits the per-server state as one compact
segment: a state glyph + server id + (when indexing) percentage --
e.g. `rust ⟳ 45%`, `rust ✓`, `rust ✗`. The glyph palette degrades per
the icon-palette rule (Nerd Fonts when `ui.nerd_fonts=on`, BMP
fallback `⟳ ✓ ✗` otherwise; equal cell width). The two LSP segments
that exist today (`LspMode`'s `[lsp]` badge + `LspProgressMode`'s
percentage) collapse into this single state-driven segment: progress
becomes the `Indexing` projection, so there is ONE LSP segment.

Convention: the status line is where Vim (lualine `lsp_status`), Helix
(`◍`), and Zed (bottom-right) all surface LSP readiness. This is the
muscle-memory home; it is NOT the headerline -- that rule
(`async_buffer_status_in_headerline`) governs async-*buffer*
providers (§5.9), not global subsystem readiness.

### `:lsp-status` buffer

`help_views::lsp_status_help` already renders one block per running
actor (id, workspace root, encoding, capability bools, subscriber
count). It gains the lifecycle-state line and, when present, the
active progress title + percentage. It stays a read-only registry
buffer; live-refresh reuses the log-buffer live-tail wiring
(drain-on-event) so an open `:lsp-status` repaints as servers
transition -- no reopen.

---

## 15. Diagnostics presentation

Diagnostics have four surfaces; the first two ship, the last two are
L4.

| Surface | Status | Shape |
|---|---|---|
| Gutter severity column | ✅ | most-severe glyph per line, themed |
| Inline underline | ✅ | per-range coloured underline |
| **Inline end-of-line summary** | L4 | cursor-line only, idle-gated trailing virtual text |
| **Cursor popup (full detail)** | L4 | `gl` opens a `CursorAnchored` float |

### Inline end-of-line summary

A one-line summary of the cursor line's diagnostics, rendered as
trailing virtual text at end-of-line, reusing the inlay-hint
virtual-text span substrate (`splice_virtual_text_into_spans`, §4.4.g
in the feature matrix). Default scope is the **cursor line only**
(Helix's `cursor-line` model), painted ~300 ms after the cursor lands
on a new line, suppressed in Insert mode. Summary text = the
most-severe diagnostic's message (truncated) + `+N` when the line
carries more. Option-gated:

```toml
ui.diagnostics.inline              = "cursor-line"   # off | cursor-line | all
ui.diagnostics.inline-min-severity = "hint"          # error | warning | info | hint
```

`all`-line mode is a follow-on slice (L5); cursor-line is the
low-noise default that respects the keystroke UX contract -- only the
cursor line's eol changes, and only on idle. Cursor movement (not an
edit) repainting its own line's annotation is the intended,
convention-matching behaviour.

### Cursor popup (full detail)

`gl` (Normal mode) opens a `CursorAnchored` popup
(`PopupPlacement::CursorAnchored`, already defined in
`lattice-core::ui::popup`) listing **every** diagnostic on the cursor
line -- severity glyph, full message, `source`, `code`,
related-information count. Reuses the hover popup's render + placement +
dismiss-on-Esc pipeline. `]d` / `[d` additionally echo the landed
diagnostic's message to the minibuffer.

Ownership: the binding AND the handler body live in
`lsp-diagnostics-mode` (`feedback_mode_owns_its_surface`) -- keymap at
`KeymapLayer::MinorMode(lsp-diagnostics)`, handler registered as a
closure bound to the mode's `ActionId`. No host `Action` variant, no
`Editor::do_*` in the dispatcher.

---

## 16. Mode ownership of the nav surface (the `Effect::Lsp` boundary)

The seven LSP **navigation** chords -- `K` (hover), `gd` (definition),
`gD` (declaration), `gy` (typeDefinition), `gI` (implementation), `gr`
(references), `gx` (follow documentLink) -- are owned by `lsp-mode`:
both the binding (`LspMode::keymap()`, scoped by K.1.c to
lsp-attached buffers) AND the handler body (`LspMode::action_handlers()`
closures). This is L4b's `gl` pattern (§15) generalised from one
synchronous chord to the seven async ones.

### The complication: nav is async, `gl` was not

`gl` reads the local diagnostics layer synchronously and returns
`Effect::ShowDiagnosticsPopup` in one call. The nav chords instead fire
an **async** LSP request (hover → popup, definition → jump, references →
picker) whose result lands dozens of milliseconds later, off-keystroke.
A handler closure has only a borrowed `&ActionContext` (cursor, buffer,
services, events) -- it cannot drive the request, because the request
substrate is irreducibly `&mut Editor`: popup State-A/B focus
promotion, the sub-mode gate, URI resolution, UTF-8→UTF-16 position
conversion, the cancellation-token lifecycle, the result channel + the
popup anchor stash, `spawn_on_lsp_runtime`, and the `paint_request`
wake. Plus the off-keystroke `drain_pending_*` that opens the
popup / jumps / opens the picker when the response arrives.

### The split: mode decides, host executes

That substrate is **legitimate shared host machinery** -- the direct
analogue of the cells / overlay workers (§12, `display-line.md`):
generic execution that modes *trigger* but do not *own*. By the
substrate-vs-mode-helper rule (CLAUDE.md), its consumer is the generic
Effect-apply + actor drain loop, which is uniform host machinery, so it
stays host. The mode owns only the **decision** ("fire request X"),
expressed by returning a host-owned Effect:

	// lattice-grammar -- pure data, no lsp_types, no lattice-lsp dep.
	pub enum LspRequest {
		Hover, Definition, Declaration, TypeDefinition,
		Implementation, References, FollowLink,
	}
	// one new Effect arm carrying the request kind.
	Effect::Lsp(LspRequest)

Flow:

	gd
	  → LspMode::keymap()                "action:lsp-definition"   (mode-owned)
	  → ActionHandlerRegistry intercept  → LspMode::action_handlers() closure
	  → Some(Effect::Lsp(LspRequest::Definition))
	  → host apply_effect_host           → editor.lsp_request(req)   (host substrate dispatcher)
	  → editor.lsp_nav_request(Definition)  spawn + token + anchor   (unchanged)
	  → drain_pending_definitions()      off-keystroke: jump / picker (unchanged)

The handler reads no position -- the substrate reads live `Editor`
state (`self.cursor`, `self.scroll`) exactly as it does today, so the
anchor pins to the symbol the chord fired on. `Effect::Lsp` is
host-applied: both renderers list it on the host-handled no-op branch
of their effect classifiers (parity with `ShowDiagnosticsPopup`), and
it is non-mutating / non-yanking. `LspRequest::FollowLink` is the one
nav that returns `RendererSignal`s synchronously (it opens a buffer or
delegates to the OS handler); `lsp_request` returns those and the apply
arm extends `out.renderer_signals`.

### Why `Effect::Lsp`, not seven Effects (option (c) over (a))

A single data-carrying `Effect::Lsp(LspRequest)` -- not seven
per-method Effect arms -- is the form that makes the acid test pass
cleanly: a future LSP method (or a future provider crate's LSP-ish
chord) adds one `LspRequest` **data** arm, *not* a new Effect variant,
*not* a new host `Action` variant, *not* a new `Editor::do_*` bound to a
chord. It also collapses the effect-classifier threading both renderers
carry from seven arms to one. This is the Effect-vocabulary-as-host-
boundary principle (`feedback_effect_vocabulary_is_host_boundary`): the
Effect enum is the host-owned typed seam for the plugin/replay/async
boundary, so encoding the request *as data inside one Effect* is the
right grain.

### Rejected: mode-side async (option (b))

Driving the async lifecycle inside the mode (spawn / cancel via the
M-async Guard machinery, drain mode-side) was rejected on heuristic #1:
it rewrites proven substrate for no merit win and would either duplicate
the spawn/anchor/wake plumbing (a second path that can silently miss the
`paint_request` wake -- the L1/L6 off-keystroke-repaint bug class, a UX
regression with no user upside) or force the deep `&mut Editor` request
state out through a large new service surface. The async request
machinery is shared substrate, not mode-specific behaviour.

### What stays host (and why that is not a half-migration)

`editor.lsp_request` + `lsp_hover_request` / `lsp_nav_request` /
`lsp_references_request` / `do_lsp_follow_link_at_cursor` + the
`drain_pending_*` family remain host substrate. The seven
`register_action` registrations keep a dead `Effect::None` apply (the
handler intercepts first) purely so the `CommandId` resolves for the
chord binding + handler registration -- the `snippet-expand` / `gl`
shape. The mode owns the full *decision surface* (binding + handler
body); the host owns the *generic execution*. That is the documented
correct split, not a half-migration: a half-migration would leave the
binding in the mode while a host `Editor::do_*` stayed bound to the
chord -- which is exactly what this slice removes.

See the slice plan (L7) for sequencing.

---

## See also

- [design.md §5.4](design.md) -- canonical design.
- [`../operations/slice-plans/archive/lsp.md`](../operations/slice-plans/archive/lsp.md) -- slice plan (sequencing + status for §12-§15 work).
- [`../notes/lsp-features.md`](../notes/lsp-features.md) -- per-method feature matrix.
- [`../../user/lsp.md`](../../user/lsp.md) -- user help.
- [`../operations/benchmarks.md`](../operations/benchmarks.md) -- bench numbers (LSP rows
  added when feature benches land).
