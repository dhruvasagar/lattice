//! `lattice-ai` — lattice as an ACP (Agent Client Protocol) client.
//!
//! Spawns an AI coding agent (opencode, and later claude-code-acp / gemini) as
//! a stdio subprocess and drives it over JSON-RPC. Architecturally a network
//! peer like `lattice-lsp`; runs no agent in-process. See
//! `docs/dev/architecture/ai-agent-protocol.md`.

pub mod ai_log;
pub mod connection;
pub mod error;
pub mod providers;
pub mod session;

pub use ai_log::{AiLogLevel, AiLogPushed, AiLogSource, AiLogger, SessionKey};
pub use connection::{Connection, SessionId};
pub use error::{AiError, Result};
