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
    /// surfaced by the handshake). Boxed because `tungstenite::Error` is a
    /// ~136-byte enum; boxing keeps `ClaudeCodeError` small so every
    /// `Result<_, ClaudeCodeError>` stays cheap to move (clippy
    /// `result_large_err`). The manual `From` below preserves `?`
    /// ergonomics on a bare `tungstenite::Error`.
    #[error("websocket error: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),

    /// JSON (de)serialization failure on the wire.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The OS random source was unavailable when minting an auth token.
    #[error("random source unavailable: {0}")]
    Random(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for ClaudeCodeError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        ClaudeCodeError::WebSocket(Box::new(e))
    }
}

/// Crate-local result alias.
pub type Result<T> = std::result::Result<T, ClaudeCodeError>;
