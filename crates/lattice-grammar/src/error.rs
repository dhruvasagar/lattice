use thiserror::Error;

use lattice_core::CoreError;
use lattice_protocol::ProtocolError;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("unknown command id")]
    UnknownCommand,

    #[error("command kind mismatch: expected {expected}, got {actual}")]
    KindMismatch {
        expected: &'static str,
        actual: &'static str,
    },

    #[error("missing target for operator")]
    MissingTarget,

    #[error("invalid args for command: {0}")]
    InvalidArgs(&'static str),

    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

pub type GrammarResult<T> = Result<T, CommandError>;
