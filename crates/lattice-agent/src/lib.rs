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

pub mod commands;

pub mod diff_review;

pub mod state_cache;

pub mod editor_access;

pub mod write_bus;

pub mod log;

pub use commands::{parse_no_args, parse_rest_as_text};
pub use diff_review::{DiffReviewRequest, review_diff};
pub use editor_access::{EditorAccess, OpenEditor, SelectionInfo};
pub use error::{AgentError, Result};
pub use state_cache::{EditorStateCache, EditorStateHandle};
pub use write_bus::{EditorWriteRequest, InboundKind, InboundReply, make_handler};

pub use log::ai_log::{
    AiLogEventPublisher, AiLogLevel, AiLogPushed, AiLogRecord, AiLogSource, AiLogger, LogRing,
    SessionKey, format_ai_log_line, level_tag,
};
pub use log::buffer_names::{ai_log_name, parse_ai_log_name};
pub use log::modes::AiLogMode;
