//! Vim registers (DESIGN.md §5.2.2).
//!
//! This crate owns only the register *name* -- the `"<X>` a user types
//! before an operator, carried on a [`crate::CommandInvocation`] and in
//! [`crate::AppEffect::SelectRegister`]. Storage (contents, the yank ring
//! the numbered registers project, clipboard mirroring) lives in the host's
//! `Editor`, which interprets each variant as described below.

use serde::{Deserialize, Serialize};

/// A vim register name.
///
/// Parse user input with [`Register::from_input_char`]; the variants that
/// function never returns ([`Register::Expression`],
/// [`Register::ReadOnly`]) are modelled for completeness and the WIT mirror
/// but nothing in the host produces or reads them yet.
///
/// # Examples
///
/// ```
/// use lattice_grammar::Register;
///
/// assert_eq!(Register::from_input_char('a'), Some(Register::Named('a')));
/// assert_eq!(Register::from_input_char('3'), Some(Register::Numbered(3)));
/// assert_eq!(Register::from_input_char('+'), Some(Register::System));
/// assert_eq!(Register::from_input_char('_'), Some(Register::BlackHole));
/// // `"` re-selects the default.
/// assert_eq!(Register::from_input_char('"'), Some(Register::default()));
/// // `=` (expression) is not bindable through `"<X>`.
/// assert_eq!(Register::from_input_char('='), None);
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Register {
    /// `""` -- the default target of every yank / delete / put when no
    /// register was named. With the `clipboard` option on, reads prefer the
    /// system clipboard.
    #[default]
    Unnamed,
    /// `"a`-`"z` / `"A`-`"Z`. The char is stored exactly as typed:
    /// uppercase is a *separate* register, not vim's append-to-lowercase
    /// form (not implemented).
    Named(char),
    /// `"+` / `"*` -- the system clipboard. Both chars map here; writes
    /// always mirror to the clipboard, reads fall back to the unnamed
    /// register when the clipboard is unavailable.
    System,
    /// `"_` -- the sink. Writes are discarded (and do not clobber the
    /// unnamed register); reads yield nothing.
    BlackHole,
    /// `"=` -- vim's expression register. Modelled, not implemented: no
    /// input maps to it and the host stores nothing for it.
    Expression,
    /// Vim's read-only registers (`".`, `"%`, `":`, `"/`), keyed by their
    /// char. Modelled, not implemented: no input maps to it.
    ReadOnly(char),
    /// `"0`-`"9`. A read is a projection of the host's yank ring (`"0` the
    /// newest yank, `"1`-`"9` the newest deletes), falling back to an
    /// explicit write into that slot. Values above 9 are not produced by
    /// [`Register::from_input_char`].
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
