//! `acp` — lattice as an Agent Client Protocol client.
//!
//! The ACP adapter half of `lattice-ai` (AG‑5): spawns an AI coding agent
//! (opencode, and later claude-code-acp / gemini) as a stdio subprocess and
//! drives it over JSON-RPC. Architecturally a network peer like `lattice-lsp`;
//! runs no agent in-process. Gated behind `feature = "acp"`.
//!
//! Shares the protocol-neutral editor-capability port (`lattice-agent`) and the
//! transport-neutral AI-log substrate with the MCP adapter in `super::mcp`. The
//! `:ai-log` command is NOT here -- it is port-owned and lives in the crate-root
//! `commands` module.

pub mod commands;
pub mod connection;
pub mod error;
pub mod handle;
pub mod install;
pub mod providers;
pub mod session;
pub mod supervisor;

pub use commands::register_ai_ex_commands;
pub use connection::{Connection, SessionId};
pub use error::{AiError, Result};
pub use handle::{AiClientHandle, AiState};
