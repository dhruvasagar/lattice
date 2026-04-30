//! Translate `crossterm` key events into `Action`s.
//!
//! This is a small, pure function that reads modal state, the pending-key
//! buffer, and the catalog of built-in command IDs to decide what each key
//! press means. It is the v1 stand-in for the layered keymap engine
//! described in DESIGN.md §5.2.3 -- the *shape* matches (chord -> typed
//! invocation) so swapping in a real keymap layer later is mechanical.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use lattice_grammar::ModalState;
use lattice_grammar::Target;
use lattice_grammar::args::Args;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::registry::{MotionId, OperatorId};

use crate::app::{Action, Pending};

pub struct TranslateContext<'a> {
    pub modal: ModalState,
    pub pending: Pending,
    pub builtins: &'a Builtins,
}

pub fn translate(ctx: TranslateContext<'_>, event: KeyEvent) -> Action {
    // Universal escape hatch.
    if event.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(event.code, KeyCode::Char('c'))
    {
        return Action::Quit;
    }

    match ctx.modal {
        ModalState::Insert => translate_insert(event),
        ModalState::Normal => translate_normal(event, ctx.pending, ctx.builtins),
        ModalState::Command => translate_command(event),
        // The other modal states route to no-op until their respective
        // phases land.
        _ => Action::None,
    }
}

fn translate_command(event: KeyEvent) -> Action {
    match event.code {
        KeyCode::Esc => Action::CommandLineCancel,
        KeyCode::Enter => Action::CommandLineSubmit,
        KeyCode::Backspace => Action::CommandLineBackspace,
        KeyCode::Char(c) if !event.modifiers.contains(KeyModifiers::CONTROL) => {
            Action::CommandLineAppend(c)
        }
        _ => Action::None,
    }
}

fn translate_insert(event: KeyEvent) -> Action {
    match event.code {
        KeyCode::Esc => Action::EnterMode(ModalState::Normal),
        KeyCode::Backspace => Action::DeleteCharBackward,
        KeyCode::Enter => Action::Insert("\n".into()),
        KeyCode::Tab => Action::Insert("\t".into()),
        KeyCode::Char(c) if !event.modifiers.contains(KeyModifiers::CONTROL) => {
            Action::Insert(c.to_string())
        }
        _ => Action::None,
    }
}

fn translate_normal(event: KeyEvent, pending: Pending, builtins: &Builtins) -> Action {
    // Resolve any pending state first.
    match pending {
        Pending::AfterG => return resolve_after_g(event, builtins),
        Pending::AfterOperator(op) => return resolve_after_operator(event, builtins, op),
        Pending::None => {}
    }

    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('d') => invoke_with_count(builtins.line_down, 10),
            KeyCode::Char('u') => invoke_with_count(builtins.line_up, 10),
            KeyCode::Char('r') => Action::Redo,
            _ => Action::None,
        };
    }

    match event.code {
        KeyCode::Char('q') => Action::Quit,

        // Motions
        KeyCode::Char('h') | KeyCode::Left => invoke(builtins.char_left),
        KeyCode::Char('j') | KeyCode::Down => invoke(builtins.line_down),
        KeyCode::Char('k') | KeyCode::Up => invoke(builtins.line_up),
        KeyCode::Char('l') | KeyCode::Right => invoke(builtins.char_right),
        KeyCode::Char('0') | KeyCode::Home => invoke(builtins.line_start),
        KeyCode::Char('$') | KeyCode::End => invoke(builtins.line_end),
        KeyCode::Char('w') => invoke(builtins.word_forward),
        KeyCode::Char('G') => invoke(builtins.goto_last_line),

        // Pending key sequences
        KeyCode::Char('g') => Action::SetPending(Pending::AfterG),

        // Operator-leading keys
        KeyCode::Char('d') => Action::SetPending(Pending::AfterOperator(builtins.delete)),

        // Vim's `x` -- delete one char to the right.
        KeyCode::Char('x') => Action::Invoke(
            CommandInvocation::of(builtins.delete.0)
                .with_target(Target::Motion(builtins.char_right, Args::None)),
        ),

        // Mode entry
        KeyCode::Char('i') => Action::EnterMode(ModalState::Insert),
        KeyCode::Char('a') => Action::EnterAppend,
        KeyCode::Char('o') => Action::OpenLineBelow,
        KeyCode::Char('O') => Action::OpenLineAbove,
        KeyCode::Char(':') => Action::EnterCommandLine,

        // Undo
        KeyCode::Char('u') => Action::Undo,

        // Paging
        KeyCode::PageDown => invoke_with_count(builtins.line_down, 10),
        KeyCode::PageUp => invoke_with_count(builtins.line_up, 10),

        _ => Action::None,
    }
}

fn resolve_after_g(event: KeyEvent, builtins: &Builtins) -> Action {
    // `gg`: jump to first line. Anything else cancels the pending state.
    match event.code {
        KeyCode::Char('g') => invoke(builtins.goto_first_line),
        _ => Action::SetPending(Pending::None),
    }
}

fn resolve_after_operator(
    event: KeyEvent,
    builtins: &Builtins,
    op: OperatorId,
) -> Action {
    if matches!(event.code, KeyCode::Esc) {
        return Action::SetPending(Pending::None);
    }
    // For now, only recognize a small set of motions as targets. Doubled
    // operator (e.g., `dd`) is a special case that maps to `Range::CurrentLine`.
    let target = match event.code {
        KeyCode::Char('w') => Target::Motion(builtins.word_forward, Args::None),
        KeyCode::Char('h') | KeyCode::Left => Target::Motion(builtins.char_left, Args::None),
        KeyCode::Char('l') | KeyCode::Right => Target::Motion(builtins.char_right, Args::None),
        KeyCode::Char('j') | KeyCode::Down => Target::Motion(builtins.line_down, Args::None),
        KeyCode::Char('k') | KeyCode::Up => Target::Motion(builtins.line_up, Args::None),
        KeyCode::Char('0') | KeyCode::Home => Target::Motion(builtins.line_start, Args::None),
        KeyCode::Char('$') | KeyCode::End => Target::Motion(builtins.line_end, Args::None),
        KeyCode::Char('d') if op == builtins.delete => {
            // `dd` -- delete current line. The dispatcher's CurrentLine range
            // covers the line content; the trailing newline is a known
            // limitation tracked in DESIGN.md §14 for proper linewise vim
            // semantics.
            return Action::Invoke(
                CommandInvocation::of(op.0).with_range(lattice_grammar::Range::CurrentLine),
            );
        }
        _ => return Action::SetPending(Pending::None),
    };
    Action::Invoke(CommandInvocation::of(op.0).with_target(target))
}

fn invoke(motion: MotionId) -> Action {
    Action::Invoke(CommandInvocation::of(motion.0))
}

fn invoke_with_count(motion: MotionId, count: u32) -> Action {
    Action::Invoke(
        CommandInvocation::of(motion.0).with_count(lattice_grammar::command::Count(count)),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::CommandRegistry;
    use lattice_grammar::builtins::populate;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn fixture() -> (CommandRegistry, Builtins) {
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        (r, b)
    }

    fn ctx<'a>(modal: ModalState, pending: Pending, b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext { modal, pending, builtins: b }
    }

    fn invocation_command(action: &Action) -> Option<lattice_protocol::ids::CommandId> {
        if let Action::Invoke(inv) = action {
            Some(inv.command)
        } else {
            None
        }
    }

    // ---- Universal ----

    #[test]
    fn ctrl_c_quits_in_any_mode() {
        let (_, b) = fixture();
        for modal in [ModalState::Normal, ModalState::Insert] {
            assert!(matches!(
                translate(ctx(modal, Pending::None, &b), ctrl(KeyCode::Char('c'))),
                Action::Quit
            ));
        }
    }

    // ---- Normal mode motions ----

    #[test]
    fn hjkl_invoke_corresponding_motions() {
        let (_, b) = fixture();
        let cases = [
            (KeyCode::Char('h'), b.char_left.0),
            (KeyCode::Char('j'), b.line_down.0),
            (KeyCode::Char('k'), b.line_up.0),
            (KeyCode::Char('l'), b.char_right.0),
        ];
        for (code, expected) in cases {
            let action = translate(ctx(ModalState::Normal, Pending::None, &b), key(code));
            assert_eq!(invocation_command(&action), Some(expected));
        }
    }

    #[test]
    fn arrows_alias_hjkl() {
        let (_, b) = fixture();
        let cases = [
            (KeyCode::Left, b.char_left.0),
            (KeyCode::Down, b.line_down.0),
            (KeyCode::Up, b.line_up.0),
            (KeyCode::Right, b.char_right.0),
        ];
        for (code, expected) in cases {
            let action = translate(ctx(ModalState::Normal, Pending::None, &b), key(code));
            assert_eq!(invocation_command(&action), Some(expected));
        }
    }

    #[test]
    fn zero_and_dollar_invoke_line_start_and_end() {
        let (_, b) = fixture();
        assert_eq!(
            invocation_command(&translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('0'))
            )),
            Some(b.line_start.0)
        );
        assert_eq!(
            invocation_command(&translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('$'))
            )),
            Some(b.line_end.0)
        );
    }

    #[test]
    fn capital_g_invokes_goto_last_line() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, Pending::None, &b), key(KeyCode::Char('G')));
        assert_eq!(invocation_command(&action), Some(b.goto_last_line.0));
    }

    #[test]
    fn first_g_sets_pending_state() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('g'))
            ),
            Action::SetPending(Pending::AfterG)
        ));
    }

    #[test]
    fn second_g_with_pending_resolves_to_goto_first_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterG, &b),
            key(KeyCode::Char('g')),
        );
        assert_eq!(invocation_command(&action), Some(b.goto_first_line.0));
    }

    #[test]
    fn unrelated_key_after_pending_g_clears_pending() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::AfterG, &b),
                key(KeyCode::Char('z'))
            ),
            Action::SetPending(Pending::None)
        ));
    }

    // ---- Mode entry ----

    #[test]
    fn i_enters_insert_mode() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('i'))
            ),
            Action::EnterMode(ModalState::Insert)
        ));
    }

    #[test]
    fn a_enters_append_mode() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('a'))
            ),
            Action::EnterAppend
        ));
    }

    #[test]
    fn o_opens_line_below() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('o'))
            ),
            Action::OpenLineBelow
        ));
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('O'))
            ),
            Action::OpenLineAbove
        ));
    }

    // ---- Operator-pending state ----

    #[test]
    fn d_sets_pending_operator() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('d')),
        );
        match action {
            Action::SetPending(Pending::AfterOperator(op)) => assert_eq!(op, b.delete),
            _ => panic!("expected SetPending(AfterOperator(delete))"),
        }
    }

    #[test]
    fn dw_resolves_to_delete_with_word_forward_target() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
            key(KeyCode::Char('w')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.word_forward),
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn dd_resolves_to_delete_current_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
            key(KeyCode::Char('d')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn esc_after_operator_cancels_pending() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
                key(KeyCode::Esc)
            ),
            Action::SetPending(Pending::None)
        ));
    }

    #[test]
    fn x_resolves_directly_to_delete_char_right() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('x')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.char_right),
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    // ---- Insert mode ----

    #[test]
    fn esc_in_insert_returns_to_normal() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Insert, Pending::None, &b), key(KeyCode::Esc)),
            Action::EnterMode(ModalState::Normal)
        ));
    }

    #[test]
    fn printable_char_in_insert_inserts_text() {
        let (_, b) = fixture();
        match translate(
            ctx(ModalState::Insert, Pending::None, &b),
            key(KeyCode::Char('h')),
        ) {
            Action::Insert(s) => assert_eq!(s, "h"),
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn enter_in_insert_inserts_newline() {
        let (_, b) = fixture();
        match translate(
            ctx(ModalState::Insert, Pending::None, &b),
            key(KeyCode::Enter),
        ) {
            Action::Insert(s) => assert_eq!(s, "\n"),
            _ => panic!("expected Insert(\"\\n\")"),
        }
    }

    #[test]
    fn backspace_in_insert_deletes_char_backward() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Insert, Pending::None, &b),
                key(KeyCode::Backspace)
            ),
            Action::DeleteCharBackward
        ));
    }

    // ---- Undo / Redo ----

    #[test]
    fn u_in_normal_undoes() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('u'))
            ),
            Action::Undo
        ));
    }

    #[test]
    fn ctrl_r_in_normal_redoes() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                ctrl(KeyCode::Char('r'))
            ),
            Action::Redo
        ));
    }

    // ---- Command modal ----

    #[test]
    fn colon_in_normal_enters_command_line() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char(':'))
            ),
            Action::EnterCommandLine
        ));
    }

    #[test]
    fn printable_char_in_command_appends() {
        let (_, b) = fixture();
        match translate(
            ctx(ModalState::Command, Pending::None, &b),
            key(KeyCode::Char('w')),
        ) {
            Action::CommandLineAppend(c) => assert_eq!(c, 'w'),
            other => panic!("expected CommandLineAppend, got {other:?}"),
        }
    }

    #[test]
    fn enter_in_command_submits() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Command, Pending::None, &b),
                key(KeyCode::Enter)
            ),
            Action::CommandLineSubmit
        ));
    }

    #[test]
    fn esc_in_command_cancels() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, Pending::None, &b), key(KeyCode::Esc)),
            Action::CommandLineCancel
        ));
    }

    #[test]
    fn backspace_in_command_pops() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Command, Pending::None, &b),
                key(KeyCode::Backspace)
            ),
            Action::CommandLineBackspace
        ));
    }

    #[test]
    fn ctrl_c_in_command_quits_immediately() {
        // Universal ctrl+c quits regardless of mode -- the user shouldn't
        // need to cancel the command line first.
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Command, Pending::None, &b),
                ctrl(KeyCode::Char('c'))
            ),
            Action::Quit
        ));
    }
}
