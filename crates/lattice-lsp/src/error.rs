//! Error surface for the LSP client. Kept narrow and shape-rich
//! so the editor can branch on the failure (UI surface) instead
//! of pattern-matching error strings.
//!
//! Mirrors the `RuntimeError` shape from `lattice-runtime`:
//! protocol-level errors (`Busy`, `ActorGone`, `Cancelled`) are
//! distinct from server-reported errors (`ResponseError` from
//! the wire), which are distinct from transport / spawn failures
//! (`Transport`, `Codec`).

use thiserror::Error;

use crate::codec::CodecError;
use crate::framing::FrameError;
use crate::jsonrpc::ResponseError;
use crate::transport::TransportError;

/// All failure modes a `ServerHandle` caller can observe.
#[derive(Debug, Error)]
pub enum LspError {
    /// Failed to spawn the server binary or capture its stdio.
    /// Usually the binary isn't on PATH or isn't executable.
    #[error("transport: {0}")]
    Transport(#[from] TransportError),

    /// Codec-level failure -- bad framing, mid-message EOF,
    /// invalid JSON-RPC body. These tear the transport down;
    /// the supervisor may restart.
    #[error("codec: {0}")]
    Codec(#[from] CodecError),

    /// Frame-header level failure. Distinct from `Codec` so the
    /// supervisor can decide framing errors are unrecoverable
    /// (server is sending garbage) while body errors might be a
    /// version mismatch (server speaks an older spec).
    #[error("framing: {0}")]
    Framing(#[from] FrameError),

    /// The server returned a JSON-RPC error response. Carries
    /// the structured `ResponseError` so callers can branch on
    /// `error_codes::REQUEST_CANCELLED` etc.
    #[error("server error {}: {}", .0.code, .0.message)]
    Server(ResponseError),

    /// Outbound request failed because the actor's mailbox is
    /// closed (the actor task has shut down).
    #[error("server actor is no longer running")]
    ActorGone,

    /// The actor task accepted the request but the response
    /// `oneshot::Sender` was dropped without sending. Means the
    /// actor task panicked or shut down between accepting the
    /// request and dispatching the response.
    #[error("server actor dropped the response without sending")]
    ResponseDropped,

    /// The request was cancelled (either by the client via
    /// `$/cancelRequest`, by content-modification supersession,
    /// or by an explicit shutdown).
    #[error("request cancelled")]
    Cancelled,

    /// Server failed the initialize handshake. Distinct from
    /// `Server` so the actor knows the server is unusable and
    /// not to send further requests.
    #[error("initialize handshake failed: {0}")]
    HandshakeFailed(String),

    /// Server response could not be deserialized into the
    /// requested type. The caller asked for `T`, the server sent
    /// JSON that doesn't match `T`'s shape. Indicates either a
    /// spec mismatch or a buggy server.
    #[error("response deserialization failed: {0}")]
    ResponseDecode(#[source] serde_json::Error),

    /// Server expected the client to be initialized but got a
    /// request before initialize completed. Should be
    /// unreachable from outside the actor, but the path is here
    /// in case a misbehaving call sneaks past gating.
    #[error("server is not yet initialized")]
    NotInitialized,
}

impl LspError {
    /// True iff the failure is recoverable by retrying the
    /// request. `Cancelled` and `Busy`-equivalent (mailbox closed)
    /// are NOT retryable; transport errors are not retryable until
    /// the supervisor brings the server back up.
    pub fn is_retryable(&self) -> bool {
        matches!(self, LspError::Cancelled)
    }

    /// True iff the failure means the server is dead from this
    /// client's perspective. The supervisor should respawn.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            LspError::ActorGone
                | LspError::Transport(_)
                | LspError::Codec(_)
                | LspError::Framing(_)
                | LspError::HandshakeFailed(_)
        )
    }
}

/// Convenience alias so the rest of the crate doesn't have to
/// re-spell `Result<T, LspError>` everywhere.
pub type LspResult<T> = Result<T, LspError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_fatal_vs_retryable() {
        let fatal = LspError::ActorGone;
        assert!(fatal.is_fatal());
        assert!(!fatal.is_retryable());

        let cancelled = LspError::Cancelled;
        assert!(!cancelled.is_fatal());
        assert!(cancelled.is_retryable());
    }

    #[test]
    fn server_error_carries_structured_payload() {
        let e = LspError::Server(ResponseError {
            code: -32601,
            message: "method not found".into(),
            data: None,
        });
        assert!(!e.is_fatal());
        assert_eq!(e.to_string(), "server error -32601: method not found");
    }
}
