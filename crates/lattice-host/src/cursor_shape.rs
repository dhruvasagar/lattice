//! Vim-style cursor shape derived from [`ModalState`].
//!
//! Phase 5.8.N: hoisted out of both renderer peers. Pre-5.8.N the
//! TUI peer had `lattice-ui-tui::runtime::cursor_style_for(modal)
//! -> SetCursorStyle` and the GPUI peer had
//! `lattice-ui-gpui::window::CursorShape::for_mode(modal) -> Self`.
//! Both implemented the same vim convention:
//!
//!   - Normal / Visual / Operator-Pending → Block
//!   - Insert / Command / Search          → Bar
//!   - Replace                            → Underline
//!
//! Now lives once here; renderer peers map [`CursorShape`] →
//! their native cursor primitive (crossterm `SetCursorStyle` for
//! TUI; div-border style for GPUI). Renderer-neutral so any future
//! peer (web, headless) reuses the mapping.

use lattice_grammar::ModalState;

/// Vim-style cursor shape, renderer-neutral. Each renderer peer
/// translates this to its own primitive at paint time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorShape {
    /// Full inverted-cell block — classic vim cursor for command-
    /// language modes (Normal, Visual, Operator-Pending).
    Block,
    /// Thin left-side vertical bar — vim Insert / Command-line
    /// convention. The cursor sits BEFORE the next character.
    Bar,
    /// Thin bottom-side horizontal underline — Replace mode (vim
    /// convention; signals overwrite).
    Underline,
}

impl CursorShape {
    /// Map a [`ModalState`] to its canonical cursor shape.
    pub fn for_mode(modal: ModalState) -> Self {
        match modal {
            ModalState::Insert | ModalState::Command | ModalState::Search(_) => Self::Bar,
            ModalState::Replace => Self::Underline,
            ModalState::Normal | ModalState::Visual(_) | ModalState::OperatorPending => Self::Block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_grammar::{SearchDirection, VisualKind};

    #[test]
    fn normal_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Normal),
            CursorShape::Block
        );
    }

    #[test]
    fn visual_all_kinds_use_block() {
        for k in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            assert_eq!(
                CursorShape::for_mode(ModalState::Visual(k)),
                CursorShape::Block
            );
        }
    }

    #[test]
    fn operator_pending_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::OperatorPending),
            CursorShape::Block
        );
    }

    #[test]
    fn insert_uses_bar() {
        assert_eq!(CursorShape::for_mode(ModalState::Insert), CursorShape::Bar);
    }

    #[test]
    fn command_uses_bar() {
        assert_eq!(CursorShape::for_mode(ModalState::Command), CursorShape::Bar);
    }

    #[test]
    fn search_both_directions_use_bar() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Search(SearchDirection::Forward)),
            CursorShape::Bar
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Search(SearchDirection::Backward)),
            CursorShape::Bar
        );
    }

    #[test]
    fn replace_uses_underline() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Replace),
            CursorShape::Underline
        );
    }
}
