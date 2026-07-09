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

pub use commands::{parse_no_args, parse_rest_as_text};
pub use diff_review::{DiffReviewRequest, review_diff};
pub use error::{AgentError, Result};
