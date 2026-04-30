//! Logical positions and ranges within a buffer.
//!
//! Per §5.6.4: core, plugins, and the dispatcher deal exclusively in *logical*
//! positions (line, byte). The renderer translates to visual positions when
//! drawing. Plugins never see pixels.

use serde::{Deserialize, Serialize};

/// A logical cursor position: zero-based line, zero-based byte offset within
/// that line. UTF-8 byte offsets, not codepoint indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub byte: u32,
}

impl Position {
    pub const ZERO: Position = Position { line: 0, byte: 0 };

    pub const fn new(line: u32, byte: u32) -> Self {
        Self { line, byte }
    }
}

/// A half-open `[start, end)` range expressed as two `Position`s.
///
/// The vim-grammar `Range` (line ranges, marks, patterns, `:%`, `Selection`,
/// custom) lives in `lattice-grammar`; this is the protocol-level structural
/// range used by edits and decorations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    pub const fn empty(at: Position) -> Self {
        Self { start: at, end: at }
    }

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
