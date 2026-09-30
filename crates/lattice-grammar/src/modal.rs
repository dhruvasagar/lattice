//! Modal state -- a buffer-level state machine in front of the buffer
//! (DESIGN.md §5.2). Orthogonal to major / minor modes.
//!
//! This crate owns the state *type* only. Transitions are not methods here:
//! a command asks for one by returning [`Effect::EnterMode`](crate::Effect::EnterMode)
//! (or [`AppEffect::EnterMode`](crate::AppEffect::EnterMode)), and the host
//! applies it to the focused buffer's modal field. That keeps the state
//! machine's *policy* (which chord enters which state) in the keymap and the
//! command bodies, and its *storage* in the host, while every layer agrees on
//! one vocabulary.
//!
//! The usual vim transitions, for orientation:
//!
//! | From | Key | To |
//! |---|---|---|
//! | Normal | `i` `a` `o` … | [`ModalState::Insert`] |
//! | Normal | `v` / `V` / `<C-v>` | [`ModalState::Visual`] (charwise / linewise / blockwise) |
//! | Normal | `gh` / `gH` / `g<C-h>` | [`ModalState::Select`] |
//! | Normal | an operator (`d`, `c`, `y` …) | [`ModalState::OperatorPending`] |
//! | Normal | `:` | [`ModalState::Command`] |
//! | Normal | `/` / `?` | [`ModalState::Search`] |
//! | Normal | `R` | [`ModalState::Replace`] |
//! | any | `<Esc>` | [`ModalState::Normal`] |
//!
//! # Examples
//!
//! ```
//! use lattice_grammar::{ModalState, VisualKind};
//!
//! let state = ModalState::default();
//! assert_eq!(state, ModalState::Normal);
//!
//! // `V` in Normal: the host applies `EnterMode(Visual(Linewise))`.
//! let state = ModalState::Visual(VisualKind::Linewise);
//! assert!(state.is_visual());
//! assert!(!state.is_select()); // same geometry, different dispatch
//! assert!(!state.is_operator_pending());
//! ```

use serde::{Deserialize, Serialize};

/// The vim modal state of a buffer — which grammar a keystroke is read in.
///
/// Buffer-level and orthogonal to major / minor modes (a `rust` buffer is
/// in exactly one of these at a time; the axes never collapse). Keymap
/// lookups are filtered by it, and [`Range::Selection`](crate::Range::Selection)
/// defaults from it while Visual is active. Serializable so it can cross
/// the core protocol and be recorded in snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModalState {
    /// Vim Normal mode: keys are operators, motions and commands. The
    /// initial state of every buffer and the target of `<Esc>`.
    #[default]
    Normal,
    /// Vim Insert mode: printable keys insert text at the cursor.
    Insert,
    /// Vim Visual mode with the given selection shape; the selection is the
    /// active region and the default range for operators and ex-commands.
    Visual(VisualKind),
    /// Vim Select mode (SN.3d). Same selection *geometry* as
    /// [`Self::Visual`] (the `VisualKind` is reused verbatim), but
    /// inverted typing semantics: a printable key replaces the whole
    /// selection and drops into Insert. See
    /// `docs/dev/architecture/select-mode.md`.
    Select(VisualKind),
    /// An operator has been typed and a motion or text object is awaited
    /// (`d` in `dw`). Repeating the operator key operates linewise on the
    /// current line (`dd`, `cc`, `yy`).
    OperatorPending,
    /// The `:` command line (the `*command-line*` minibuffer) is focused.
    Command,
    /// The `/` or `?` search line (the `*search-line*` minibuffer) is
    /// focused, searching in the given direction.
    Search(SearchDirection),
    /// Vim Replace mode (`R`): printable keys overtype existing characters
    /// instead of inserting.
    Replace,
    /// A generic one-line minibuffer text prompt is focused (see
    /// `Effect::OpenPrompt`) — distinct from `Command`/`Search`
    /// because those are tied to the specific `*command-line*` /
    /// `*search-line*` buffers and their own submit semantics; a
    /// prompt's buffer/label/submit-action varies per invocation.
    Prompt,
}

/// The shape of a Visual / Select selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VisualKind {
    /// Character-wise (`v`): from anchor to cursor, inclusive.
    Charwise,
    /// Line-wise (`V`): whole lines from the anchor's line to the cursor's.
    Linewise,
    /// Block-wise (`<C-v>`): the rectangle spanned by anchor and cursor.
    Blockwise,
}

/// Which way a search runs: `/` searches forward, `?` backward. `n`
/// repeats in the same direction, `N` in the opposite one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchDirection {
    /// Towards the end of the buffer (`/`).
    Forward,
    /// Towards the start of the buffer (`?`).
    Backward,
}

impl ModalState {
    /// Whether this state is currently consuming a Visual-mode selection (in
    /// any of charwise / linewise / blockwise). Used by callers that want to
    /// supply `Range::Selection` as a default when no explicit range is given.
    pub fn is_visual(self) -> bool {
        matches!(self, ModalState::Visual(_))
    }

    /// Whether this state is Select mode (SN.3d), in any of charwise /
    /// linewise / blockwise. Select shares Visual's selection geometry
    /// but overtypes on a printable key. Kept distinct from
    /// [`Self::is_visual`] because the *dispatch* differs; callers that
    /// care only about "is a selection live" should gain an explicit
    /// `selection_is_active` helper when one is first needed (no
    /// production caller exists yet — see select-mode.md §2).
    pub fn is_select(self) -> bool {
        matches!(self, ModalState::Select(_))
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
            ModalState::Select(VisualKind::Charwise),
            ModalState::OperatorPending,
            ModalState::Command,
            ModalState::Search(SearchDirection::Forward),
            ModalState::Replace,
        ] {
            assert!(!s.is_visual(), "{s:?} should not be visual");
        }
    }

    #[test]
    fn select_predicate_recognises_each_kind_and_excludes_others() {
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            let s = ModalState::Select(kind);
            assert!(s.is_select());
            // Select is NOT Visual — the dispatch differs even though
            // the geometry is shared.
            assert!(!s.is_visual());
        }
        for s in [
            ModalState::Normal,
            ModalState::Visual(VisualKind::Charwise),
            ModalState::Insert,
        ] {
            assert!(!s.is_select(), "{s:?} should not be select");
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
