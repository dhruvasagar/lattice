//! `lattice-ai` — lattice as an ACP (Agent Client Protocol) client.
//!
//! Spawns an AI coding agent (opencode, and later claude-code-acp / gemini) as
//! a stdio subprocess and drives it over JSON-RPC. Architecturally a network
//! peer like `lattice-lsp`; runs no agent in-process. See
//! `docs/dev/architecture/ai-agent-protocol.md`.

pub mod ai_log;
pub mod buffer_names;
pub mod commands;
pub mod connection;
pub mod error;
pub mod handle;
pub mod modes;
pub mod providers;
pub mod session;
pub mod supervisor;

pub use ai_log::{
    AiLogEventPublisher, AiLogLevel, AiLogPushed, AiLogRecord, AiLogSource, AiLogger, LogRing,
    SessionKey, format_ai_log_line, level_tag,
};
pub use buffer_names::{ai_log_name, parse_ai_log_name};
pub use commands::register_ai_ex_commands;
pub use connection::{Connection, SessionId};
pub use error::{AiError, Result};
pub use handle::{AiClientHandle, AiState};
pub use modes::AiLogMode;
