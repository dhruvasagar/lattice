# Agent integration — implementation plan (slices AG‑0 … AG‑5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the editor-capability surface shared by lattice's two agent
integrations into a new `lattice-agent` crate, then fold `lattice-claude-code`
into `lattice-ai` as a feature-gated MCP adapter — without changing what any
user sees.

**Architecture:** `lattice-claude-code` (MCP server; the agent dials in) and
`lattice-ai` (ACP client; lattice drives the agent) are inverted transports over
the *same* editor-capability surface: read the selection, write a file, get the
user's verdict on a diff. That surface becomes `EditorAccess` in a
protocol-neutral `lattice-agent` crate. MCP serves it; ACP serves it and
additionally drives a conversation. See
`docs/dev/architecture/agent-integration.md`.

**Tech Stack:** Rust, tokio, `lattice_diff::ProgrammaticDiffBus`,
`lattice_mode::{SubsystemBoot, InboundBus}`, `lattice_grammar::Effect`, cargo
features.

## Global Constraints

- **Behavior-preserving.** No user-visible change in AG‑0 … AG‑5. `:claude`,
  `:claude-send`, `:claude-interrupt`, `:claude-code-start`, `:claude-code-stop`,
  `:opencode`, `:ai-prompt`, `:ai-stop`, `:ai-log` all keep their names and
  semantics. Command renaming is a separate, later slice.
- **The 73 `lattice-claude-code` tests are the safety net.** They must pass
  unchanged through AG‑1 and AG‑2. A red test means the port is wrong; stop.
- **`lattice-agent` must not depend on** `agent-client-protocol`,
  `tokio-tungstenite`, `dirs`, `getrandom`, or `lattice-lsp`. It *may* depend on
  `lattice-protocol`, `lattice-mode`, `lattice-grammar`, `lattice-runtime`,
  `lattice-core`, `lattice-diff`, `lattice-config`, `linkme`.
- **Mode ownership.** Zero new `Editor` fields. Zero new host `Action` variants.
  Zero new `Effect` variants (all needed variants already exist).
- **Do NOT extract a shared supervisor.** `AiClientHandle` (2 fields,
  `impl ::spawn`) and `ClaudeCodeServerHandle` (10 fields, free-fn `spawn`) share
  an idea, not code. Design fragment §6 and §10 record why.
- `edition`, `rust-version`, `license`, and `[lints] workspace = true` are
  inherited from the workspace. Copy `lattice-ai/Cargo.toml`'s `[package]` block
  verbatim, changing only `name`.
- Workspace `Cargo.toml` `members` is **intro-order, tab-indented — not
  alphabetical.** Append `"crates/lattice-agent"` next to `lattice-claude-code`.
- `cargo fmt -p <crate>` reflows ~70 files of pre-existing branch drift. **Never
  run it.** Format touched lines by hand; verify with
  `rustfmt --edition 2024 --check <file>`.
- TDD: every task is red → green → commit.

## Slice roadmap

| Slice | Deliverable | Status |
|-------|-------------|--------|
| **AG‑0** | `lattice-agent` crate + `AgentError` + `parse_no_args`; both crates consume it | ⬜ not started |
| **AG‑1** | `diff_review` extracted; `openDiff` becomes MCP marshalling over it | ⬜ not started |
| **AG‑2** | `EditorAccess` (reads + writes + state cache) extracted | ⬜ not started |
| **AG‑3** | `AiLogger` / `LogRing` / `SessionKey` / `AiLogPushed` / buffer names / `AiLogMode` → `lattice-agent` | ⬜ not started |
| **AG‑4** | Fold `lattice-claude-code` into `lattice-ai/mcp/`; delete the crate; one boot line | ⬜ not started |
| **AG‑5** | Restructure ACP code into `lattice-ai/acp/`; feature-gate both adapters | ⬜ not started |

**Ordering rationale.** AG‑1 comes first among the extractions because it is the
highest-value move (it *is* AI‑2's deliverable) and the smallest self-contained
one. AG‑1 and AG‑2 deliberately **do not move any crate** — the port is proven
against 73 existing tests while `lattice-claude-code` still exists, so a red test
isolates to the port rather than tangling with a file move, a feature gate, and a
boot-line change. Only then does AG‑4 relocate code whose behavior is already
pinned.

Task-level detail below covers **AG‑0 and AG‑1**, the slices executed next. AG‑2
… AG‑5 carry slice-level contracts and exit criteria; each gets its own
task-level plan when it starts (the pattern `ai-agent-protocol.md` used for
AI‑1).

---

## Task 0: Create the `lattice-agent` crate skeleton

**Files:**
- Create: `crates/lattice-agent/Cargo.toml`
- Create: `crates/lattice-agent/src/lib.rs`
- Create: `crates/lattice-agent/src/error.rs`
- Modify: `Cargo.toml` (workspace `members`)

**Interfaces:**
- Produces: `lattice_agent::{AgentError, Result}`.

`AgentError` is a *port-level* error, distinct from each adapter's protocol
error (`AiError`, `ClaudeCodeError`), which stay where they are.

- [ ] **Step 1: Write the failing test**

Create `crates/lattice-agent/src/error.rs`:

```rust
//! Error surface for the agent capability port.

/// Failures raised by [`crate`]'s port operations. Distinct from an adapter's
/// protocol error: this describes the *editor* refusing or failing, never a
/// wire fault.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Nothing is draining the bus on the host side — a boot misconfiguration,
    /// or the editor is shutting down.
    #[error("editor not reachable: {0}")]
    Bus(String),
    /// A review's reply channel dropped before the user decided (the diff was
    /// closed, the session died, the editor went away).
    #[error("cancelled: {0}")]
    Cancelled(String),
    /// A read or write against the editor failed.
    #[error("editor io error: {0}")]
    Io(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, AgentError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_prefixed() {
        assert_eq!(
            AgentError::Bus("no receiver".into()).to_string(),
            "editor not reachable: no receiver"
        );
        assert_eq!(
            AgentError::Cancelled("diff closed".into()).to_string(),
            "cancelled: diff closed"
        );
        assert_eq!(
            AgentError::Io("read failed".into()).to_string(),
            "editor io error: read failed"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p lattice-agent`
Expected: FAIL — `error: package ID specification 'lattice-agent' did not match any packages`

- [ ] **Step 3: Write the minimal implementation**

Create `crates/lattice-agent/Cargo.toml`:

```toml
[package]
name = "lattice-agent"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
thiserror = { workspace = true }

[lints]
workspace = true
```

Create `crates/lattice-agent/src/lib.rs`:

```rust
//! `lattice-agent` — the editor-capability port that lattice's agent
//! integrations are built on.
//!
//! lattice integrates coding agents over two inverted transports: an MCP
//! server the agent dials into (Claude Code) and an ACP client that spawns and
//! drives the agent (opencode). Transport direction is not what the code is
//! made of — "give me the selection", "write this file", "ask the user to
//! approve this diff" mean one thing regardless of who opened the socket.
//!
//! This crate owns that surface. It carries **no agent wire protocol**: no
//! `agent-client-protocol`, no `tokio-tungstenite`, no `lattice-lsp`. Adapters
//! live in `lattice-ai`. See `docs/dev/architecture/agent-integration.md`.

pub mod error;

pub use error::{AgentError, Result};
```

Modify the workspace `Cargo.toml` `members` list — the list is **intro-order and
tab-indented**, so insert next to the other agent crates rather than
alphabetically:

```toml
	"crates/lattice-diff",
	"crates/lattice-agent",
	"crates/lattice-claude-code",
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lattice-agent`
Expected: PASS — `test result: ok. 1 passed`

- [ ] **Step 5: Verify fmt and clippy on the new files only**

Run:
```bash
rustfmt --edition 2024 --check crates/lattice-agent/src/lib.rs crates/lattice-agent/src/error.rs
cargo clippy -p lattice-agent --all-targets
```
Expected: no output from rustfmt; no clippy warnings attributed to `lattice-agent`.

- [ ] **Step 6: Commit**

```bash
git add crates/lattice-agent Cargo.toml
git commit -m "feat(agent): AG-0 lattice-agent crate skeleton + AgentError

The editor-capability port both agent adapters will consume. AgentError is
port-level (the editor failed or refused), distinct from each adapter's
protocol error, which stays put."
```

---

## Task 1: Move `parse_no_args` into `lattice-agent`

**Files:**
- Create: `crates/lattice-agent/src/commands.rs`
- Modify: `crates/lattice-agent/src/lib.rs`
- Modify: `crates/lattice-ai/src/commands.rs:25-34` (delete the local copy, import instead)
- Modify: `crates/lattice-claude-code/src/commands.rs:33-42` (same)
- Modify: `crates/lattice-ai/Cargo.toml`, `crates/lattice-claude-code/Cargo.toml`

**Interfaces:**
- Consumes: `lattice_agent::{AgentError, Result}` (Task 0) — not used here, but the crate must exist.
- Produces: `lattice_agent::commands::parse_no_args(rest: &str, bang: bool) -> GrammarResult<Args>`

The two copies are **byte-identical today** (verified with `diff`). `lattice-ai`
additionally has `parse_rest_as_text`, which `lattice-claude-code` does not —
move that too, since `:ai-log` and `:ai-prompt` both use it and a future
`:claude-send` variant would.

> **On TDD for a code move.** The implementation already exists and is already
> tested indirectly. The discipline that matters here is *the tests must exist
> and must fail before the crate can build them* — so Step 1 lands the moved
> function together with tests that pin it directly (it had none of its own),
> and the red state is a compile failure from the missing dependency. This is
> the same fold `ai-agent-protocol.md` used: test lands with impl, in one commit.

- [ ] **Step 1: Write the tests and the moved function**

Create `crates/lattice-agent/src/commands.rs`:

```rust
//! Ex-command parsing helpers shared by every agent adapter.

use lattice_grammar::args::Args;
use lattice_grammar::error::{CommandError, GrammarResult};

/// Reject any trailing characters; these commands take no arguments.
pub fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs(
            "trailing characters after command".into(),
        ))
    }
}

/// Take the rest of the line verbatim (trimmed) as a single string arg.
pub fn parse_rest_as_text(rest: &str, _bang: bool) -> GrammarResult<Args> {
    Ok(Args::String(rest.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_accepts_empty_and_whitespace() {
        assert_eq!(parse_no_args("", false).expect("empty is ok"), Args::None);
        assert_eq!(
            parse_no_args("   ", false).expect("whitespace is ok"),
            Args::None
        );
    }

    #[test]
    fn no_args_rejects_trailing_characters() {
        assert!(parse_no_args("junk", false).is_err());
    }

    #[test]
    fn rest_as_text_trims_and_keeps_inner_spaces() {
        assert_eq!(
            parse_rest_as_text("  hello  world  ", false).expect("always ok"),
            Args::String("hello  world".to_string())
        );
        assert_eq!(
            parse_rest_as_text("", false).expect("always ok"),
            Args::String(String::new())
        );
    }
}
```

Then declare it in `crates/lattice-agent/src/lib.rs`, after `pub mod error;`:

```rust
pub mod commands;

pub use commands::{parse_no_args, parse_rest_as_text};
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p lattice-agent commands`
Expected: FAIL to compile — `error[E0432]: unresolved import 'lattice_grammar'`,
because `lattice-agent` has no `lattice-grammar` dependency yet.

- [ ] **Step 3: Add the dependency that makes it compile**

Add to `crates/lattice-agent/Cargo.toml` under `[dependencies]`:

```toml
lattice-grammar = { path = "../lattice-grammar" }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lattice-agent`
Expected: PASS — `test result: ok. 4 passed`

- [ ] **Step 5: Point both consumers at it**

In `crates/lattice-ai/Cargo.toml`, add under `[dependencies]` (after the other
`lattice-*` path deps):

```toml
lattice-agent = { path = "../lattice-agent" }
```

Same line in `crates/lattice-claude-code/Cargo.toml`.

In `crates/lattice-ai/src/commands.rs`, delete the local `parse_no_args` (lines
25‑34) and `parse_rest_as_text` (lines 36‑39), then add to the import block:

```rust
use lattice_agent::{parse_no_args, parse_rest_as_text};
```

Remove `CommandError` and `GrammarResult` from the `lattice_grammar::error`
import if nothing else in the file uses them (check first — the `apply` closures
return `GrammarResult<Effect>`, so `GrammarResult` almost certainly stays and
`CommandError` almost certainly goes).

In `crates/lattice-claude-code/src/commands.rs`, delete the local `parse_no_args`
(lines 33‑42) and add:

```rust
use lattice_agent::parse_no_args;
```

Apply the same import cleanup.

- [ ] **Step 6: Run every affected suite**

Run:
```bash
cargo test -p lattice-agent -p lattice-ai -p lattice-claude-code
```
Expected: `lattice-agent` 4 passed; `lattice-ai` 41 passed / 2 ignored;
`lattice-claude-code` 73 passed. **Zero failures. The 73 is the safety net.**

- [ ] **Step 7: Verify fmt and clippy**

Run:
```bash
rustfmt --edition 2024 --check \
  crates/lattice-agent/src/commands.rs \
  crates/lattice-agent/src/lib.rs \
  crates/lattice-ai/src/commands.rs \
  crates/lattice-claude-code/src/commands.rs
cargo clippy -p lattice-agent -p lattice-ai -p lattice-claude-code --all-targets
```
Expected: no rustfmt output; no new clippy warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/lattice-agent crates/lattice-ai crates/lattice-claude-code
git commit -m "refactor(agent): AG-0 hoist parse_no_args into lattice-agent

Byte-identical in both crates before this. parse_rest_as_text moves with it.
73 claude-code + 41 ai tests unchanged and green."
```

---

## Task 2: Extract `diff_review` — the port's headline seam

**Files:**
- Create: `crates/lattice-agent/src/diff_review.rs`
- Modify: `crates/lattice-agent/src/lib.rs`
- Modify: `crates/lattice-agent/Cargo.toml` (add `lattice-diff`, `tokio`)
- Modify: `crates/lattice-claude-code/src/diff.rs` (becomes MCP marshalling)

**Interfaces:**
- Consumes: `lattice_agent::{AgentError, Result}`.
- Produces:
  - `lattice_agent::diff_review::DiffReviewRequest { old_file_path: PathBuf, new_file_path: PathBuf, new_contents: String, tab_name: String, origin_session: u64 }`
  - `lattice_agent::diff_review::review_diff(bus: &ProgrammaticDiffBus, req: DiffReviewRequest) -> Result<DiffOutcome>`

**Why this is the load-bearing slice.** `review_diff` *is* AI‑2's deliverable.
`lattice-claude-code/src/diff.rs:38-99` already builds the
`ProgrammaticDiffRequest`, sends it on the bus, and awaits the `DiffOutcome`
with no timeout. Extracting it means ACP's `session/request_permission` maps
onto working, tested code instead of a second implementation racing the same bus.

**Contract notes, from the real types:**
- `ProgrammaticDiffBus = InboundBus<ProgrammaticDiffRequest>`; `send` returns
  `Result<(), ProgrammaticDiffRequest>` — the item comes *back* on a dropped
  receiver.
- `DiffOutcome` is `#[non_exhaustive]` with `Accept` and `Reject`. Never write an
  exhaustive match.
- A dropped `oneshot` sender means the session was cancelled. The MCP caller maps
  that to a reject so the agent never hangs. The port surfaces it as
  `AgentError::Cancelled` and lets each adapter decide — MCP still rejects; ACP
  will answer `reject_once`.
- `ReviewGuard` / `ReviewHandle` live in `claude-code/src/status.rs` and are a
  **modeline concern, not a port concern.** They stay in the adapter, wrapped
  around the `review_diff` call.

- [ ] **Step 1: Write the failing tests and the extracted function**

Create `crates/lattice-agent/src/diff_review.rs`, and declare it in
`crates/lattice-agent/src/lib.rs`:

```rust
pub mod diff_review;

pub use diff_review::{DiffReviewRequest, review_diff};
```

```rust
//! The diff-review seam: propose an edit, block until the user rules on it.
//!
//! This is the one operation both agent protocols share verbatim. MCP's
//! `openDiff` and ACP's `session/request_permission` are two encodings of it.
//! The producer/awaiter lives here; the host owns the matching receiver and
//! resolves the reply when the user runs `:diff-accept` / `:diff-reject`.

use std::path::PathBuf;

use lattice_diff::subsystem::DiffOutcome;
use lattice_diff::{ProgrammaticDiffBus, ProgrammaticDiffRequest};
use tokio::sync::oneshot;

use crate::error::{AgentError, Result};

/// A proposed edit awaiting the user's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReviewRequest {
    /// Baseline path; its on-disk content is the left (read-only) side.
    pub old_file_path: PathBuf,
    /// Path the proposed content carries. An Accept writes the right side here.
    /// Usually equals `old_file_path` (an in-place edit).
    pub new_file_path: PathBuf,
    /// The proposed text — the editable right side.
    pub new_contents: String,
    /// Display label for the diff. Presentation only; teardown keys on
    /// `origin_session`.
    pub tab_name: String,
    /// The originating agent session. Tags the diff so a later session-scoped
    /// close from *this* session tears it down. `0` means "no origin".
    pub origin_session: u64,
}

/// Send `req` to the editor and block until the user resolves it.
///
/// No timeout: the user reviews at their own pace. A dropped reply channel
/// means the session was cancelled (the diff was closed, `:diffoff`, the editor
/// went away) and surfaces as [`AgentError::Cancelled`] — never a hang.
pub async fn review_diff(bus: &ProgrammaticDiffBus, req: DiffReviewRequest) -> Result<DiffOutcome> {
    let (tx, rx) = oneshot::channel::<DiffOutcome>();
    let request = ProgrammaticDiffRequest {
        old_file_path: req.old_file_path,
        new_file_path: req.new_file_path,
        new_contents: req.new_contents,
        tab_name: req.tab_name,
        origin_session: req.origin_session,
        response: tx,
    };
    if bus.send(request).is_err() {
        return Err(AgentError::Bus("programmatic diff receiver is gone".into()));
    }
    rx.await
        .map_err(|_| AgentError::Cancelled("diff review was dismissed".into()))
}

#[cfg(test)]
mod tests {
    use lattice_mode::inbound::make_inbound_raw;
    use std::sync::Arc;
    use tokio::sync::Notify;

    use super::*;

    fn req(session: u64) -> DiffReviewRequest {
        DiffReviewRequest {
            old_file_path: PathBuf::from("/tmp/a.rs"),
            new_file_path: PathBuf::from("/tmp/a.rs"),
            new_contents: "fn main() {}\n".to_string(),
            tab_name: "openDiff".to_string(),
            origin_session: session,
        }
    }

    /// The request reaches the host with every field intact, and the host's
    /// verdict comes back to the caller.
    #[tokio::test]
    async fn accept_round_trips_through_the_bus() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));

        let host = tokio::spawn(async move {
            let request = rx.recv().await.expect("a request should arrive");
            assert_eq!(request.old_file_path, PathBuf::from("/tmp/a.rs"));
            assert_eq!(request.new_contents, "fn main() {}\n");
            assert_eq!(request.origin_session, 7);
            request
                .response
                .send(DiffOutcome::Accept)
                .expect("caller is still awaiting");
        });

        let outcome = review_diff(&bus, req(7)).await.expect("accept");
        assert_eq!(outcome, DiffOutcome::Accept);
        host.await.expect("host task");
    }

    #[tokio::test]
    async fn reject_round_trips_through_the_bus() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        tokio::spawn(async move {
            let request = rx.recv().await.expect("a request should arrive");
            let _ = request.response.send(DiffOutcome::Reject);
        });
        assert_eq!(
            review_diff(&bus, req(1)).await.expect("reject"),
            DiffOutcome::Reject
        );
    }

    /// A dropped reply channel is a cancelled review, not a hang. This is the
    /// case that fires when the user closes the diff or the session dies.
    #[tokio::test]
    async fn dropped_reply_channel_is_cancelled_not_a_hang() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        tokio::spawn(async move {
            let request = rx.recv().await.expect("a request should arrive");
            drop(request); // the host gave up without answering
        });
        assert!(matches!(
            review_diff(&bus, req(1)).await,
            Err(AgentError::Cancelled(_))
        ));
    }

    /// No host draining the bus at all — a boot misconfiguration.
    #[tokio::test]
    async fn dropped_receiver_is_a_bus_error() {
        let (bus, rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        drop(rx);
        assert!(matches!(
            review_diff(&bus, req(1)).await,
            Err(AgentError::Bus(_))
        ));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lattice-agent diff_review`
Expected: FAIL to compile — `error[E0432]: unresolved import 'lattice_diff'`,
because the dependency is not yet in `Cargo.toml`.

- [ ] **Step 3: Add the dependencies that make it compile**

Add to `crates/lattice-agent/Cargo.toml` under `[dependencies]`:

```toml
lattice-diff = { path = "../lattice-diff" }
lattice-mode = { path = "../lattice-mode" }
tokio = { workspace = true, features = ["sync", "rt", "macros"] }
```

and a new `[dev-dependencies]` section before `[lints]`:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "time", "macros", "rt"] }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lattice-agent`
Expected: PASS — `test result: ok. 8 passed` (1 error + 3 commands + 4 diff_review).

- [ ] **Step 5: Rewrite `claude-code/src/diff.rs` as MCP marshalling over the port**

Replace the body of `open_diff` (currently `diff.rs:38-99`) so it parses MCP
arguments, delegates, and encodes the result. The argument parsing, the
`ReviewGuard`, and the `FILE_SAVED` / `DIFF_REJECTED` envelopes all stay here —
they are MCP's shape, not the port's.

```rust
use std::path::PathBuf;

use serde_json::{Value, json};

use lattice_agent::{AgentError, DiffReviewRequest, review_diff};
use lattice_diff::ProgrammaticDiffBus;
use lattice_diff::subsystem::DiffOutcome;

pub async fn open_diff(
    bus: Option<&ProgrammaticDiffBus>,
    args: &Value,
    conn_id: u64,
    review: &crate::status::ReviewHandle,
) -> Value {
    let Some(bus) = bus else {
        return error_result("openDiff unavailable: IDE server not fully initialized");
    };
    // `old_file_path` (baseline) + `new_file_contents` (proposed) are required;
    // `new_file_path` defaults to `old_file_path` (an in-place edit).
    let Some(old) = args.get("old_file_path").and_then(|v| v.as_str()) else {
        return error_result("openDiff: missing old_file_path");
    };
    let Some(contents) = args.get("new_file_contents").and_then(|v| v.as_str()) else {
        return error_result("openDiff: missing new_file_contents");
    };
    let new_path = args
        .get("new_file_path")
        .and_then(|v| v.as_str())
        .unwrap_or(old);
    let tab = args
        .get("tab_name")
        .and_then(|v| v.as_str())
        .unwrap_or("openDiff");

    let request = DiffReviewRequest {
        old_file_path: PathBuf::from(old),
        new_file_path: PathBuf::from(new_path),
        new_contents: contents.to_string(),
        tab_name: tab.to_string(),
        origin_session: conn_id,
    };

    // D-fix.6 follow-up: a review is now pending for the modeline badge. The
    // guard clears it on ANY exit below -- resolve, cancel, or the task being
    // dropped -- so the count can never leak high. The badge is a modeline
    // concern, so it stays adapter-side rather than moving into the port.
    let _review = review.begin();

    match review_diff(bus, request).await {
        Ok(DiffOutcome::Accept) => saved_result(),
        // Reject, a cancelled review (dropped sender), or any future
        // `DiffOutcome` variant (the enum is `#[non_exhaustive]`) -> "not
        // saved": a reject reply, so the agent never hangs.
        Ok(_) | Err(AgentError::Cancelled(_)) => rejected_result(tab),
        Err(_) => error_result("openDiff failed: editor not reachable"),
    }
}
```

Leave `saved_result`, `rejected_result`, `error_result`, and `text_result`
unchanged. Delete the now-unused `use tokio::sync::oneshot;` and
`use lattice_diff::ProgrammaticDiffRequest;` imports.

Note the one behavior difference this makes visible: previously a dropped
*receiver* (`bus.send` failed) produced `error_result`, and a dropped *sender*
produced `rejected_result`. The port distinguishes these as `Bus` vs `Cancelled`,
and the match above preserves both mappings exactly.

- [ ] **Step 6: Run the safety net**

Run: `cargo test -p lattice-claude-code`
Expected: **73 passed, 0 failed.** Any failure means the port changed behavior —
revert and diagnose before proceeding.

Then run: `cargo test -p lattice-agent -p lattice-ai -p lattice-claude-code`
Expected: 8 + (41 pass / 2 ignored) + 73, zero failures.

- [ ] **Step 7: Verify fmt and clippy**

Run:
```bash
rustfmt --edition 2024 --check \
  crates/lattice-agent/src/diff_review.rs \
  crates/lattice-agent/src/lib.rs \
  crates/lattice-claude-code/src/diff.rs
cargo clippy -p lattice-agent -p lattice-claude-code --all-targets
```
Expected: no output; no new warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/lattice-agent crates/lattice-claude-code/src/diff.rs
git commit -m "refactor(agent): AG-1 extract review_diff into lattice-agent

The one operation both agent protocols share verbatim: MCP's openDiff and
ACP's session/request_permission are two encodings of it. openDiff becomes
argument parsing + result envelopes over lattice_agent::review_diff.

The port distinguishes a dropped receiver (Bus) from a dropped reply channel
(Cancelled); openDiff maps both back to its existing envelopes, so behavior is
unchanged. The ReviewGuard modeline badge stays adapter-side.

This is AI-2's deliverable, landed early and proven by claude-code's 73 tests."
```

---

## AG‑2: Extract `EditorAccess` (reads + writes + state cache)

**Contract.** `lattice-agent` gains:

- `state_cache.rs` ← `claude-code/src/snapshot.rs`. The open-buffer set and
  active selection, fed by the generic event bus. Rename
  `ClaudeCodeReadState` → `EditorStateCache`, `ReadStateHandle` → `EditorStateHandle`.
- `editor_access.rs` ← the *editor-facing* halves of `claude-code/src/reads.rs`,
  `writes.rs`, and `inbound.rs`. A concrete clone-able `EditorAccess` struct:

```rust
pub struct EditorAccess {
    cache: EditorStateHandle,
    buffer_store: Option<lattice_mode::BufferStoreHandle>,
    diagnostics: Option<DiagnosticsSource>,
    writes: Option<lattice_mode::inbound::InboundBus<EditorWriteRequest>>,
    diff: Option<lattice_diff::ProgrammaticDiffBus>,
    workspace_folders: Vec<String>,
}
```

**Constraints specific to this slice:**

- **`serde_json::Value` must not appear in `lattice-agent`.** `reads.rs`'s tool
  fns return `Value` because that is MCP's `CallToolResult` envelope. The port
  returns Rust types (`Option<Selection>`, `Vec<OpenEditor>`, `bool`,
  `Vec<AgentDiagnostic>`); the MCP adapter keeps the `*_result` builders that
  wrap them into JSON. This is the line that decides whether the port is real or
  is MCP wearing a hat.
- **Diagnostics must not pull `lattice-lsp` into the port.** Define
  `pub trait DiagnosticsSource: Send + Sync` in `lattice-agent` with
  `for_uri(&str) -> Vec<AgentDiagnostic>` and `uris_with_diagnostics() -> Vec<String>`,
  and `pub type DiagnosticsSourceHandle = Arc<dyn DiagnosticsSource>`. The MCP
  adapter's `install` adapts `lattice_lsp::modes::DiagnosticsQueryHandle` to it.
  `AgentDiagnostic` is a neutral struct — do not re-export `lsp_types::Diagnostic`.
  *(This is the one trait the design permits, because a second implementation —
  the test fake — exists on day one, and the alternative is an LSP dependency in
  the port.)*
- `EditorWriteRequest` is `claude-code/src/inbound.rs`'s `ClaudeCodeInboundRequest`
  renamed; `InboundKind` moves verbatim. `make_handler` moves with it, still
  returning `Vec<Effect>` — the mapping to `Effect::OpenBufferAtColumn`,
  `SaveBuffer`, `CloseSessionDiffs`, `CloseAllSessionDiffs` is protocol-neutral.
- `boot.inbound::<T, H>(handler)` is the only inbound API available to a
  subsystem (`inbound_raw` is a host-only `BootContext` inherent method). The
  write bus stays handler-drained; the diff bus stays a host-registered service.

**Exit criteria:** `lattice-claude-code`'s 73 tests pass **unchanged**.
`cargo tree -p lattice-agent | grep -E 'lattice-lsp|tungstenite|agent-client-protocol'`
returns nothing. `grep -rn 'serde_json' crates/lattice-agent/src/` returns nothing.

**Risk:** `reads.rs` slices selection text out of the buffer store using
UTF‑16-aware helpers (`abs_offset`, `slice_selection`, `ordered`). These are
`lattice_protocol::position::Position` / `Selection` operations, protocol-neutral,
and move cleanly — but they are the fiddliest part of the slice and carry the
most subtle off-by-one risk. Move them with their tests.

---

## AG‑3: Move the log substrate into `lattice-agent`

**Contract.** `ai_log.rs`, `buffer_names.rs`, and `modes.rs` (`AiLogMode`) move
from `lattice-ai` to `lattice-agent`; `lattice-ai` re-exports them so its public
API is unchanged.

**Constraints:**
- The `register_event!(AiLogPushed, "ai.log-pushed", …)` invocation moves too,
  which means `lattice-agent` gains a direct `linkme` dependency and the
  declaring module needs `#![allow(unsafe_code)]`. The macro cannot be invoked
  from a `cfg(test)` module and its path must stay `lattice_protocol::register_event!`.
- **Do not rename the event or the buffers in this slice.** The event name stays
  `"ai.log-pushed"`; buffers stay `*ai:<provider>:<index>*`. Renaming is a
  behavior change (it breaks `:ai-log`'s name parser and `:describe-events`), and
  this slice is behavior-preserving. `AiLogMode` also reads `ai.log` /
  `ai.log_level` config options, which stay named as they are.
- `AiLogMode::on_activate` reaches `ctx.service::<AiLogger>()`. Registration
  moves to whichever `install` runs — unchanged in AG‑3, revisited in AG‑4.

**Exit criteria:** `lattice-ai` 41 tests pass unchanged; `lattice-ui-tui` 1565
and `lattice-ui-gpui` 28 pass (boot touches the mode registry).

**Deferred to a later, non-behavior-preserving slice:** once the MCP adapter also
logs, `*ai:…*` reads oddly for Claude Code and `ai.log` reads oddly as a global.
Renaming both, with a deprecation path, is its own slice.

---

## AG‑4: Fold `lattice-claude-code` into `lattice-ai/mcp/`

**Contract.**

- Move `server.rs`, `transport.rs`, `auth.rs`, `lockfile.rs`, `protocol.rs`,
  `dispatch.rs`, `notifications.rs`, `status.rs`, `commands.rs`, `modes.rs`, and
  the MCP-shaped remnant of `reads.rs` / `writes.rs` / `diff.rs` into
  `crates/lattice-ai/src/mcp/`.
- Move `error.rs`'s `ClaudeCodeError` to `mcp/error.rs`. It stays a distinct type
  from `AiError` and `AgentError`.
- Add cargo features to `lattice-ai`:

```toml
[features]
default = ["acp", "mcp"]
acp = ["dep:agent-client-protocol", "dep:tokio-util", "tokio/process"]
mcp = ["dep:tokio-tungstenite", "dep:dirs", "dep:getrandom", "dep:lattice-lsp", "tokio/net"]
```

  Make `agent-client-protocol`, `tokio-util`, `tokio-tungstenite`, `dirs`,
  `getrandom`, and `lattice-lsp` `optional = true`.
- `lattice_ai::install(&mut boot)` calls `mcp::install(boot)` and
  `acp::install(boot)` behind `#[cfg(feature = …)]`.
- `crates/lattice-host/src/editor_boot.rs`: **delete** the
  `lattice_claude_code::install(&mut boot);` line (~521) and its comment block.
  `lattice_ai::install(&mut boot);` (~525) now installs both. Update the comment.
- Delete `crates/lattice-claude-code/`; remove it from workspace `members`.
- Remove `lattice-claude-code` from `lattice-host/Cargo.toml` and
  `lattice-ui-tui/Cargo.toml` if present.

**Exit criteria:** all 73 former claude-code tests pass in their new home.
`cargo test -p lattice-ai --no-default-features --features acp` builds and passes
the ACP subset; `--no-default-features --features mcp` likewise. TUI 1565, GPUI
28, host suite green. `grep -rn "lattice_claude_code\|lattice-claude-code" crates/ docs/`
returns only historical references in docs.

**Risk — the highest of the plan.** This touches `editor_boot.rs`, deletes a
crate, and introduces feature gates in one change. Mitigation: AG‑1 through AG‑3
have already moved every line whose *behavior* could regress, so AG‑4 is a move
plus a `cfg`. Do the move and the feature gates as two commits, not one, and run
the full workspace suite between them.

---

## AG‑5: Restructure the ACP code into `lattice-ai/acp/`

**Contract.** `connection.rs`, `session.rs`, `providers.rs`, and the ACP half of
`supervisor.rs` / `handle.rs` move under `crates/lattice-ai/src/acp/`, gated on
`feature = "acp"`. `commands.rs` splits: `:opencode` / `:ai-prompt` / `:ai-stop`
are ACP-owned; `:ai-log` is port-owned (it opens a log buffer, which AG‑3 moved)
and stays unconditional.

**Constraint:** `Effect::OpenAiLog` and the three host methods behind it
(`snapshot_ai_sessions`, `open_ai_log_in_pane`, `do_open_ai_log`) are unchanged.
`do_open_ai_log` reads `AiLogger` via `self.services.get::<AiLogger>()`; after
AG‑3 that type path is `lattice_agent::AiLogger`, so `lattice-host` gains a
`lattice-agent` dependency and drops nothing.

**Exit criteria:** `lattice-ai` 41 tests pass; the `#[ignore]`d live
`opencode_supervisor_end_to_end` still passes when run manually; all four feature
combinations (`--no-default-features`, `acp`, `mcp`, `acp,mcp`) build.

---

## What this plan does NOT do

Named explicitly so a later reader does not mistake silence for oversight:

- **No shared supervisor.** See Global Constraints and design §6.
- **No command renaming.** `:claude*` and `:opencode` / `:ai-*` keep their names.
  The target (`:opencode`, `:claude`, `:gemini` as entry points; `:agent-prompt`,
  `:agent-stop`, `:agent-log`, `:agent-send` as shared verbs) is design §11's,
  and gets its own slice.
- **No logging for the MCP path.** Claude Code still logs via `tracing::debug!`
  only. Wiring it into the shared `AiLogger` is a follow-up, and it is the slice
  that should also rename `*ai:…*` buffers.
- **No AI‑2.** This plan *enables* AI‑2 by landing `review_diff` and
  `EditorAccess`; the ACP `session/request_permission` handler and the
  `fs/read_text_file` / `fs/write_text_file` handlers are AI‑2's own slice, which
  should be re-planned against the port once AG‑2 lands.
- **No `EditorAccess` trait.** Concrete struct; see design §2 and §10.

## Open risks

- **AG‑2 is the largest slice** and has no natural sub-slice boundary: the reads,
  the writes, and the state cache are mutually entangled through `ReadContext`.
  If it proves too big, split it as `AG‑2a` (state cache + reads) and `AG‑2b`
  (writes + inbound), with `EditorAccess` gaining its write methods in 2b.
- **The 73-test safety net covers the MCP path only.** `lattice-ai` has 41 tests
  and no diff-review path yet, so AG‑1's `review_diff` is proven by its own four
  new unit tests plus claude-code's integration coverage — not by an ACP consumer.
  The first real ACP consumer arrives in AI‑2.
- **`main` may move under this branch.** AG‑4 edits `editor_boot.rs` and AG‑5
  edits `lattice-host/dispatch.rs`, both busy files. Rebase on `main` between
  slices rather than at the end.
