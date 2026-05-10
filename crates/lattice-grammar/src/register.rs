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

impl Register {
    /// Map a user-typed register-prefix char (the `<X>` in `"<X>`)
    /// to a [`Register`] variant. Returns `None` for chars that
    /// don't name any register (the App treats `None` as "drop
    /// pending state" -- see `docs/dev/notes/8i-approach.md` slice 8.i.3).
    ///
    /// Mirrors vim's `:help registers`: letters name a register,
    /// digits name the numbered ring, `"` re-selects the unnamed
    /// register, `_` is the black-hole sink, `+` / `*` are the
    /// system clipboard (X11 / macOS conventions overlap here).
    /// Expression / readonly registers aren't user-bindable via
    /// `"<X>` and intentionally return `None`.
    pub fn from_input_char(c: char) -> Option<Self> {
        match c {
            'a'..='z' | 'A'..='Z' => Some(Register::Named(c)),
            '0'..='9' => Some(Register::Numbered((c as u8) - b'0')),
            '"' => Some(Register::Unnamed),
            '_' => Some(Register::BlackHole),
            '+' | '*' => Some(Register::System),
            _ => None,
        }
    }
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
