use thiserror::Error;

use lattice_protocol::ProtocolError;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("nothing to undo")]
    NothingToUndo,

    #[error("nothing to redo")]
    NothingToRedo,

    #[error("document has no path; use save_as")]
    NoPath,

    /// A long-running operation was interrupted by a flipped
    /// [`lattice_protocol::CancellationToken`]. Bubbles up from
    /// the search hot loops so callers (grammar dispatcher,
    /// substitute) can map it to their domain-specific error.
    #[error("operation cancelled")]
    Cancelled,
}

pub type CoreResult<T> = Result<T, CoreError>;
