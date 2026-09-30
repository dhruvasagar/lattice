//! Edit primitives.
//!
//! An `Edit` is a single atomic change to a buffer. Compound changes are
//! sequences of edits; the dispatcher groups them into one undo step.
//!
//! [`EditDelta`] is the tree-sitter-shaped sibling of `Edit`: the
//! byte/position deltas a parser needs to know how an edit reshaped
//! a buffer. Producers (the buffer's `apply_edit`) emit it as a
//! by-product of the rope mutation; consumers (the syntax worker)
//! convert it to `tree_sitter::InputEdit` at the parser boundary so
//! `lattice-protocol` stays parser-agnostic. The type sits here so
//! all edit-shaped types live together.

use serde::{Deserialize, Serialize};

use crate::position::{Position, Range};

/// A single buffer mutation: replace the bytes in [`Self::range`] with the
/// text carried by [`Self::kind`].
///
/// Coordinates are pre-edit [`Position`]s (line + UTF-8 byte). A compound
/// change is a sequence of `Edit`s applied in order — each against the buffer
/// as the previous one left it — and grouped into one undo step by the
/// dispatcher.
///
/// # Examples
///
/// ```
/// use lattice_protocol::{Edit, EditKind, Position, Range};
///
/// // Insert "fn " at the start of line 3: an empty range plus text.
/// let insert = Edit::insert(Position::new(3, 0), "fn ");
/// assert!(insert.range.is_empty());
///
/// // Delete bytes 4..7 of line 0: a range plus empty text.
/// let delete = Edit::delete(Range::new(Position::new(0, 4), Position::new(0, 7)));
/// let EditKind::Replace { text } = &delete.kind;
/// assert!(text.is_empty());
///
/// // Replace is the general form both of the above reduce to.
/// let word = Range::new(Position::new(1, 0), Position::new(1, 5));
/// assert_eq!(Edit::replace(word, "world").range, word);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// The half-open span to replace, in pre-edit coordinates. Empty for an
    /// insert.
    pub range: Range,
    /// What to put there.
    pub kind: EditKind,
}

/// The mutation operation. Replace covers insert (empty range) and delete
/// (empty replacement); we keep the simple form here and let higher layers
/// build dot-repeat / macro records on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    /// Replace the bytes in `range` with `text`. An insert is `range.is_empty()
    /// && !text.is_empty()`; a delete is `!range.is_empty() && text.is_empty()`.
    Replace {
        /// The replacement text; empty for a delete.
        text: String,
    },
}

impl Edit {
    /// Insert `text` at `at` (an empty-range replace).
    pub fn insert(at: crate::Position, text: impl Into<String>) -> Self {
        Self {
            range: Range::empty(at),
            kind: EditKind::Replace { text: text.into() },
        }
    }

    /// Delete the bytes in `range` (a replace with empty text).
    pub fn delete(range: Range) -> Self {
        Self {
            range,
            kind: EditKind::Replace {
                text: String::new(),
            },
        }
    }

    /// Replace the bytes in `range` with `text`.
    pub fn replace(range: Range, text: impl Into<String>) -> Self {
        Self {
            range,
            kind: EditKind::Replace { text: text.into() },
        }
    }
}

/// Tree-sitter-shaped description of a single applied edit.
///
/// Carries the six fields tree-sitter's `InputEdit` needs (three
/// byte offsets + three line/byte positions) so a syntax worker
/// can drive incremental reparse without re-querying the buffer.
/// Produced as a by-product of `Buffer::apply_edit` (zero new
/// rope reads -- every field is already computed there); rides
/// on `lattice_core::AppliedEdit`. The actual `tree_sitter::InputEdit`
/// conversion lives in `lattice-syntax` so this crate stays
/// parser-agnostic.
///
/// Field semantics match tree-sitter:
/// - `start_byte`: byte offset of the edit's start in the
///   pre-edit buffer.
/// - `old_end_byte`: byte offset of the end of the deleted span
///   in the pre-edit buffer (== `start_byte` for pure inserts).
/// - `new_end_byte`: byte offset of the end of the inserted span
///   in the post-edit buffer (== `start_byte` for pure deletes).
/// - `start_position` / `old_end_position` / `new_end_position`:
///   the same three points in line-and-byte-within-line form.
///   `Position.byte` is byte-within-line, which already matches
///   tree-sitter's `Point.column` semantics (column-as-bytes).
///
/// `Copy` so callers pass it through chains by register move,
/// not by Arc bump. 36 bytes -- fits one cache line.
///
/// The producer (`lattice_core::Buffer::apply_edit`) saturates each byte
/// offset at `u32::MAX` rather than panicking on a buffer over 4 GiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditDelta {
    /// Absolute byte offset (from the start of the buffer) where the edit
    /// begins; the same in the pre- and post-edit buffer.
    pub start_byte: u32,
    /// Absolute byte offset of the end of the removed span, in the pre-edit
    /// buffer. Equals `start_byte` for a pure insert.
    pub old_end_byte: u32,
    /// Absolute byte offset of the end of the inserted span, in the post-edit
    /// buffer. Equals `start_byte` for a pure delete.
    pub new_end_byte: u32,
    /// `start_byte` as (line, byte-within-line).
    pub start_position: Position,
    /// `old_end_byte` as (line, byte-within-line), pre-edit.
    pub old_end_position: Position,
    /// `new_end_byte` as (line, byte-within-line), post-edit.
    pub new_end_position: Position,
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
