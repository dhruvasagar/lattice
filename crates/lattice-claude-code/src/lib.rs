//! `lattice-claude-code` — lattice as the IDE side of the Claude Code
//! agent protocol.
//!
//! An external `claude` CLI connects over a loopback WebSocket and drives
//! the editor (reading selection / open buffers / diagnostics, opening
//! files, proposing edits as interactive diffs). This is the lattice
//! analog of `claude-code-ide.el` and the VS Code / JetBrains
//! integrations — it speaks the same WebSocket + MCP contract.
//!
//! Architecturally this is a **network peer**, like `lattice-lsp` (an LSP
//! *client*): a loopback JSON-RPC connection to an external process. It
//! runs no agent in-process and adds no scripting surface, so it does not
//! touch the "WASM is the only extension substrate" rule.
//!
//! See `docs/dev/architecture/ide-protocol.md` (design) +
//! `docs/dev/operations/slice-plans/ide-protocol.md` (sequencing). This
//! is the I1 skeleton: transport + MCP envelope + `initialize` /
//! `tools/list` / `prompts/list`; the tools themselves are stubbed (reads
//! land in I2, writes I3, `openDiff` I4).

pub mod auth;
pub mod commands;
pub mod dispatch;
pub mod error;
pub mod inbound;
pub mod lockfile;
pub mod modes;
pub mod protocol;
pub mod reads;
pub mod server;
pub mod snapshot;
pub mod transport;
pub mod writes;

pub use commands::register_claude_code_ex_commands;
pub use error::{ClaudeCodeError, Result};
pub use modes::{ClaudeCodeMode, register_claude_code_modes};
pub use server::{ClaudeCodeServerHandle, ServerConfig, ServerState, spawn};
