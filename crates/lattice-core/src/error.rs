//! The crate's error type, [`CoreError`], and its [`CoreResult`] alias.

use thiserror::Error;

use lattice_protocol::ProtocolError;

/// Every error `lattice-core` returns.
///
/// Callers mostly propagate it with `?` via [`CoreResult`]. The variants
/// worth matching on are [`CoreError::NothingToUndo`] /
/// [`CoreError::NothingToRedo`] (a user-facing no-op, not a failure) and
/// [`CoreError::Cancelled`] (a cooperative abort, not a failure either).
#[derive(Debug, Error)]
pub enum CoreError {
    /// A position or range was invalid for the buffer it was applied to
    /// (out of bounds, or `end < start`). Raised by
    /// [`Buffer::slice`](crate::Buffer::slice),
    /// [`Buffer::apply_edit`](crate::Buffer::apply_edit) and everything built
    /// on them.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// A filesystem read or write failed — [`Document::open`](crate::Document::open)
    /// (including a file that is not valid UTF-8) or a save.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// [`Document::undo`](crate::Document::undo) with an empty undo stack.
    #[error("nothing to undo")]
    NothingToUndo,

    /// [`Document::redo`](crate::Document::redo) with an empty redo stack.
    #[error("nothing to redo")]
    NothingToRedo,

    /// [`Document::save`](crate::Document::save) on a document with no path.
    #[error("document has no path; use save_as")]
    NoPath,

    /// A long-running operation was interrupted by a flipped
    /// [`lattice_protocol::CancellationToken`]. Bubbles up from
    /// the search hot loops so callers (grammar dispatcher,
    /// substitute) can map it to their domain-specific error.
    #[error("operation cancelled")]
    Cancelled,
}

/// `Result` specialised to [`CoreError`].
pub type CoreResult<T> = Result<T, CoreError>;
