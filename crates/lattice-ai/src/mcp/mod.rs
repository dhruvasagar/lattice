//! `mcp` — lattice as the IDE side of the Claude Code agent protocol.
//!
//! The MCP adapter half of `lattice-ai` (AG‑4): an external `claude` CLI
//! connects over a loopback WebSocket and drives the editor (reading
//! selection / open buffers / diagnostics, opening files, proposing edits
//! as interactive diffs). This is the lattice analog of
//! `claude-code-ide.el` and the VS Code / JetBrains integrations — it
//! speaks the same WebSocket + MCP contract. Gated behind
//! `feature = "mcp"`.
//!
//! Architecturally this is a **network peer**, like `lattice-lsp` (an LSP
//! *client*): a loopback JSON-RPC connection to an external process. It
//! runs no agent in-process and adds no scripting surface, so it does not
//! touch the "WASM is the only extension substrate" rule. It shares the
//! protocol-neutral editor-capability port (`lattice-agent`) with the ACP
//! adapter in `super::acp`.
//!
//! See `docs/dev/architecture/ide-protocol.md` (design) +
//! `docs/dev/architecture/agent-integration.md` (the AG‑4 fold).

pub mod auth;
pub mod commands;
// I4 (openDiff): the blocking interactive-diff tool. Sends a
// `ProgrammaticDiffRequest` on the host-drained bus and awaits the user's
// verdict (no timeout) to shape the FILE_SAVED / DIFF_REJECTED reply.
pub mod diff;
pub mod dispatch;
pub mod error;
// BC.3b: the crate-owned `install(boot)` entry point — one Phase-B line in
// `editor_boot`, all wiring here.
pub mod install;
pub mod lockfile;
pub mod modes;
// I6: server-initiated notifications (selection_changed / didChangeActiveEditor)
// — a task coalesces SelectionsChanged + broadcasts frames to connected agents.
pub mod notifications;
pub mod protocol;
pub mod reads;
pub mod server;
// I7: the `claude-code` modeline status segment (running/port/conns) shown on
// the agent terminal's modeline. Mode-owned (registered by claude-code-mode).
pub mod status;
pub mod transport;
pub mod writes;

pub use commands::register_claude_code_ex_commands;
pub use error::{ClaudeCodeError, Result};
pub use install::install;
pub use modes::{ClaudeCodeMode, register_claude_code_modes};
pub use server::{ClaudeCodeServerHandle, ServerConfig, ServerState, spawn};
