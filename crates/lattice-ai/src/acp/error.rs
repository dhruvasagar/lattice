//! Error surface for the `lattice-ai` ACP client.

/// Errors surfaced by the ACP client (transport, protocol, process lifecycle).
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// The provider process failed to spawn or exited unexpectedly.
    #[error("provider process error: {0}")]
    Process(String),
    /// A transport-level failure (stdio closed, framing error).
    #[error("transport error: {0}")]
    Transport(String),
    /// The agent returned a JSON-RPC error or an unexpected frame.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, AiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_prefixed() {
        let e = AiError::Transport("eof".into());
        assert_eq!(e.to_string(), "transport error: eof");
    }
}
