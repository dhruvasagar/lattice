# AI agent protocol — implementation plan (slice AI‑1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up `lattice-ai` as an ACP client that spawns an agent
subprocess (opencode), completes the `initialize → session/new → session/prompt`
handshake, and streams the agent's `session/update` reply text into the
`*messages*` buffer — driven by `:opencode` / `:ai-prompt`.

**Architecture:** A clone-able `AiClientHandle` drives an idle tokio supervisor
(mirroring `lattice-claude-code`'s `server.rs`) that spawns the provider process
and wires its stdio to an ACP JSON-RPC connection actor. All protocol I/O is
off the editor thread; the editor thread only sends commands (channel) and reads
a wait-free `ArcSwap<AiState>` snapshot. Agent output reaches the editor through
a `SubsystemBoot::inbound` bus whose handler emits `tracing` events into
`*messages*`. Wire types come from Zed's official `agent-client-protocol` crate.

**Tech Stack:** Rust, tokio (process, io-util, sync), the `agent-client-protocol`
crate, serde/serde_json, tracing, the lattice `SubsystemBoot` / `InboundBus` /
`Effect` / `CommandRegistry` seams.

## Global Constraints

- Mode-ownership: the crate adds **zero** `Editor::` methods and **zero** new
  `Effect` / `Action` variants. All host contact is through the existing
  `SubsystemBoot` surface and existing `Effect` variants (`Echo`,
  `OpenMessages`). (`ai-agent-protocol.md` §2.)
- One Phase-B boot line only: `lattice_ai::install(&mut boot)`.
- Ex-commands are registered **bare + dashed** (`opencode`, `ai-prompt`,
  `ai-stop`), resolved via `id_by_name`; each `apply` captures the
  `AiClientHandle`. (Mirror `lattice-claude-code::commands`.)
- All protocol I/O off the editor thread; editor thread does channel-send +
  wait-free snapshot read only.
- `edition`, `rust-version`, `license`, and `[lints] workspace = true` inherited
  from the workspace (copy `lattice-claude-code/Cargo.toml`'s `[package]` block
  verbatim, changing only `name`).
- AI‑1 assumes the provider is **pre-authenticated** (`opencode` already logged
  in / `claude login` done out of band). The in-protocol `authenticate` flow is
  AI‑4. (`ai-agent-protocol.md` §5b.)
- TDD: every task is red → green → commit. Unit tests use `tokio::io::duplex`
  in-memory streams + a scripted mock agent; no test spawns a real subprocess
  except the one explicitly `#[ignore]`-gated integration test.

## Slice roadmap (this feature)

| Slice | Deliverable | Status |
|-------|-------------|--------|
| **AI‑1** | ACP transport + session skeleton (opencode), reply text → `*messages*` | 🚧 this plan |
| AI‑2 | Shared diff review — `request_permission` + diff ↔ `ProgrammaticDiffBus` | ⬜ not started |
| AI‑3 | Native `AiChat` conversation buffer + streaming render + prompt UX | ⬜ not started |
| AI‑4 | `:ai-send` context push · claude-code-acp + gemini · in-protocol auth | ⬜ not started |

---

## Post-spike revision (Option A) — GOVERNS Tasks 1, 3, 4, 5

Task 0's spike (report: `.superpowers/sdd/task-0-report.md`) confirmed the plan
but found the `agent-client-protocol` crate **frames JSON-RPC itself** via a
`Builder` / `Stdio` / `ActiveSession` API (v1.2.0) — there is **no**
`ClientSideConnection`. User decision: **adopt the crate's connection** (not the
hand-rolled JSONL codec/actor the original Task 3/4 code shows). Where this
section conflicts with the Task 3/4/5 code blocks below, **this section wins**;
those blocks remain only as the interface contract + test shape to preserve.

**Confirmed values (bake in verbatim):**
- Dep: `agent-client-protocol = "1.2.0"` (Task 1 Cargo.toml — replace `"X.Y.Z"`).
- opencode ACP entry: `command = "opencode"`, `args = ["acp"]` (Task 2 — already correct).
- Framing: newline-delimited JSON (the crate handles it; do not hand-roll).
- `session/update` assistant text: `params.update.content.text`, gated on
  `params.update.sessionUpdate == "agent_message_chunk"` (Task 6 — already correct).

**Mandatory first step for the Task 3 implementer (de-risks the unpinned API):**
run `cargo add agent-client-protocol@1.2.0 -p lattice-ai` then
`cargo doc -p agent-client-protocol --no-deps --open` (or read the vendored
source under `~/.cargo/registry/src/`) and record the exact signatures of
`Builder`, `Stdio`, `ActiveSession`, `connect_with`, `InitializeRequest`,
`NewSessionRequest`, `PromptRequest`, `SessionNotification`, `SessionUpdate`,
`ContentBlock`, `TextContent`. Then do a live smoke capture: spawn
`opencode acp`, write an `initialize` + `session/new` + `session/prompt` sequence
to its stdin, read stdout — confirm the real frames match the schema before
locking code.

**Revised Task 3 (was: JSONL codec).** Build `connection.rs` as a thin adapter
over the crate. It **must expose this interface** so Tasks 4/5/7 are unchanged:
- `Connection::spawn(reader, writer) -> (Arc<Connection>, mpsc::Receiver<SessionNotification>)`
  where reader/writer are `AsyncRead`/`AsyncWrite` (real child stdio in prod,
  `tokio::io::duplex` in tests). Internally: build the crate's client connection
  over a `Stdio` wrapping the pair; forward inbound `session/update`
  notifications to the mpsc receiver.
- `async fn Connection::initialize(&self) -> Result<()>`
- `async fn Connection::new_session(&self, cwd: &str) -> Result<SessionId>`
- `async fn Connection::prompt(&self, session: &SessionId, text: &str) -> Result<()>`
  (These three replace the generic `request(method, Value)` — call the crate's
  typed request methods.) Keep `struct SessionId(pub String)`.
- Test over `tokio::io::duplex` with a scripted peer that emits real
  newline-JSON responses/notifications (the crate is on our side; the mock only
  needs to speak the wire format). Assert: `new_session` returns the peer's
  `sessionId`; a `session/update` notification reaches the receiver.

**Revised Task 4 (was: connection actor).** Now "handshake + live smoke": a free
`async fn handshake(conn: &Connection, cwd: &str) -> Result<SessionId>` =
`initialize` then `new_session`; plus the `#[ignore]` live integration test that
spawns real `opencode acp`, handshakes, prompts "reply with pong", and asserts a
`SessionNotification` arrives. (The supervisor in Task 7 consumes `handshake` +
`Connection::prompt`.)

**Revised Task 5 (was: session handshake).** Folded into Tasks 3–4 above. Task 5
becomes a no-op slot: mark it complete referencing Tasks 3–4 (do not create a
separate `session.rs`; `handshake` lives in `connection.rs` or a small
`session.rs` re-exporting from it — implementer's choice, keep the
`session::handshake`/`SessionId` paths the downstream tasks import).

**Task 6 unchanged in contract:** `assistant_text_from_update` stays
`serde_json::Value`-based (independent of the crate's enum shape) — the
supervisor serializes the `SessionNotification` to a `Value` before calling it,
OR reads the typed fields directly; either satisfies the Task 6 tests as written.

Model note: Tasks 3–4 are now integration/judgment work (unpinned API) → dispatch
their implementers on a standard model, not the cheap tier.

---

## File Structure (AI‑1)

- Create `crates/lattice-ai/Cargo.toml` — crate manifest.
- Create `crates/lattice-ai/src/lib.rs` — module decls + re-exports.
- Create `crates/lattice-ai/src/error.rs` — `AiError` + `Result`.
- Create `crates/lattice-ai/src/providers.rs` — `ProviderConfig` (pure data) +
  `opencode()`.
- Create `crates/lattice-ai/src/acp.rs` — thin wrapper over the
  `agent-client-protocol` crate: our type aliases + `codec` (JSONL frame
  encode/decode) + `Frame` enum.
- Create `crates/lattice-ai/src/connection.rs` — the async JSON-RPC connection
  actor over an `AsyncRead`+`AsyncWrite` pair (pending-request map +
  notification channel).
- Create `crates/lattice-ai/src/session.rs` — handshake + `prompt`.
- Create `crates/lattice-ai/src/supervisor.rs` — spawn provider subprocess, wire
  stdio → `connection`, own lifecycle.
- Create `crates/lattice-ai/src/handle.rs` — `AiClientHandle` + `AiState`.
- Create `crates/lattice-ai/src/inbound.rs` — `AiInboundEvent` + handler
  (session/update → `tracing`).
- Create `crates/lattice-ai/src/commands.rs` — `:opencode` / `:ai-prompt` /
  `:ai-stop`.
- Create `crates/lattice-ai/src/install.rs` — `install(boot)`.
- Modify `Cargo.toml` (workspace root) — add `crates/lattice-ai` to `members`.
- Modify the host boot site — add `lattice_ai::install(&mut boot)` (Task 9).

---

## Task 0: Spike — pin ACP crate + wire shapes (no production code)

**Files:** none (research; findings recorded in this task's commit message + a
scratch note deleted before commit).

**Interfaces:**
- Produces: confirmed dependency line for `agent-client-protocol`, the exact
  method names + payload struct paths for `initialize`, `session/new`,
  `session/prompt`, and the `session/update` notification, and the stdio framing
  (newline-delimited JSON vs. Content-Length). All later tasks consume these.

- [ ] **Step 1: Confirm the crate exists + version**

Run: `cargo search agent-client-protocol`
Expected: a line like `agent-client-protocol = "x.y.z"`. Record the latest
version. If absent, STOP and escalate — the plan assumes this crate; falling back
to hand-rolled serde types is a plan change, not an inline decision.

- [ ] **Step 2: Read its API surface**

Run: `cargo doc -p agent-client-protocol --no-deps` in a throwaway crate, or read
`https://docs.rs/agent-client-protocol`. Confirm and write down:
- the client/agent connection entry point (a `Connection`/`ClientSideConnection`
  type? a trait to implement for the client side?),
- the request types for `initialize`, `session/new`, `session/prompt`,
- the `session/update` notification enum + its `agent_message_chunk` variant
  shape (the field carrying assistant text),
- whether the crate already frames over stdio (if so, Tasks 4/6 use it directly
  and our `connection.rs` becomes a thin adapter).

- [ ] **Step 3: Confirm opencode's ACP invocation**

Run: `opencode --help` (and check `opencode.ai/docs/acp`). Record the exact
subcommand/flag that starts opencode as an ACP agent over stdio (e.g.
`opencode acp` or similar). This is the `ProviderConfig::opencode` command/args
in Task 2.

- [ ] **Step 4: Record findings + commit a marker**

Write the confirmed facts into the commit body. No code.

```bash
git commit --allow-empty -m "chore(ai): AI-1 spike — pin agent-client-protocol vX.Y.Z + opencode acp entry

- dep: agent-client-protocol = \"X.Y.Z\"
- client entry: <type/trait>
- session/update assistant text: <path/field>
- stdio framing: <newline-delimited | crate-managed>
- opencode ACP: <exact command + args>"
```

---

## Task 1: Crate skeleton + error type

**Files:**
- Create: `crates/lattice-ai/Cargo.toml`
- Create: `crates/lattice-ai/src/lib.rs`
- Create: `crates/lattice-ai/src/error.rs`
- Modify: `Cargo.toml` (workspace `members`)
- Test: inline in `error.rs`

**Interfaces:**
- Produces: `lattice_ai::error::{AiError, Result}`; a compiling crate in the
  workspace.

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/error.rs`:

```rust
//! Error surface for the `lattice-ai` ACP client.

/// Errors surfaced by the ACP client (transport, protocol, process lifecycle).
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// The provider process failed to spawn or exited unexpectedly.
    #[error("provider process error: {0}")]
    Process(String),
    /// A transport-level failure (stdio closed, framing error).
    #[error("transport error: {0}")]
    Transport(String),
    /// The agent returned a JSON-RPC error or an unexpected frame.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, AiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_prefixed() {
        let e = AiError::Transport("eof".into());
        assert_eq!(e.to_string(), "transport error: eof");
    }
}
```

Create `crates/lattice-ai/Cargo.toml` (copy `lattice-claude-code`'s `[package]`
block verbatim, change `name`):

```toml
[package]
name = "lattice-ai"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
agent-client-protocol = "X.Y.Z" # pinned in Task 0
lattice-protocol = { path = "../lattice-protocol" }
lattice-mode = { path = "../lattice-mode" }
lattice-grammar = { path = "../lattice-grammar" }
lattice-runtime = { path = "../lattice-runtime" }
lattice-core = { path = "../lattice-core" }
lattice-diff = { path = "../lattice-diff" }
tokio = { workspace = true, features = ["sync", "rt", "rt-multi-thread", "macros", "process", "io-util", "time"] }
futures = { workspace = true }
arc-swap = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "time", "macros", "rt", "io-util"] }
futures = { workspace = true }

[lints]
workspace = true
```

Create `crates/lattice-ai/src/lib.rs`:

```rust
//! `lattice-ai` — lattice as an ACP (Agent Client Protocol) client.
//!
//! Spawns an AI coding agent (opencode, and later claude-code-acp / gemini) as
//! a stdio subprocess and drives it over JSON-RPC. Architecturally a network
//! peer like `lattice-lsp`; runs no agent in-process. See
//! `docs/dev/architecture/ai-agent-protocol.md`.

pub mod error;

pub use error::{AiError, Result};
```

Add `"crates/lattice-ai"` to the workspace `Cargo.toml` `members` list
(alphabetical position, after `lattice-ai`'s neighbors).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-ai error_display_is_prefixed`
Expected: the crate must first *compile*. If `agent-client-protocol = "X.Y.Z"` is
unresolved, fix the version from Task 0. Once compiling, the test PASSES (it is a
trivial guard) — this task's "red" is the crate not existing/compiling.

- [ ] **Step 3: (folded into Step 1)** — implementation is the code above.

- [ ] **Step 4: Verify build is green**

Run: `cargo build -p lattice-ai && cargo test -p lattice-ai`
Expected: builds; 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/Cargo.toml crates/lattice-ai/src/lib.rs crates/lattice-ai/src/error.rs Cargo.toml
git commit -m "feat(ai): AI-1 crate skeleton + error type"
```

---

## Task 2: Provider config (pure data)

**Files:**
- Create: `crates/lattice-ai/src/providers.rs`
- Modify: `crates/lattice-ai/src/lib.rs` (add `pub mod providers;`)
- Test: inline in `providers.rs`

**Interfaces:**
- Produces: `ProviderConfig { command: String, args: Vec<String>, env:
  Vec<(String, String)>, display_name: &'static str }` and
  `ProviderConfig::opencode() -> ProviderConfig`. Consumed by `supervisor.rs`
  (Task 6) and `commands.rs` (Task 8).

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/providers.rs`:

```rust
//! Provider launch configs — the crate's extension point. Adding an agent is a
//! new constructor here, not a new subsystem.

/// How to launch one ACP agent as a stdio subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Executable to spawn.
    pub command: String,
    /// Arguments that put the agent into ACP-over-stdio mode.
    pub args: Vec<String>,
    /// Extra environment injected into the child.
    pub env: Vec<(String, String)>,
    /// Human-readable name (modeline / echoes).
    pub display_name: &'static str,
}

impl ProviderConfig {
    /// opencode's native ACP entry (exact args confirmed in Task 0's spike).
    pub fn opencode() -> Self {
        Self {
            command: "opencode".to_string(),
            args: vec!["acp".to_string()], // ← replace with Task 0's confirmed flag
            env: Vec::new(),
            display_name: "opencode",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_config_names_the_binary_and_display() {
        let p = ProviderConfig::opencode();
        assert_eq!(p.command, "opencode");
        assert_eq!(p.display_name, "opencode");
        assert!(!p.args.is_empty(), "must pass the ACP-mode flag");
    }
}
```

Add `pub mod providers;` to `lib.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-ai opencode_config_names_the_binary_and_display`
Expected: FAIL before the module is added (unresolved), then PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green**

Run: `cargo test -p lattice-ai providers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/providers.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 provider config + opencode launch spec"
```

---

## Task 3: ACP frame codec (JSONL over stdio)

**Files:**
- Create: `crates/lattice-ai/src/acp.rs`
- Modify: `crates/lattice-ai/src/lib.rs` (add `pub mod acp;`)
- Test: inline in `acp.rs`

**Interfaces:**
- Consumes: `agent-client-protocol` request/notification types (Task 0).
- Produces: `acp::encode_line(value: &serde_json::Value) -> String` and
  `acp::decode_line(line: &str) -> Result<Frame>` where
  `enum Frame { Response { id: u64, result: serde_json::Value }, Error { id: u64,
  message: String }, Notification { method: String, params: serde_json::Value } }`.
  Consumed by `connection.rs` (Task 4).

> If Task 0 found the `agent-client-protocol` crate manages its own stdio
> framing, this task instead wraps that framing and `Frame` mirrors the crate's
> incoming-message enum — keep the same `Frame` shape so Task 4 is unchanged.

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/acp.rs`:

```rust
//! JSON-RPC-over-stdio framing for ACP. ACP frames are newline-delimited JSON
//! objects (one JSON value per line). This module is transport-agnostic: it
//! turns lines into typed [`Frame`]s and values into lines.

use serde_json::Value;

use crate::error::{AiError, Result};

/// A decoded inbound frame from the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// A successful response to a request we sent, keyed by its `id`.
    Response { id: u64, result: Value },
    /// A JSON-RPC error response, keyed by its `id`.
    Error { id: u64, message: String },
    /// A server-initiated notification (no `id`) — e.g. `session/update`.
    Notification { method: String, params: Value },
}

/// Serialize a JSON-RPC value to a single newline-terminated line.
pub fn encode_line(value: &Value) -> String {
    let mut s = value.to_string();
    s.push('\n');
    s
}

/// Parse one line into a [`Frame`].
pub fn decode_line(line: &str) -> Result<Frame> {
    let v: Value = serde_json::from_str(line.trim())
        .map_err(|e| AiError::Protocol(format!("bad json: {e}")))?;
    if let Some(err) = v.get("error") {
        let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        return Ok(Frame::Error { id, message });
    }
    if let Some(id) = v.get("id").and_then(Value::as_u64) {
        let result = v.get("result").cloned().unwrap_or(Value::Null);
        return Ok(Frame::Response { id, result });
    }
    let method = v
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| AiError::Protocol("frame has neither id nor method".into()))?
        .to_string();
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    Ok(Frame::Notification { method, params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_a_response() {
        let f = decode_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert_eq!(f, Frame::Response { id: 1, result: json!({"ok": true}) });
    }

    #[test]
    fn decodes_a_notification() {
        let f = decode_line(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"kind":"x"}}"#,
        )
        .unwrap();
        assert_eq!(
            f,
            Frame::Notification {
                method: "session/update".into(),
                params: json!({"kind": "x"})
            }
        );
    }

    #[test]
    fn decodes_an_error() {
        let f = decode_line(r#"{"jsonrpc":"2.0","id":2,"error":{"message":"nope"}}"#).unwrap();
        assert_eq!(f, Frame::Error { id: 2, message: "nope".into() });
    }

    #[test]
    fn encode_line_is_newline_terminated() {
        let line = encode_line(&json!({"id": 1}));
        assert!(line.ends_with('\n'));
        assert!(line.starts_with('{'));
    }
}
```

Add `pub mod acp;` to `lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lattice-ai acp::`
Expected: FAIL (module unresolved) before adding; then all four PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green**

Run: `cargo test -p lattice-ai acp::`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/acp.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 JSON-RPC frame codec (JSONL)"
```

---

## Task 4: Connection actor over async stdio

**Files:**
- Create: `crates/lattice-ai/src/connection.rs`
- Modify: `crates/lattice-ai/src/lib.rs` (add `pub mod connection;`)
- Test: inline in `connection.rs` (using `tokio::io::duplex`)

**Interfaces:**
- Consumes: `acp::{Frame, encode_line, decode_line}` (Task 3).
- Produces:
  - `Connection::spawn<R, W>(reader: R, writer: W) -> (Connection, mpsc::Receiver<acp::Frame>)`
    where `R: AsyncRead + Unpin + Send + 'static`, `W: AsyncWrite + Unpin + Send + 'static`.
    The returned receiver yields inbound `Frame::Notification`s.
  - `async fn Connection::request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value>`
    — sends a JSON-RPC request with a fresh id and resolves with its result.
  Consumed by `session.rs` (Task 5) and `supervisor.rs` (Task 6).

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/connection.rs`:

```rust
//! The ACP JSON-RPC connection actor. Owns a writer + a reader task; matches
//! responses to pending requests by id; forwards notifications on a channel.
//! Transport-agnostic over any `AsyncRead`/`AsyncWrite` (real child stdio in
//! production, `tokio::io::duplex` in tests).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::acp::{self, Frame};
use crate::error::{AiError, Result};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

/// A live JSON-RPC connection to the agent.
pub struct Connection {
    next_id: AtomicU64,
    pending: Pending,
    tx: mpsc::UnboundedSender<String>,
}

impl Connection {
    /// Wire `reader`/`writer` into a connection. Returns the connection and a
    /// receiver of inbound notification frames.
    pub fn spawn<R, W>(reader: R, writer: W) -> (Arc<Connection>, mpsc::Receiver<Frame>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::channel::<Frame>(64);
        let (tx, mut out_rx) = mpsc::unbounded_channel::<String>();

        // Writer task: drain outbound lines to the child stdin.
        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        });

        // Reader task: parse inbound lines, route responses vs notifications.
        let pending_r = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match acp::decode_line(&line) {
                    Ok(Frame::Response { id, result }) => {
                        if let Some(tx) = pending_r.lock().unwrap().remove(&id) {
                            let _ = tx.send(Ok(result));
                        }
                    }
                    Ok(Frame::Error { id, message }) => {
                        if let Some(tx) = pending_r.lock().unwrap().remove(&id) {
                            let _ = tx.send(Err(message));
                        }
                    }
                    Ok(n @ Frame::Notification { .. }) => {
                        let _ = notif_tx.send(n).await;
                    }
                    Err(_) => { /* skip malformed line */ }
                }
            }
        });

        (
            Arc::new(Connection {
                next_id: AtomicU64::new(1),
                pending,
                tx,
            }),
            notif_rx,
        )
    }

    /// Send a JSON-RPC request and await its result.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (res_tx, res_rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, res_tx);
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.tx
            .send(acp::encode_line(&frame))
            .map_err(|_| AiError::Transport("writer closed".into()))?;
        match res_rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(m)) => Err(AiError::Protocol(m)),
            Err(_) => Err(AiError::Transport("connection dropped".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A request resolves when the mock agent echoes a response with the same id.
    #[tokio::test]
    async fn request_resolves_on_matching_response() {
        // duplex: (our_side) <-> (agent_side)
        let (our_read, agent_write) = tokio::io::duplex(4096);
        let (agent_read, our_write) = tokio::io::duplex(4096);
        let (conn, _notif) = Connection::spawn(our_read, our_write);

        // Mock agent: read one request line, reply with a canned result.
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let mut agent_write = agent_write;
            if let Ok(Some(req)) = lines.next_line().await {
                let v: Value = serde_json::from_str(&req).unwrap();
                let id = v["id"].as_u64().unwrap();
                let reply = format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"x"}}}}"#
                );
                agent_write.write_all(reply.as_bytes()).await.unwrap();
                agent_write.write_all(b"\n").await.unwrap();
            }
        });

        let result = conn.request("initialize", json!({})).await.unwrap();
        assert_eq!(result["protocolVersion"], "x");
    }

    /// Notification frames arrive on the notification receiver.
    #[tokio::test]
    async fn notifications_reach_the_receiver() {
        let (our_read, agent_write) = tokio::io::duplex(4096);
        let (_agent_read, our_write) = tokio::io::duplex(4096);
        let (_conn, mut notif) = Connection::spawn(our_read, our_write);

        let mut agent_write = agent_write;
        agent_write
            .write_all(
                br#"{"jsonrpc":"2.0","method":"session/update","params":{"k":1}}"#,
            )
            .await
            .unwrap();
        agent_write.write_all(b"\n").await.unwrap();

        let frame = notif.recv().await.unwrap();
        match frame {
            Frame::Notification { method, .. } => assert_eq!(method, "session/update"),
            other => panic!("expected notification, got {other:?}"),
        }
    }
}
```

Add `pub mod connection;` to `lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lattice-ai connection::`
Expected: FAIL (unresolved) before adding; then both PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green**

Run: `cargo test -p lattice-ai connection::`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/connection.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 JSON-RPC connection actor over async stdio"
```

---

## Task 5: Session handshake + prompt

**Files:**
- Create: `crates/lattice-ai/src/session.rs`
- Modify: `crates/lattice-ai/src/lib.rs` (add `pub mod session;`)
- Test: inline in `session.rs` (scripted mock agent over duplex)

**Interfaces:**
- Consumes: `Connection::request` (Task 4).
- Produces:
  - `async fn handshake(conn: &Connection, cwd: &str) -> Result<SessionId>` —
    runs `initialize` then `session/new`, returns the new session id.
  - `struct SessionId(pub String)`.
  - `async fn prompt(conn: &Connection, session: &SessionId, text: &str) -> Result<()>`.
  Consumed by `supervisor.rs` (Task 6).

> Method names/params here (`initialize`, `session/new`, `session/prompt`) and
> the session-id field path are the ones confirmed in Task 0. If the
> `agent-client-protocol` crate exposes typed request builders, call those and
> keep these function signatures.

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/session.rs`:

```rust
//! ACP session lifecycle: the `initialize → session/new` handshake and
//! `session/prompt`. Thin orchestration over [`Connection`](crate::connection).

use serde_json::json;

use crate::connection::Connection;
use crate::error::{AiError, Result};

/// An opaque ACP session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(pub String);

/// Run the ACP handshake and open a session rooted at `cwd`.
pub async fn handshake(conn: &Connection, cwd: &str) -> Result<SessionId> {
    // MCP-style capability handshake.
    conn.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    )
    .await?;
    let res = conn
        .request("session/new", json!({"cwd": cwd, "mcpServers": []}))
        .await?;
    let id = res
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AiError::Protocol("session/new returned no sessionId".into()))?;
    Ok(SessionId(id.to_string()))
}

/// Send a user prompt into `session`.
pub async fn prompt(conn: &Connection, session: &SessionId, text: &str) -> Result<()> {
    conn.request(
        "session/prompt",
        json!({
            "sessionId": session.0,
            "prompt": [{"type": "text", "text": text}]
        }),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// The handshake sends initialize then session/new and returns the id the
    /// agent assigns.
    #[tokio::test]
    async fn handshake_returns_session_id() {
        let (our_read, agent_write) = tokio::io::duplex(8192);
        let (agent_read, our_write) = tokio::io::duplex(8192);
        let (conn, _notif) = Connection::spawn(our_read, our_write);

        // Mock agent: reply to each request by id; session/new → {sessionId}.
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let mut w = agent_write;
            while let Ok(Some(req)) = lines.next_line().await {
                let v: Value = serde_json::from_str(&req).unwrap();
                let id = v["id"].as_u64().unwrap();
                let method = v["method"].as_str().unwrap();
                let result = if method == "session/new" {
                    r#"{"sessionId":"sess-42"}"#
                } else {
                    r#"{}"#
                };
                let reply = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#);
                w.write_all(reply.as_bytes()).await.unwrap();
                w.write_all(b"\n").await.unwrap();
            }
        });

        let id = handshake(&conn, "/work").await.unwrap();
        assert_eq!(id, SessionId("sess-42".into()));
    }
}
```

Add `pub mod session;` to `lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lattice-ai session::`
Expected: FAIL (unresolved) before adding; then PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green**

Run: `cargo test -p lattice-ai session::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/session.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 session handshake + prompt"
```

---

## Task 6: Inbound event + handler (session/update → *messages*)

**Files:**
- Create: `crates/lattice-ai/src/inbound.rs`
- Modify: `crates/lattice-ai/src/lib.rs` (add `pub mod inbound;`)
- Test: inline in `inbound.rs`

**Interfaces:**
- Consumes: `acp::Frame` (Task 3), `lattice_grammar::effect::Effect`,
  `lattice_mode::inbound::InboundBus`.
- Produces:
  - `enum AiInboundEvent { AssistantText(String), Status(String) }`.
  - `fn assistant_text_from_update(params: &serde_json::Value) -> Option<String>`
    — extracts assistant text from a `session/update` notification's params
    (the field path confirmed in Task 0).
  - `fn make_handler() -> impl FnMut(AiInboundEvent) -> Vec<Effect>` — the
    per-tick drain closure passed to `SubsystemBoot::inbound`; emits a `tracing`
    event (→ `*messages*`) per chunk and returns no `Effect`s (rendering is via
    the tracing layer, not an Effect, in AI‑1).
  Consumed by `install.rs` (Task 8/9) and `supervisor.rs` (Task 7).

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/inbound.rs`:

```rust
//! Editor-thread delivery of agent output. The supervisor's notification task
//! parses `session/update` frames into [`AiInboundEvent`]s and `send`s them on
//! the `SubsystemBoot::inbound` bus; the per-tick handler logs each into
//! `*messages*` (via `tracing`; the real `AiChat` buffer arrives in AI-3).

use serde_json::Value;

use lattice_grammar::effect::Effect;

/// An agent output event delivered to the editor thread.
#[derive(Debug, Clone, PartialEq)]
pub enum AiInboundEvent {
    /// A chunk of assistant reply text.
    AssistantText(String),
    /// A lifecycle/status line (session opened, done, error).
    Status(String),
}

/// Extract assistant text from a `session/update` notification's params.
/// The field path (`update.sessionUpdate == "agent_message_chunk"`, text under
/// `update.content.text`) is confirmed against the schema in Task 0.
pub fn assistant_text_from_update(params: &Value) -> Option<String> {
    let update = params.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
        return None;
    }
    Some(update.get("content")?.get("text")?.as_str()?.to_string())
}

/// The per-tick drain handler: log each event into `*messages*`. Returns no
/// `Effect`s (AI-1 renders via the tracing layer).
pub fn make_handler() -> impl FnMut(AiInboundEvent) -> Vec<Effect> {
    move |event| {
        match event {
            AiInboundEvent::AssistantText(t) => tracing::info!(target: "ai", "{t}"),
            AiInboundEvent::Status(s) => tracing::info!(target: "ai", "[{s}]"),
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_assistant_chunk_text() {
        let params = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk",
                       "content": {"type": "text", "text": "hello"}}
        });
        assert_eq!(assistant_text_from_update(&params), Some("hello".into()));
    }

    #[test]
    fn ignores_non_message_updates() {
        let params = json!({"update": {"sessionUpdate": "tool_call"}});
        assert_eq!(assistant_text_from_update(&params), None);
    }

    #[test]
    fn handler_consumes_events_without_panicking() {
        let mut h = make_handler();
        assert!(h(AiInboundEvent::AssistantText("x".into())).is_empty());
        assert!(h(AiInboundEvent::Status("open".into())).is_empty());
    }
}
```

Add `pub mod inbound;` to `lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lattice-ai inbound::`
Expected: FAIL (unresolved) before adding; then 3 PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green**

Run: `cargo test -p lattice-ai inbound::`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/inbound.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 inbound events + session/update text extraction"
```

---

## Task 7: Supervisor + handle (spawn provider, wire lifecycle)

**Files:**
- Create: `crates/lattice-ai/src/supervisor.rs`
- Create: `crates/lattice-ai/src/handle.rs`
- Modify: `crates/lattice-ai/src/lib.rs`
- Test: inline in `handle.rs` (state snapshot) + one `#[ignore]` integration test
  in `supervisor.rs`.

**Interfaces:**
- Consumes: `ProviderConfig` (Task 2), `Connection` (Task 4), `session::*`
  (Task 5), `AiInboundEvent` (Task 6), `InboundBus<AiInboundEvent>`,
  `tokio::runtime::Handle`.
- Produces:
  - `struct AiState { pub running: bool, pub provider: Option<&'static str>,
    pub session: Option<String> }` (Default).
  - `struct AiClientHandle` (Clone) with:
    - `fn spawn(runtime: &Handle, inbound: InboundBus<AiInboundEvent>) -> AiClientHandle`
    - `fn start(&self, provider: ProviderConfig)` — non-blocking; supervisor
      spawns the child + handshakes.
    - `fn prompt(&self, text: String)` — non-blocking; forwards to the session.
    - `fn stop(&self)`
    - `fn snapshot(&self) -> AiState`
  Consumed by `commands.rs` (Task 8) + `install.rs` (Task 9).

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/handle.rs`:

```rust
//! The clone-able editor-thread handle to the ACP client + its wait-free state
//! snapshot. Mirrors `lattice-claude-code`'s `ClaudeCodeServerHandle`.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use crate::providers::ProviderConfig;

/// Wait-free snapshot of client state (modeline / status reads).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiState {
    /// Whether a provider process is running with an open session.
    pub running: bool,
    /// The active provider's display name.
    pub provider: Option<&'static str>,
    /// The active session id, when open.
    pub session: Option<String>,
}

/// Commands to the supervisor task.
pub(crate) enum AiCmd {
    Start(ProviderConfig),
    Prompt(String),
    Stop,
}

/// Clone-able handle. The editor thread only ever sends `AiCmd`s and reads the
/// `ArcSwap<AiState>` — never blocks.
#[derive(Clone)]
pub struct AiClientHandle {
    pub(crate) cmd_tx: mpsc::UnboundedSender<AiCmd>,
    pub(crate) state: Arc<ArcSwap<AiState>>,
}

impl AiClientHandle {
    /// Start a provider (non-blocking channel send).
    pub fn start(&self, provider: ProviderConfig) {
        let _ = self.cmd_tx.send(AiCmd::Start(provider));
    }
    /// Send a prompt (non-blocking).
    pub fn prompt(&self, text: String) {
        let _ = self.cmd_tx.send(AiCmd::Prompt(text));
    }
    /// Stop the provider (non-blocking).
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(AiCmd::Stop);
    }
    /// Read the current state snapshot.
    pub fn snapshot(&self) -> AiState {
        (**self.state.load()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh handle (no supervisor running) reports the default (idle) state,
    /// and non-blocking sends never panic even with no live receiver.
    #[test]
    fn idle_snapshot_and_nonblocking_sends() {
        let (cmd_tx, _rx) = mpsc::unbounded_channel();
        let handle = AiClientHandle {
            cmd_tx,
            state: Arc::new(ArcSwap::from_pointee(AiState::default())),
        };
        assert_eq!(handle.snapshot(), AiState::default());
        handle.prompt("hi".into()); // must not panic
        handle.stop();
    }
}
```

In `crates/lattice-ai/src/supervisor.rs`:

```rust
//! The idle supervisor task: owns the provider child process + its ACP
//! connection + session. Driven by `AiCmd`s from the [`AiClientHandle`]. All
//! protocol I/O lives here, off the editor thread.

use std::process::Stdio;
use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use lattice_mode::inbound::InboundBus;

use crate::connection::Connection;
use crate::handle::{AiClientHandle, AiCmd, AiState};
use crate::inbound::{assistant_text_from_update, AiInboundEvent};
use crate::providers::ProviderConfig;
use crate::session::{self, SessionId};
use crate::acp::Frame;

impl AiClientHandle {
    /// Spawn the supervisor on `runtime`; returns the clone-able handle.
    pub fn spawn(runtime: &Handle, inbound: InboundBus<AiInboundEvent>) -> AiClientHandle {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AiCmd>();
        let state = Arc::new(ArcSwap::from_pointee(AiState::default()));
        let state_task = state.clone();

        runtime.spawn(async move {
            let mut conn: Option<Arc<Connection>> = None;
            let mut sess: Option<SessionId> = None;

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    AiCmd::Start(provider) => {
                        match start_provider(&provider, inbound.clone()).await {
                            Ok((c, s)) => {
                                conn = Some(c);
                                sess = Some(s.clone());
                                state_task.store(Arc::new(AiState {
                                    running: true,
                                    provider: Some(provider.display_name),
                                    session: Some(s.0),
                                }));
                                let _ = inbound.send(AiInboundEvent::Status("session opened".into()));
                            }
                            Err(e) => {
                                let _ = inbound.send(AiInboundEvent::Status(format!("start failed: {e}")));
                            }
                        }
                    }
                    AiCmd::Prompt(text) => {
                        if let (Some(c), Some(s)) = (conn.as_ref(), sess.as_ref()) {
                            let c = c.clone();
                            let s = s.clone();
                            tokio::spawn(async move {
                                let _ = session::prompt(&c, &s, &text).await;
                            });
                        }
                    }
                    AiCmd::Stop => {
                        conn = None;
                        sess = None;
                        state_task.store(Arc::new(AiState::default()));
                    }
                }
            }
        });

        AiClientHandle { cmd_tx, state }
    }
}

/// Spawn the provider child, wire its stdio into a [`Connection`], run the
/// handshake, and start forwarding `session/update` text to the editor.
async fn start_provider(
    provider: &ProviderConfig,
    inbound: InboundBus<AiInboundEvent>,
) -> crate::Result<(Arc<Connection>, SessionId)> {
    let mut child = Command::new(&provider.command)
        .args(&provider.args)
        .envs(provider.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| crate::AiError::Process(e.to_string()))?;

    let stdin = child.stdin.take().ok_or_else(|| crate::AiError::Process("no stdin".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| crate::AiError::Process("no stdout".into()))?;
    let (conn, mut notif) = Connection::spawn(stdout, stdin);

    // Forward notification text to the editor thread.
    let inbound_notif = inbound.clone();
    tokio::spawn(async move {
        while let Some(Frame::Notification { method, params }) = notif.recv().await {
            if method == "session/update" {
                if let Some(text) = assistant_text_from_update(&params) {
                    let _ = inbound_notif.send(AiInboundEvent::AssistantText(text));
                }
            }
        }
    });

    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let session = session::handshake(&conn, &cwd).await?;
    Ok((conn, session))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Integration: spawn the REAL opencode agent, open a session, prompt, and
    /// observe assistant text. Requires `opencode` installed + authenticated;
    /// `#[ignore]`d so CI without opencode stays green. Run manually:
    ///   cargo test -p lattice-ai --  --ignored opencode_end_to_end
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn opencode_end_to_end() {
        let (inbound, mut rx) = InboundBus::unbounded_for_test();
        let (c, _s) = start_provider(&ProviderConfig::opencode(), inbound).await.unwrap();
        let sess = session::handshake(&c, ".").await.unwrap();
        session::prompt(&c, &sess, "reply with the single word: pong").await.unwrap();
        // Expect at least one assistant text event within a few seconds.
        let got = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("agent replied in time");
        assert!(matches!(got, Some(AiInboundEvent::AssistantText(_))));
    }
}
```

Add to `lib.rs`:

```rust
pub mod handle;
pub mod supervisor;

pub use handle::{AiClientHandle, AiState};
```

> **Note on `InboundBus::unbounded_for_test`:** confirm the test constructor the
> `lattice-mode` `InboundBus` exposes (grep `InboundBus` in
> `crates/lattice-mode/src/inbound.rs`). If none exists, the integration test
> constructs the bus via `SubsystemBoot::inbound` in Task 9's wiring instead, and
> this task's `#[ignore]` test is deferred to Task 9. The `handle.rs` unit test
> (Step 1) stands regardless.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lattice-ai handle::`
Expected: FAIL (unresolved) before adding; then PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green (unit) + compile (integration)**

Run: `cargo test -p lattice-ai handle:: && cargo test -p lattice-ai --no-run`
Expected: unit test PASSES; integration test compiles (not run).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/handle.rs crates/lattice-ai/src/supervisor.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 supervisor + AiClientHandle (spawn opencode, handshake, prompt)"
```

---

## Task 8: Ex-commands (`:opencode` / `:ai-prompt` / `:ai-stop`)

**Files:**
- Create: `crates/lattice-ai/src/commands.rs`
- Modify: `crates/lattice-ai/src/lib.rs`
- Test: inline in `commands.rs`

**Interfaces:**
- Consumes: `AiClientHandle` (Task 7), `lattice_grammar::registry::{CommandRegistry,
  ExCommandSpec, SurfaceForm, ExCommandContext}`, `Effect`, `Args`.
- Produces: `fn register_ai_ex_commands(registry: &mut CommandRegistry, handle:
  AiClientHandle)`. Consumed by `install.rs` (Task 9).

- [ ] **Step 1: Write the failing test**

In `crates/lattice-ai/src/commands.rs` (mirror `lattice-claude-code/src/commands.rs`):

```rust
//! Crate-owned ex-commands. Each `apply` captures the `AiClientHandle` and
//! drives it directly (non-blocking sends), returning existing `Effect`s only.

use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::error::{CommandError, GrammarResult};
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use crate::handle::AiClientHandle;
use crate::providers::ProviderConfig;

fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs("trailing characters after command".into()))
    }
}

/// The rest-of-line is the prompt text (may be empty → error echo).
fn parse_rest_as_text(rest: &str, _bang: bool) -> GrammarResult<Args> {
    Ok(Args::Text(rest.trim().to_string()))
}

/// Register `:opencode`, `:ai-prompt`, `:ai-stop` against `registry`.
pub fn register_ai_ex_commands(registry: &mut CommandRegistry, handle: AiClientHandle) {
    let start = handle.clone();
    registry.register_ex_command(
        "opencode",
        "Launch the opencode agent over ACP and open a session wired to this editor.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                start.start(ProviderConfig::opencode());
                Ok(Effect::Echo { level: EchoLevel::Info, text: "opencode: starting agent".into() })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let prompt = handle.clone();
    registry.register_ex_command(
        "ai-prompt",
        "Send the rest of the line to the running AI agent as a prompt.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_rest_as_text),
            apply: Box::new(move |ctx| {
                let text = match &ctx.args {
                    Args::Text(t) if !t.is_empty() => t.clone(),
                    _ => return Ok(Effect::Echo { level: EchoLevel::Error, text: "ai-prompt: empty prompt".into() }),
                };
                prompt.prompt(text);
                Ok(Effect::Echo { level: EchoLevel::Info, text: "ai-prompt: sent".into() })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let stop = handle;
    registry.register_ex_command(
        "ai-stop",
        "Stop the running AI agent and close its session.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                stop.stop();
                Ok(Effect::Echo { level: EchoLevel::Info, text: "ai: stopping agent".into() })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::handle::{AiClientHandle, AiState};
    use arc_swap::ArcSwap;
    use lattice_grammar::registry::ExCommandContext;
    use lattice_grammar::{CancellationToken, Count, Register};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn test_handle() -> (AiClientHandle, mpsc::UnboundedReceiver<crate::handle::AiCmd>) {
        let (cmd_tx, rx) = mpsc::unbounded_channel();
        (AiClientHandle { cmd_tx, state: Arc::new(ArcSwap::from_pointee(AiState::default())) }, rx)
    }

    fn ctx(args: Args) -> ExCommandContext {
        ExCommandContext {
            bang: false, args, range: None,
            register: Register::default(), count: Count::default(),
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn opencode_registers_and_starts() {
        let (handle, mut rx) = test_handle();
        let mut reg = CommandRegistry::new();
        register_ai_ex_commands(&mut reg, handle);
        let id = reg.id_by_name("opencode").expect("`:opencode` registered");
        let spec = reg.ex_command_spec(id).unwrap();
        match (spec.apply)(&ctx(Args::None)).unwrap() {
            Effect::Echo { level, .. } => assert_eq!(level, EchoLevel::Info),
            other => panic!("expected echo, got {other:?}"),
        }
        assert!(matches!(rx.try_recv(), Ok(crate::handle::AiCmd::Start(_))));
    }

    #[test]
    fn ai_prompt_rejects_empty() {
        let (handle, _rx) = test_handle();
        let mut reg = CommandRegistry::new();
        register_ai_ex_commands(&mut reg, handle);
        let id = reg.id_by_name("ai-prompt").unwrap();
        let spec = reg.ex_command_spec(id).unwrap();
        match (spec.apply)(&ctx(Args::Text(String::new()))).unwrap() {
            Effect::Echo { level, .. } => assert_eq!(level, EchoLevel::Error),
            other => panic!("expected echo, got {other:?}"),
        }
    }
}
```

Add `pub mod commands;` + `pub use commands::register_ai_ex_commands;` to `lib.rs`.
Make `AiCmd` visible to the test module — it is already `pub(crate)`.

> Verify `Args::Text` is the correct variant for rest-of-line text (grep
> `enum Args` in `crates/lattice-grammar/src/args.rs`); if the variant differs,
> adjust `parse_rest_as_text` + the match arms. This is the only
> lattice-grammar-shape assumption in the task.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lattice-ai commands::`
Expected: FAIL (unresolved) before adding; then both PASS.

- [ ] **Step 3: (folded into Step 1).**

- [ ] **Step 4: Verify green**

Run: `cargo test -p lattice-ai commands::`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ai/src/commands.rs crates/lattice-ai/src/lib.rs
git commit -m "feat(ai): AI-1 ex-commands :opencode / :ai-prompt / :ai-stop"
```

---

## Task 9: Boot install + host wiring

**Files:**
- Create: `crates/lattice-ai/src/install.rs`
- Modify: `crates/lattice-ai/src/lib.rs` (add `pub mod install; pub use install::install;`)
- Modify: the host boot site (find with the grep in Step 1).
- Test: inline in `install.rs`.

**Interfaces:**
- Consumes: `SubsystemBoot` (Task refs), `register_ai_ex_commands` (Task 8),
  `AiClientHandle::spawn` (Task 7), `AiInboundEvent` + `make_handler` (Task 6).
- Produces: `pub fn install(boot: &mut impl SubsystemBoot)`.

- [ ] **Step 1: Locate the host boot install list**

Run: `grep -rn "lattice_claude_code::install" crates/`
Expected: one call site in the host boot (e.g. `crates/lattice-host/src/…boot….rs`).
That is where `lattice_ai::install(&mut boot)` is added.

- [ ] **Step 2: Write the failing test**

In `crates/lattice-ai/src/install.rs`:

```rust
//! The single crate-owned boot entry point. One Phase-B line in the host:
//! `lattice_ai::install(&mut boot)`.

use lattice_mode::SubsystemBoot;

use crate::commands;
use crate::handle::AiClientHandle;
use crate::inbound::{make_handler, AiInboundEvent};

/// Wire the ACP client into the editor at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
    // The inbound bus: agent output (`session/update` text) → per-tick drain →
    // `*messages*` via tracing. The drain registration rides into the Editor.
    let inbound = boot.inbound::<AiInboundEvent, _>(make_handler());

    // Spawn the idle supervisor on the shared runtime; expose the handle.
    let handle = AiClientHandle::spawn(boot.runtime_handle(), inbound);

    // Crate-owned ex-commands.
    commands::register_ai_ex_commands(boot.commands_mut(), handle.clone());

    // Expose the handle as a service (for a future modeline segment / AI-3 UI).
    boot.register_service::<AiClientHandle>(handle);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::handle::AiClientHandle;

    /// `install` registers `:opencode` on the boot's command registry and
    /// exposes the `AiClientHandle` service. Uses the shared test boot harness.
    #[tokio::test]
    async fn install_registers_commands_and_service() {
        // Build the test SubsystemBoot the same way lattice-claude-code's
        // install test does — see crates/lattice-claude-code/src/install.rs
        // tests (or the shared test helper in lattice-mode). Then:
        let mut boot = lattice_mode::testing::TestBoot::new(); // ← confirm exact path in Step 3
        install(&mut boot);
        assert!(boot.commands_ref().id_by_name("opencode").is_some());
        assert!(boot.service::<AiClientHandle>().is_some());
    }
}
```

- [ ] **Step 3: Confirm the test-boot harness**

Run: `grep -rn "impl SubsystemBoot" crates/ | grep -i test` and inspect
`crates/lattice-claude-code/src/install.rs` for how (if) its `install` is tested.
Replace `lattice_mode::testing::TestBoot` + `commands_ref()` with the actual
harness names. If `lattice-claude-code` has **no** `install` unit test (its
coverage is via `commands.rs` tests + a host integration test), then **drop this
`install.rs` unit test** and rely on Task 8's command tests + a manual boot
check; note that in the commit. Do not invent a harness that does not exist.

- [ ] **Step 4: Add the host boot line**

At the site from Step 1, add alongside the other `install` calls:

```rust
lattice_ai::install(&mut boot);
```

Add to `lib.rs`:

```rust
pub mod install;
pub use install::install;
```

- [ ] **Step 5: Verify build + tests + the workspace suites the boot change touches**

Run:
```bash
cargo build -p lattice-ai -p lattice-host
cargo test -p lattice-ai
```
Expected: workspace builds; `lattice-ai` tests pass. Per
`lattice-run-renderer-suites-after-boot-changes`, because this edits the host
boot list, also run the renderer/app suites:
```bash
cargo test -p lattice-ui-tui -p lattice-ui-gpui
```
Expected: PASS (no regression from the added install line).

- [ ] **Step 6: Manual smoke (opencode installed + authenticated)**

Launch lattice, run `:opencode`, then `:ai-prompt reply with the word pong`,
then `:messages`. Expected: the `*messages*` buffer shows the agent's reply text
under the `ai` target. (This exercises the real subprocess path Task 7's
`#[ignore]` test also covers.)

- [ ] **Step 7: Commit**

```bash
git add crates/lattice-ai/src/install.rs crates/lattice-ai/src/lib.rs <host-boot-file>
git commit -m "feat(ai): AI-1 boot install + host wiring (:opencode end to end)"
```

---

## Self-Review (completed)

- **Spec coverage:** AI‑1 row of the umbrella (§7) — transport (Tasks 3–4),
  session skeleton (Task 5), opencode provider (Task 2), reply → `*messages*`
  (Tasks 6, 9). Mode-ownership constraint (§2) — no new `Effect`/`Editor`
  (Tasks 6, 8, 9 use only `Echo`/`OpenMessages` + `SubsystemBoot`). Auth
  pre-auth assumption (§5b) — Global Constraints + Task 7 integration note.
  §5b in-protocol auth, §4 diff review, §6 `AiChat` buffer are explicitly
  **out of AI‑1** (AI‑2/3/4) — not gaps.
- **Placeholder scan:** the three "confirm at implementation" notes (ACP crate
  API, `opencode acp` flag, `Args`/test-boot harness names) are deliberate
  verification steps with exact grep/commands and a defined fallback — not
  hand-waves. No "TBD"/"add error handling"/"similar to Task N".
- **Type consistency:** `AiClientHandle`, `AiCmd`, `AiState`, `AiInboundEvent`,
  `ProviderConfig`, `Connection::{spawn,request}`, `session::{handshake,prompt,
  SessionId}`, `Frame`, `make_handler`, `register_ai_ex_commands`, `install` —
  names + signatures match across Tasks 1–9.
