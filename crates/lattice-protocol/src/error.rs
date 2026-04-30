//! Protocol-level error type.
//!
//! Crate-level errors compose with this via `thiserror::Error` `#[from]`.

use thiserror::Error;

use crate::ids::DocumentId;
use crate::position::Position;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("unknown document {0}")]
    UnknownDocument(DocumentId),

    #[error("position {position:?} is out of bounds (document has {line_count} lines)")]
    PositionOutOfBounds {
        position: Position,
        line_count: u32,
    },

    #[error("stale version: client supplied {client}, document is at {actual}")]
    StaleVersion { client: u64, actual: u64 },

    #[error("invalid range: {0}")]
    InvalidRange(&'static str),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::ids::DocumentId;

    #[test]
    fn unknown_document_renders_id() {
        let err = ProtocolError::UnknownDocument(DocumentId::new(7));
        assert_eq!(format!("{err}"), "unknown document DocumentId#7");
    }

    #[test]
    fn position_out_of_bounds_includes_line_count() {
        let err = ProtocolError::PositionOutOfBounds {
            position: Position::new(99, 0),
            line_count: 3,
        };
        let msg = format!("{err}");
        assert!(msg.contains("99"), "msg = {msg}");
        assert!(msg.contains("3 lines"), "msg = {msg}");
    }

    #[test]
    fn stale_version_includes_both_versions() {
        let err = ProtocolError::StaleVersion {
            client: 4,
            actual: 7,
        };
        let msg = format!("{err}");
        assert!(msg.contains("4"));
        assert!(msg.contains("7"));
    }

    #[test]
    fn invalid_range_carries_static_reason() {
        let err = ProtocolError::InvalidRange("end < start");
        assert_eq!(format!("{err}"), "invalid range: end < start");
    }
}
