//! `lattice-ai` — lattice as an ACP (Agent Client Protocol) client.
//!
//! Spawns an AI coding agent (opencode, and later claude-code-acp / gemini) as
//! a stdio subprocess and drives it over JSON-RPC. Architecturally a network
//! peer like `lattice-lsp`; runs no agent in-process. See
//! `docs/dev/architecture/ai-agent-protocol.md`.

pub mod commands;
pub mod connection;
pub mod error;
pub mod handle;
pub mod install;
pub mod providers;
pub mod session;
pub mod supervisor;

// AG-3: the per-process agent-log substrate (AiLogger, LogRing, SessionKey,
// AiLogPushed, the `*ai:<provider>:<index>*` buffer names, and AiLogMode)
// moved into the protocol-neutral `lattice-agent` port under `log/`, so the
// MCP adapter can eventually log through the same machinery. Re-exported
// here verbatim so this crate's public API (and `lattice-host`, which reads
// `lattice_ai::AiLogger` from the `ServiceRegistry`) is unchanged -- a
// re-export does not change a type's identity, so the `TypeId`-keyed lookup
// still resolves to what `install` registered.
pub use commands::register_ai_ex_commands;
pub use connection::{Connection, SessionId};
pub use error::{AiError, Result};
pub use handle::{AiClientHandle, AiState};
pub use install::install;
pub use lattice_agent::AiLogMode;
pub use lattice_agent::{
    AiLogEventPublisher, AiLogLevel, AiLogPushed, AiLogRecord, AiLogSource, AiLogger, LogRing,
    SessionKey, format_ai_log_line, level_tag,
};
pub use lattice_agent::{ai_log_name, parse_ai_log_name};
