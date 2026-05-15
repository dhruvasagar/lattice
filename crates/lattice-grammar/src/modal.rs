//! Modal state -- a buffer-level state machine in front of the buffer
//! (DESIGN.md §5.2). Orthogonal to major / minor modes.
//!
//! Phase 1 only models the state *type*. The transitions (Normal → Insert on
//! `i`, Operator-Pending entered after an operator, etc.) are driven by the
//! keystroke parser which is itself a later Phase 1 deliverable.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModalState {
    #[default]
    Normal,
    Insert,
    Visual(VisualKind),
    OperatorPending,
    Command,
    Search(SearchDirection),
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VisualKind {
    Charwise,
    Linewise,
    Blockwise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchDirection {
    Forward,
    Backward,
}

impl ModalState {
    /// Whether this state is currently consuming a Visual-mode selection (in
    /// any of charwise / linewise / blockwise). Used by callers that want to
    /// supply `Range::Selection` as a default when no explicit range is given.
    pub fn is_visual(self) -> bool {
        matches!(self, ModalState::Visual(_))
    }

    /// Whether this state expects more input to complete a pending operator.
    /// In Op-Pending, a motion or text object is awaited; pressing an operator
    /// key here resolves to "operate on the current line" (vim's `dd`, `cc`,
    /// `yy` semantics).
    pub fn is_operator_pending(self) -> bool {
        matches!(self, ModalState::OperatorPending)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn visual_predicate_recognises_each_kind() {
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            assert!(ModalState::Visual(kind).is_visual());
        }
    }

    #[test]
    fn non_visual_states_are_not_visual() {
        for s in [
            ModalState::Normal,
            ModalState::Insert,
            ModalState::OperatorPending,
            ModalState::Command,
            ModalState::Search(SearchDirection::Forward),
            ModalState::Replace,
        ] {
            assert!(!s.is_visual(), "{s:?} should not be visual");
        }
    }

    #[test]
    fn operator_pending_predicate() {
        assert!(ModalState::OperatorPending.is_operator_pending());
        assert!(!ModalState::Normal.is_operator_pending());
    }

    #[test]
    fn states_are_serializable() {
        let s = ModalState::Visual(VisualKind::Linewise);
        let json = serde_json::to_string(&s).unwrap_or_else(|_| panic!("serialize"));
        let back: ModalState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
