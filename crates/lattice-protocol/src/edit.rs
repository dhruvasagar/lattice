//! Edit primitives.
//!
//! An `Edit` is a single atomic change to a buffer. Compound changes are
//! sequences of edits; the dispatcher groups them into one undo step.

use serde::{Deserialize, Serialize};

use crate::position::Range;

/// A single buffer mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub range: Range,
    pub kind: EditKind,
}

/// The mutation operation. Replace covers insert (empty range) and delete
/// (empty replacement); we keep the simple form here and let higher layers
/// build dot-repeat / macro records on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    /// Replace the bytes in `range` with `text`. An insert is `range.is_empty()
    /// && !text.is_empty()`; a delete is `!range.is_empty() && text.is_empty()`.
    Replace { text: String },
}

impl Edit {
    pub fn insert(at: crate::Position, text: impl Into<String>) -> Self {
        Self {
            range: Range::empty(at),
            kind: EditKind::Replace { text: text.into() },
        }
    }

    pub fn delete(range: Range) -> Self {
        Self {
            range,
            kind: EditKind::Replace {
                text: String::new(),
            },
        }
    }

    pub fn replace(range: Range, text: impl Into<String>) -> Self {
        Self {
            range,
            kind: EditKind::Replace { text: text.into() },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::position::Position;

    #[test]
    fn insert_uses_empty_range_at_position() {
        let edit = Edit::insert(Position::new(2, 5), "hi");
        assert!(edit.range.is_empty());
        assert_eq!(edit.range.start, Position::new(2, 5));
        let EditKind::Replace { text } = &edit.kind;
        assert_eq!(text, "hi");
    }

    #[test]
    fn delete_uses_empty_replacement_text() {
        let r = Range::new(Position::new(0, 0), Position::new(0, 3));
        let edit = Edit::delete(r);
        assert_eq!(edit.range, r);
        let EditKind::Replace { text } = &edit.kind;
        assert!(text.is_empty());
    }

    #[test]
    fn replace_carries_both_range_and_text() {
        let r = Range::new(Position::new(1, 0), Position::new(1, 5));
        let edit = Edit::replace(r, "world");
        assert_eq!(edit.range, r);
        let EditKind::Replace { text } = &edit.kind;
        assert_eq!(text, "world");
    }

    #[test]
    fn edit_is_clone_and_eq() {
        let edit = Edit::insert(Position::ZERO, "hi");
        let copy = edit.clone();
        assert_eq!(edit, copy);
    }

    #[test]
    fn edit_is_serializable() {
        let r = Range::new(Position::new(1, 0), Position::new(1, 5));
        let edit = Edit::replace(r, "x");
        let json = serde_json::to_string(&edit).unwrap();
        let back: Edit = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edit);
    }
}
