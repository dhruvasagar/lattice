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
            ModalState::Insert
            | ModalState::Command
            | ModalState::Search(_)
            | ModalState::Prompt => Self::Bar,
            ModalState::Replace => Self::Underline,
            // SN.3d: Select shares Visual's Block cursor — the
            // selection conveys the mode; the cursor matches Visual.
            ModalState::Normal
            | ModalState::Visual(_)
            | ModalState::Select(_)
            | ModalState::OperatorPending => Self::Block,
        }
    }
}

/// Does a **minibuffer** own the caret in this modal state?
///
/// True for the three buffer-backed readline surfaces — the `:` command
/// line, the `/`·`?` search line and `Effect::OpenPrompt`'s prompt. Each
/// draws its own caret at its own buffer's cursor, so every OTHER surface
/// must leave the caret alone while one is open.
///
/// **Renderer-neutral because both peers got this wrong the same way.**
/// The TUI drives one hardware caret, so two surfaces placing it means the
/// last writer wins; GPUI paints carets as elements, so two surfaces mean
/// two carets. Different symptoms, one question — and the pane path
/// already asked it locally (`prompt_owns_cursor`) while the popup path
/// did not ask it at all. Reported in use: with a popup focused, `/` left
/// the caret sitting in the popup and merely changed its shape to a Bar,
/// so a read-only popup looked like it had entered Insert.
///
/// Note it includes `Prompt`, which the TUI's local copy omitted — the
/// prompt line draws its own caret exactly as the other two do.
pub fn minibuffer_owns_caret(modal: ModalState) -> bool {
    matches!(
        modal,
        ModalState::Command | ModalState::Search(_) | ModalState::Prompt
    )
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

    /// SN.3d parity: Select mode uses the same Block cursor as Visual
    /// for every kind — Select must never diverge from Visual here.
    #[test]
    fn select_all_kinds_use_block_matching_visual() {
        for k in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            assert_eq!(
                CursorShape::for_mode(ModalState::Select(k)),
                CursorShape::for_mode(ModalState::Visual(k)),
                "Select({k:?}) cursor must match Visual({k:?})"
            );
            assert_eq!(
                CursorShape::for_mode(ModalState::Select(k)),
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

#[cfg(test)]
mod caret_owner_tests {
    use super::*;
    use lattice_grammar::{SearchDirection, VisualKind};

    /// The three readline surfaces own the caret; nothing else does.
    #[test]
    fn the_minibuffers_own_the_caret() {
        for modal in [
            ModalState::Command,
            ModalState::Search(SearchDirection::Forward),
            ModalState::Search(SearchDirection::Backward),
            ModalState::Prompt,
        ] {
            assert!(
                minibuffer_owns_caret(modal),
                "{modal:?} draws its own caret at its own buffer's cursor"
            );
        }
    }

    /// The editing states do not — the pane (or a focused popup) places it.
    #[test]
    fn the_editing_states_do_not() {
        for modal in [
            ModalState::Normal,
            ModalState::Insert,
            ModalState::Replace,
            ModalState::Visual(VisualKind::Charwise),
            ModalState::Select(VisualKind::Linewise),
            ModalState::OperatorPending,
        ] {
            assert!(
                !minibuffer_owns_caret(modal),
                "{modal:?} is an editing state — the surface owns its caret"
            );
        }
    }

    /// Insert and Search both draw a Bar, and that is exactly why the shape
    /// cannot stand in for the question: a popup whose caret shape came from
    /// `Search` looked like it had entered Insert.
    #[test]
    fn shape_alone_cannot_answer_this() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Insert),
            CursorShape::for_mode(ModalState::Search(SearchDirection::Forward)),
            "same shape, different owner — hence a separate predicate"
        );
    }
}
