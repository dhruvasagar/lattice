//! Error type for the Claude Code IDE peer.
//!
//! Per the design's graceful-degradation contract (§5): every WS-thread
//! failure logs-and-skips; a dropped host receiver yields a JSON-RPC
//! error to the agent, never a hang or panic. This enum is the recoverable
//! error currency on the connection-serving path.

use thiserror::Error;

/// Errors raised while serving an agent connection or managing the server.
#[derive(Debug, Error)]
pub enum ClaudeCodeError {
    /// Socket / lockfile I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// WebSocket transport / handshake failure (including auth rejection
    /// surfaced by the handshake).
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// JSON (de)serialization failure on the wire.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The OS random source was unavailable when minting an auth token.
    #[error("random source unavailable: {0}")]
    Random(String),
}

/// Crate-local result alias.
pub type Result<T> = std::result::Result<T, ClaudeCodeError>;
