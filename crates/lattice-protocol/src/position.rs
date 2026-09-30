//! Logical positions and ranges within a buffer.
//!
//! Per §5.6.4: core, plugins, and the dispatcher deal exclusively in *logical*
//! positions (line, byte). The renderer translates to visual positions when
//! drawing. Plugins never see pixels.
//!
//! Byte offsets, not chars or UTF-16 code units: the rope and tree-sitter both
//! index by byte, so the hot path never converts. Protocol peers that count
//! differently (LSP's UTF-16 `character`) convert at their own boundary.

use serde::{Deserialize, Serialize};

/// A logical cursor position: zero-based line, zero-based byte offset within
/// that line. UTF-8 byte offsets, not codepoint indices.
///
/// Ordering is lexicographic — by line, then byte — so `<` means "earlier in
/// the buffer". Nothing here validates a position against a buffer: a byte
/// past the end of the line, or inside a multi-byte character, is
/// representable, and it is the consumer's job to clamp or reject it
/// ([`ProtocolError::PositionOutOfBounds`](crate::ProtocolError::PositionOutOfBounds)).
///
/// # Examples
///
/// ```
/// use lattice_protocol::Position;
///
/// // "héllo": `é` is two UTF-8 bytes, so the `l` after it is at byte 3.
/// let l = Position::new(0, 3);
/// assert!(Position::ZERO < l);
/// assert!(Position::new(0, 99) < Position::new(1, 0)); // line wins
/// ```
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Position {
    /// Zero-based line index.
    pub line: u32,
    /// Zero-based UTF-8 byte offset from the start of `line` (tree-sitter's
    /// `Point.column` convention). A value equal to the line's byte length is
    /// the end-of-line position.
    pub byte: u32,
}

impl Position {
    /// The start of the buffer: line 0, byte 0.
    pub const ZERO: Position = Position { line: 0, byte: 0 };

    /// A position at `line`, `byte` (both zero-based; `byte` in UTF-8 bytes).
    pub const fn new(line: u32, byte: u32) -> Self {
        Self { line, byte }
    }
}

/// A half-open `[start, end)` range expressed as two `Position`s.
///
/// The vim-grammar `Range` (line ranges, marks, patterns, `:%`, `Selection`,
/// custom) lives in `lattice-grammar`; this is the protocol-level structural
/// range used by edits and decorations.
///
/// `start` is included, `end` is not, so `start == end` is a zero-width range
/// (an insertion point). The constructors do not order or validate the
/// endpoints; a well-formed range has `start <= end`, and consumers reject an
/// inverted one ([`ProtocolError::InvalidRange`](crate::ProtocolError::InvalidRange)).
///
/// # Examples
///
/// ```
/// use lattice_protocol::{Position, Range};
///
/// // The first three bytes of line 0 — "abc" in "abcdef".
/// let abc = Range::new(Position::new(0, 0), Position::new(0, 3));
/// assert!(!abc.is_empty());
///
/// // A range may span lines; `end` is exclusive.
/// let two_lines = Range::new(Position::new(1, 4), Position::new(2, 0));
/// assert_eq!(two_lines.end.line, 2);
///
/// // An insertion point.
/// assert!(Range::empty(Position::new(3, 1)).is_empty());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    /// First position inside the range (inclusive).
    pub start: Position,
    /// First position past the range (exclusive).
    pub end: Position,
}

impl Range {
    /// The range `[start, end)`. Endpoints are stored as given.
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// A zero-width range at `at` — where an insert goes.
    pub const fn empty(at: Position) -> Self {
        Self { start: at, end: at }
    }

    /// `true` when `start == end`: the range covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn position_zero_is_origin() {
        assert_eq!(Position::ZERO, Position::new(0, 0));
        assert_eq!(Position::ZERO.line, 0);
        assert_eq!(Position::ZERO.byte, 0);
    }

    #[test]
    fn position_constructor_sets_fields() {
        let p = Position::new(7, 3);
        assert_eq!(p.line, 7);
        assert_eq!(p.byte, 3);
    }

    #[test]
    fn position_orders_lexicographically_by_line_then_byte() {
        assert!(Position::new(0, 5) < Position::new(1, 0));
        assert!(Position::new(2, 1) < Position::new(2, 2));
        assert_eq!(Position::new(3, 4), Position::new(3, 4));
    }

    #[test]
    fn range_new_keeps_endpoints() {
        let a = Position::new(1, 0);
        let b = Position::new(2, 5);
        let r = Range::new(a, b);
        assert_eq!(r.start, a);
        assert_eq!(r.end, b);
    }

    #[test]
    fn range_empty_is_a_zero_width_at_position() {
        let p = Position::new(4, 2);
        let r = Range::empty(p);
        assert_eq!(r.start, p);
        assert_eq!(r.end, p);
        assert!(r.is_empty());
    }

    #[test]
    fn non_empty_range_is_not_empty() {
        let r = Range::new(Position::new(0, 0), Position::new(0, 1));
        assert!(!r.is_empty());
    }

    #[test]
    fn ranges_are_serializable() {
        let r = Range::new(Position::new(1, 2), Position::new(3, 4));
        let json = serde_json::to_string(&r).unwrap();
        let back: Range = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }
}
