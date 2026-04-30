//! The vim grammar `Range` -- the dispatcher's range arg.
//!
//! Distinct from `lattice_protocol::position::Range` (which is a structural
//! `[start, end)` byte range used by edits and decorations). The grammar
//! `Range` carries vim's ex-syntax range forms: `:1,5`, `:%`, `:'<,'>`,
//! `:.,+10`, `Selection` (active visual region), plugin-supplied custom
//! ranges.

use serde::{Deserialize, Serialize};

use crate::registry::RangeId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Range {
    /// `:1,5`, `:'<,'>`, `:.,+10`, etc.
    Span { start: RangeBound, end: RangeBound },
    /// `:.`
    CurrentLine,
    /// `:%`
    Whole,
    /// The current Visual / active region.
    Selection,
    /// Plugin-registered custom range (e.g., a git-hunk-range plugin).
    Custom(RangeId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeBound {
    /// Absolute line number (1-based at the user surface; 0-based internally).
    Line(u32),
    /// A named mark (`'a`, `'<`, `'>`, etc.).
    Mark(char),
    /// `.`
    CurrentLine,
    /// `$`
    LastLine,
    /// Pattern-relative (`/foo/`, `?bar?`).
    Pattern(String),
    /// Offset from another bound (`+1`, `-3`, `.+5`).
    Offset {
        base: Box<RangeBound>,
        delta: i32,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn whole_range_renders_distinct_variant() {
        assert_ne!(Range::Whole, Range::CurrentLine);
        assert_ne!(Range::Whole, Range::Selection);
    }

    #[test]
    fn span_constructed_from_bounds() {
        let r = Range::Span {
            start: RangeBound::Line(0),
            end: RangeBound::Line(4),
        };
        match r {
            Range::Span { start, end } => {
                assert_eq!(start, RangeBound::Line(0));
                assert_eq!(end, RangeBound::Line(4));
            }
            _ => panic!("expected Span"),
        }
    }

    #[test]
    fn offset_bounds_compose() {
        let off = RangeBound::Offset {
            base: Box::new(RangeBound::CurrentLine),
            delta: 5,
        };
        match off {
            RangeBound::Offset { base, delta } => {
                assert_eq!(*base, RangeBound::CurrentLine);
                assert_eq!(delta, 5);
            }
            _ => panic!("expected Offset"),
        }
    }
}
