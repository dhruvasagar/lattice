//! `lattice-ai` — lattice's AI coding-agent integration layer.
//!
//! Owns both agent transports behind cargo features (default = both):
//!   - `acp`: an Agent Client Protocol client. Spawns an agent (opencode, and
//!     later claude-code-acp / gemini) as a stdio subprocess and drives it over
//!     JSON-RPC. Architecturally a network peer like `lattice-lsp`.
//!   - `mcp`: the Claude Code IDE peer (folded in by AG-4, see `mcp`). An
//!     external `claude` CLI dials into a loopback WebSocket and drives the
//!     editor over the same MCP contract as the VS Code / JetBrains plugins.
//!
//! Both share the protocol-neutral editor-capability port (`lattice-agent`) and
//! the transport-neutral AI-log substrate (below). Runs no agent in-process.
//! See `docs/dev/architecture/agent-integration.md`.

// Transport-neutral: `:ai-log` + `register_ai_log_command` live here always;
// the ACP lifecycle commands are gated inside on `feature = "acp"`.
pub mod commands;

// ACP (Agent Client Protocol) transport, gated on `feature = "acp"`.
#[cfg(feature = "acp")]
pub mod connection;
#[cfg(feature = "acp")]
pub mod error;
#[cfg(feature = "acp")]
pub mod handle;
#[cfg(feature = "acp")]
pub mod providers;
#[cfg(feature = "acp")]
pub mod session;
#[cfg(feature = "acp")]
pub mod supervisor;

// Unified boot entry point; wires the port-neutral log substrate always and
// each transport behind its own `#[cfg(feature = …)]`.
pub mod install;

// AG-4: the MCP adapter (formerly the `lattice-claude-code` crate) folded in as
// a submodule, gated on `feature = "mcp"`. The external `claude` CLI dials into
// a loopback WebSocket; this half serves the shared editor-capability port
// (`lattice-agent`) over that transport.
#[cfg(feature = "mcp")]
pub mod mcp;

// AG-3: the per-process agent-log substrate (AiLogger, LogRing, SessionKey,
// AiLogPushed, the `*ai:<provider>:<index>*` buffer names, and AiLogMode) lives
// in the protocol-neutral `lattice-agent` port under `log/`. Re-exported here
// verbatim so this crate's public API (and `lattice-host`, which reads
// `lattice_ai::AiLogger` from the `ServiceRegistry`) is unchanged -- a re-export
// does not change a type's identity, so the `TypeId`-keyed lookup still resolves
// to what `install` registered. Transport-neutral -> unconditional.
pub use install::install;
pub use lattice_agent::AiLogMode;
pub use lattice_agent::{
    AiLogEventPublisher, AiLogLevel, AiLogPushed, AiLogRecord, AiLogSource, AiLogger, LogRing,
    SessionKey, format_ai_log_line, level_tag,
};
pub use lattice_agent::{ai_log_name, parse_ai_log_name};

// ACP-transport public surface.
#[cfg(feature = "acp")]
pub use commands::register_ai_ex_commands;
#[cfg(feature = "acp")]
pub use connection::{Connection, SessionId};
#[cfg(feature = "acp")]
pub use error::{AiError, Result};
#[cfg(feature = "acp")]
pub use handle::{AiClientHandle, AiState};
