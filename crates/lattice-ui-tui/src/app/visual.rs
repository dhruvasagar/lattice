//! Visual mode (`v`, `V`, `<C-v>`) -- selection state,
//! exit-collapse, and `gv` reselect.
//!
//! Methods that live here:
//! - `do_enter_visual` (`v` / `V` / `<C-v>` from Normal).
//! - `do_exit_visual` (`<Esc>` from Visual; also called from
//!   the post-operator-on-selection path in app.rs and
//!   from `do_create_fold_from_visual`).
//! - `do_reselect_visual` (`gv`).
//! - `visual_kind_to_mode` -- pure
//!   `VisualKind` -> `VisualMode` conversion shared with
//!   `apply_effect`'s SelectionChange handling.
//!
//! What does NOT live here yet (deferred to a later slice):
//! - `do_enter_block_visual_insert` (block-mode `I` / `A`):
//!   entangled with operator/edit semantics on block
//!   selections; moves with the block-visual + operators
//!   group later.

use super::App;

impl App {
    // 5.5.G.2: `do_enter_visual` + `do_reselect_visual` migrated to
    // [`lattice_host::dispatch::Editor`] (the `Action::EnterVisual` /
    // `Action::ReselectLastVisual` arms in `Editor::dispatch` call
    // them directly; no remaining App callers).
    //
    // `do_exit_visual` stays as a 1-line delegate because App-side
    // helpers (`run_oil_invocation`, the post-operator-on-selection
    // path in `App::run_document_invocation`, `do_create_fold_from_visual`,
    // an LSP rename path) still call it. The delegate retires when
    // those helpers migrate host-side.
    pub(super) fn do_exit_visual(&mut self) {
        self.mutate_editor_with(move |e| e.do_exit_visual());
    }
}

// 5.5.E.4: `set_selections_blocking` moved to
// [`lattice_host::dispatch::Editor::set_selections_blocking`];
// `visual_kind_to_mode` moved to
// [`lattice_host::dispatch::visual_kind_to_mode`]. Both sit alongside
// the [`Effect::SelectionChange`] arm so the renderer-neutral host
// owns the selection-set actor handshake plus the grammar->protocol
// visual-flavour translator.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::test_helpers::{app_with, invoke_motion};
    use crate::app::*;
    use lattice_grammar::{
        CommandInvocation, ModalState, Range as GrammarRange, VisualKind, YankKind, command::Count,
    };
    use lattice_protocol::position::Position;
    use lattice_protocol::selection::VisualMode;

    // ---- gv reselect ----

    #[test]
    fn exit_visual_captures_last_visual() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        // Now selection is anchor=ZERO, head=(0,6).
        a.apply(Action::ExitVisual);
        let last = a.editor.last_visual.expect("last_visual captured");
        assert_eq!(last.anchor, Position::ZERO);
        assert_eq!(last.head, Position::new(0, 6));
        assert_eq!(last.kind, VisualKind::Charwise);
    }

    #[test]
    fn gv_with_no_prior_visual_emits_error() {
        let mut a = app_with("hello", 10);
        assert!(a.editor.last_visual.is_none());
        a.apply(Action::ReselectLastVisual);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn gv_restores_anchor_head_and_kind() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        a.apply(Action::ExitVisual);
        // Cursor now collapsed; modal Normal.
        assert_eq!(a.editor.modal, ModalState::Normal);
        // gv:
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.editor.modal, ModalState::Visual(VisualKind::Charwise));
        let sels = a.editor.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::ZERO);
        assert_eq!(sel.head, Position::new(0, 6));
        assert_eq!(a.editor.cursor, Position::new(0, 6));
    }

    #[test]
    fn gv_after_yank_in_visual_restores_pre_yank_selection() {
        // Real-world test: select, yank (which auto-exits Visual), `gv`
        // should bring back the same selection so you can re-operate.
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.modal, ModalState::Normal);
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.editor.modal, ModalState::Visual(VisualKind::Charwise));
        let sels = a.editor.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.head, Position::new(0, 6));
    }

    #[test]
    fn gv_preserves_linewise_kind() {
        let mut a = app_with("aaa\nbbb\nccc", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.editor.builtins.line_down));
        a.apply(Action::ExitVisual);
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.editor.modal, ModalState::Visual(VisualKind::Linewise));
    }

    // ---- `o` swap visual ends ----

    #[test]
    fn swap_visual_ends_trades_anchor_and_head() {
        let mut a = app_with("hello world", 20);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        // anchor ZERO, head (0,6).
        let sel = *a.editor.document.selections().primary();
        assert_eq!(sel.anchor, Position::ZERO);
        assert_eq!(sel.head, Position::new(0, 6));
        // `o` swaps the ends; the cursor follows the head.
        a.apply(Action::SwapVisualEnds);
        let swapped = *a.editor.document.selections().primary();
        assert_eq!(swapped.anchor, Position::new(0, 6));
        assert_eq!(swapped.head, Position::ZERO);
        assert_eq!(a.editor.cursor, Position::ZERO);
        assert_eq!(a.editor.modal, ModalState::Visual(VisualKind::Charwise));
        // `o` again restores the original orientation.
        a.apply(Action::SwapVisualEnds);
        let back = *a.editor.document.selections().primary();
        assert_eq!(back.anchor, Position::ZERO);
        assert_eq!(back.head, Position::new(0, 6));
        assert_eq!(a.editor.cursor, Position::new(0, 6));
    }

    #[test]
    fn after_swap_a_motion_alters_the_other_end() {
        // The whole point of `o`: after swapping, a motion grows /
        // shrinks the selection at the end the cursor moved to.
        let mut a = app_with("hello world foo", 20);
        // Park the cursor at the start of "world" in Normal, then
        // select forward to the start of "foo".
        a.apply(invoke_motion(a.editor.builtins.word_forward)); // cursor (0,6)
        a.apply(Action::EnterVisual(VisualKind::Charwise)); // anchor (0,6)
        a.apply(invoke_motion(a.editor.builtins.word_forward)); // head (0,12)
        a.apply(Action::SwapVisualEnds); // head (0,6), anchor (0,12)
        assert_eq!(a.editor.cursor, Position::new(0, 6));
        // A motion now moves the swapped head (the START end) leftward.
        a.apply(invoke_motion(a.editor.builtins.word_backward)); // head (0,0)
        let sel = *a.editor.document.selections().primary();
        assert_eq!(sel.anchor, Position::new(0, 12), "far end stays put");
        assert_eq!(sel.head, Position::ZERO, "near end moved");
    }

    #[test]
    fn swap_visual_ends_outside_visual_is_a_noop() {
        let mut a = app_with("hello", 10);
        assert_eq!(a.editor.modal, ModalState::Normal);
        a.apply(Action::SwapVisualEnds);
        assert_eq!(a.editor.modal, ModalState::Normal);
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    // ---- Visual mode end-to-end ----

    #[test]
    fn enter_visual_charwise_sets_modal_and_anchor() {
        let mut a = app_with("hello", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        assert_eq!(a.editor.modal, ModalState::Visual(VisualKind::Charwise));
        assert_eq!(a.editor.visual_anchor, Some(Position::new(0, 1)));
        let sels = a.editor.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::new(0, 1));
        assert_eq!(sel.head, Position::new(0, 1));
        assert_eq!(sel.visual, Some(VisualMode::Charwise));
    }

    #[test]
    fn motion_in_visual_extends_head_keeps_anchor() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        let sels = a.editor.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::ZERO);
        assert_eq!(sel.head, Position::new(0, 6));
        assert_eq!(a.editor.cursor, Position::new(0, 6));
    }

    #[test]
    fn esc_in_visual_collapses_selection_and_returns_to_normal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.char_right));
        a.apply(Action::ExitVisual);
        assert_eq!(a.editor.modal, ModalState::Normal);
        assert!(a.editor.visual_anchor.is_none());
        assert!(a.editor.document.selections().primary().is_cursor());
    }

    #[test]
    fn delete_in_visual_removes_selection_and_returns_to_normal() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.char_right));
        a.apply(invoke_motion(a.editor.builtins.char_right));
        a.apply(invoke_motion(a.editor.builtins.char_right));
        // Selection now covers bytes 0..3 of "hello world" charwise (vim
        // INCLUSIVE -> visual range covers 0..=3 = 4 bytes "hell").
        let inv =
            CommandInvocation::of(a.editor.builtins.delete.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "o world");
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn yank_in_visual_populates_register_charwise() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Charwise);
        // Document text untouched.
        assert_eq!(a.editor.document.text(), "hello world");
        // Visual mode exited.
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn change_in_visual_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        let inv =
            CommandInvocation::of(a.editor.builtins.change.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        // Change in Visual deletes selection AND drops into Insert.
        assert_eq!(a.editor.modal, ModalState::Insert);
    }

    #[test]
    fn linewise_visual_yank_captures_full_lines() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.cursor = Position::new(1, 1); // mid-line on "BBB"
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        // Selection is single line; yank captures the whole line
        // regardless of byte offsets. Slice 8.i.4.g: linewise yank
        // content always ends with `\n`.
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB\n");
    }

    #[test]
    fn linewise_visual_extends_to_multiple_lines() {
        let mut a = app_with("aaa\nbbb\nccc\nddd", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.editor.builtins.line_down));
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        // Lines 0 and 1 -> "aaa\nbbb\n" (slice 8.i.4.g: trailing
        // `\n` always present for linewise content).
        assert_eq!(reg.content, "aaa\nbbb\n");
    }

    #[test]
    fn visual_anchor_persists_across_count_motion() {
        // Slice 8.i.4.f: count multiplication is input-side
        // (`attach_count`); the dispatcher reads the baked
        // `inv.count`. To exercise `2w` here we invoke
        // word_forward with `Count(2)` directly -- the
        // press_* harness covers the keystroke -> count flow.
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(Action::Invoke(
            CommandInvocation::of(a.editor.builtins.word_forward.0).with_count(Count(2)),
        ));
        let sels = a.editor.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::ZERO);
        // 2w from origin advances 2 word starts: "ONE two THREE" -> byte 8.
        assert_eq!(sel.head, Position::new(0, 8));
    }

    #[test]
    fn select_register_clears_partial_chord() {
        let mut a = app_with("hello", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char(
            '"',
        )));
        a.apply(Action::SelectRegister(Register::Named('a')));
        assert!(a.editor.partial_chord.is_empty());
    }

    #[test]
    fn select_register_stashes_pending_register() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SelectRegister(Register::Named('a')));
        assert_eq!(a.editor.pending_register, Some(Register::Named('a')));
    }
}
