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
}

pub type CoreResult<T> = Result<T, CoreError>;
