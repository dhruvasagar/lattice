//! What a `CommandInvocation` produced once executed.
//!
//! `Effect::None` is for read-only or selection-only commands. `Effect::Edits`
//! carries the `AppliedEdit`s that the dispatcher applied to the document
//! (suitable for `Event::DocumentChanged`). `Effect::SelectionChange` carries
//! the new selection set (suitable for `Event::SelectionsChanged`). Effects
//! compose; a single command can yield multiple via `Effect::Many`.

use lattice_core::buffer::AppliedEdit;
use lattice_protocol::selection::SelectionSet;

use crate::register::Register;

#[derive(Debug, Clone)]
pub enum Effect {
    None,
    Edits(Vec<AppliedEdit>),
    SelectionChange(SelectionSet),
    Yank {
        register: Register,
        content: String,
    },
    Many(Vec<Effect>),
}

impl Effect {
    pub fn is_none(&self) -> bool {
        matches!(self, Effect::None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn none_is_none() {
        assert!(Effect::None.is_none());
    }

    #[test]
    fn yank_carries_register_and_content() {
        let e = Effect::Yank {
            register: Register::Unnamed,
            content: "hello".into(),
        };
        match e {
            Effect::Yank { register, content } => {
                assert_eq!(register, Register::Unnamed);
                assert_eq!(content, "hello");
            }
            _ => panic!("expected Yank"),
        }
    }
}
