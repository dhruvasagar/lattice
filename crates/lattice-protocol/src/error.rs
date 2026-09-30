//! Protocol-level error type.
//!
//! Crate-level errors compose with this via `thiserror::Error` `#[from]`.

use thiserror::Error;

use crate::ids::DocumentId;
use crate::position::Position;

/// A request that is structurally well-formed but does not fit the document
/// it addresses. Crate errors wrap it (`lattice_core`'s `CoreError::Protocol`)
/// rather than redefine these cases.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// No document with this id exists (it was never opened, or was closed).
    #[error("unknown document {0}")]
    UnknownDocument(DocumentId),

    /// A [`Position`] names a line past the end of the document, or a byte
    /// past the end of its line. `lattice_core::Buffer` raises it on edit
    /// and position conversion.
    #[error("position {position:?} is out of bounds (document has {line_count} lines)")]
    PositionOutOfBounds {
        /// The offending position.
        position: Position,
        /// How many lines the document actually has.
        line_count: u32,
    },

    /// A write was computed against an older document version than the
    /// current one (optimistic concurrency).
    #[error("stale version: client supplied {client}, document is at {actual}")]
    StaleVersion {
        /// The version the caller based its request on.
        client: u64,
        /// The document's current version.
        actual: u64,
    },

    /// A [`Range`](crate::Range) is malformed — typically `end` before
    /// `start`. The payload is a fixed, human-readable reason.
    #[error("invalid range: {0}")]
    InvalidRange(&'static str),
}

/// `Result` specialised to [`ProtocolError`].
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
