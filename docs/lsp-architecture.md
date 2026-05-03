# LSP Architecture (developer reference)

This document is the implementer-side companion to
[DESIGN.md §5.4](DESIGN.md). DESIGN.md is the terse,
principle-led canonical text; this is the longer-form "how it
actually works", with concrete pointers into the
`lattice-lsp` crate.

User-facing help lives in
[`help/lsp.md`](help/lsp.md). The feature
tracking matrix lives in
[`lsp-features.md`](lsp-features.md).

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
    Request {
        method: String,
        params: Option<Value>,
        reply: oneshot::Sender<LspResult<Value>>,
    },
    Notify { method: String, params: Option<Value> },
    Cancel { id: i64 },
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

`DocSync::open(uri, language_id, text)` →

- Send `didOpen` with `version=1`.
- Store a `String` mirror of the text + the language id and
  current version.

`DocSync::record_edit(uri, edit)` →

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

`DocSync::flush(uri)` →

- Read the negotiated `TextDocumentSyncKind`:
  - `Incremental` → send queued events as one `didChange`.
  - `Full` → drop queued events; send the entire mirror as
    one change with no range.
  - `None` → clear the queue; emit nothing.
- Bump the wire-side version (matches mirror version).

`DocSync::close(uri)` flushes pending then sends `didClose`
and removes the mirror.

The mirror is a `String` (not a `Rope`) because the LSP layer
only ever splices one contiguous region per edit; per-line
indexing is rare and bounded by line count.

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

### Configuration surface

Documented in [`help/lsp.md`](help/lsp.md). Wire-level keys:

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

## 10. Editor integration (preview)

The `App` will hold a `LspSupervisor` that maintains:

```text
LspSupervisor
├── HashMap<(WorkspaceRoot, ServerId), Arc<ServerHandle>>
├── HashMap<BufferId, Vec<Arc<ServerHandle>>>   -- which servers care about this buffer
├── HashMap<BufferId, DocSync>                  -- per-buffer sync state
├── DiagnosticsLayer                            -- per-URI latest diagnostics
└── Vec<Arc<ServerConfig>>                      -- the registry (builtin + user overrides)
```

Buffer open: walk to the matching `ServerConfig`s, ensure each
`(workspace, server_id)` actor exists (spawn if not), call
`DocSync::open` for each. Buffer edit: actor commits → App
calls `DocSync::record_edit` for each attached server →
flush on idle (50ms). Buffer close: `DocSync::close` for
each. Server crash: supervisor restarts; re-`didOpen`s every
attached buffer.

This integration ships in 4.1 follow-ups (4.1.d.ii–iv) and
4.2 (when navigation features need it for definition jumps).

---

## See also

- [DESIGN.md §5.4](DESIGN.md) -- canonical design.
- [`lsp-features.md`](lsp-features.md) -- feature matrix.
- [`help/lsp.md`](help/lsp.md) -- user help.
- [`BENCHMARKS.md`](BENCHMARKS.md) -- bench numbers (LSP rows
  added when feature benches land).
