//! Vim registers (DESIGN.md §5.2.2).
//!
//! Phase 1 only models the type. Backing storage and the numbered ring
//! ("kill-ring") land later in Phase 1 alongside macros and dot-repeat.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Register {
    #[default]
    Unnamed,
    Named(char),
    System,
    BlackHole,
    Expression,
    ReadOnly(char),
    Numbered(u8),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn default_is_unnamed() {
        assert_eq!(Register::default(), Register::Unnamed);
    }

    #[test]
    fn named_registers_are_distinct_by_letter() {
        assert_ne!(Register::Named('a'), Register::Named('b'));
        assert_eq!(Register::Named('a'), Register::Named('a'));
    }

    #[test]
    fn numbered_registers_distinct_by_index() {
        assert_ne!(Register::Numbered(0), Register::Numbered(1));
    }
}
