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

use lattice_grammar::{ModalState, VisualKind};
use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};
use lattice_runtime::block_on;

use super::{App, EchoLevel, LastVisual};

impl App {
    pub(super) fn do_enter_visual(&mut self, kind: VisualKind) {
        self.editor.modal = ModalState::Visual(kind);
        self.editor.visual_anchor = Some(self.editor.cursor);
        // Seed document.selections so Range::Selection picks up the
        // anchor=head=cursor selection immediately.
        let sel = Selection {
            anchor: self.editor.cursor,
            head: self.editor.cursor,
            visual: Some(visual_kind_to_mode(kind)),
        };
        self.set_selections_blocking(SelectionSet::single(sel));
    }

    pub(super) fn do_exit_visual(&mut self) {
        // Capture the selection extents BEFORE collapsing, so `gv` can
        // restore them. We want the kind from `self.editor.modal` (Visual carries
        // it) and the anchor / head from the document selection.
        if let ModalState::Visual(kind) = self.editor.modal {
            let sels = self.editor.document.selections();
            let sel = sels.primary();
            self.editor.last_visual = Some(LastVisual {
                anchor: sel.anchor,
                head: sel.head,
                kind,
            });
        }
        self.editor.modal = ModalState::Normal;
        self.editor.visual_anchor = None;
        // Collapse selection to a cursor at the current head.
        self.set_selections_blocking(SelectionSet::single(Selection::cursor(self.editor.cursor)));
    }

    pub(super) fn do_reselect_visual(&mut self) {
        let Some(last) = self.editor.last_visual else {
            self.set_message(EchoLevel::Error, "no previous visual selection".to_string());
            return;
        };
        // Restore the selection: cursor lands at `head`, anchor at `anchor`,
        // visual mode is the saved kind.
        self.editor.modal = ModalState::Visual(last.kind);
        self.editor.visual_anchor = Some(last.anchor);
        self.editor.cursor = last.head;
        let sel = Selection {
            anchor: last.anchor,
            head: last.head,
            visual: Some(visual_kind_to_mode(last.kind)),
        };
        self.set_selections_blocking(SelectionSet::single(sel));
    }

    pub fn set_selections_blocking(&self, selections: SelectionSet) {
        // SetSelections only fails on actor-gone; ignore the
        // Result (post-shutdown nothing meaningful to do).
        let _ = block_on(self.editor.document.set_selections(selections));
        self.publish_selections_changed();
    }
}

pub(super) fn visual_kind_to_mode(kind: VisualKind) -> VisualMode {
    match kind {
        VisualKind::Charwise => VisualMode::Charwise,
        VisualKind::Linewise => VisualMode::Linewise,
        VisualKind::Blockwise => VisualMode::Blockwise,
    }
}

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
        let inv = CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
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
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_range(GrammarRange::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "o world");
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn yank_in_visual_populates_register_charwise() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        let inv = CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
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
        let inv = CommandInvocation::of(a.editor.builtins.change.0).with_range(GrammarRange::Selection);
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
        let inv = CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
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
        let inv = CommandInvocation::of(a.editor.builtins.yank.0).with_range(GrammarRange::Selection);
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
