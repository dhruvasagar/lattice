//! Translate `crossterm` key events into `Action`s.
//!
//! This is a small, pure function that reads modal state, the pending-key
//! buffer, and the catalog of built-in command IDs to decide what each key
//! press means. It is the v1 stand-in for the layered keymap engine
//! described in DESIGN.md §5.2.3 -- the *shape* matches (chord -> typed
//! invocation) so swapping in a real keymap layer later is mechanical.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use lattice_grammar::ModalState;
use lattice_grammar::SearchDirection;
use lattice_grammar::Target;
use lattice_grammar::args::Args;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::registry::{MotionId, OperatorId};

use crate::app::{Action, FindKind, Pending};

pub struct TranslateContext<'a> {
    pub modal: ModalState,
    pub pending: Pending,
    pub builtins: &'a Builtins,
    /// In-progress count prefix; `0` means none. Translate uses this to
    /// disambiguate the `0` key (line_start when no count in progress;
    /// digit-zero appended to count otherwise).
    pub pending_count: u32,
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
        ModalState::Normal => {
            translate_normal(event, ctx.pending, ctx.builtins, ctx.pending_count)
        }
        ModalState::Command => translate_command(event),
        ModalState::Search(_) => translate_search(event),
        // The other modal states route to no-op until their respective
        // phases land.
        _ => Action::None,
    }
}

fn translate_search(event: KeyEvent) -> Action {
    match event.code {
        KeyCode::Esc => Action::SearchCancel,
        KeyCode::Enter => Action::SearchSubmit,
        KeyCode::Backspace => Action::SearchBackspace,
        KeyCode::Char(c) if !event.modifiers.contains(KeyModifiers::CONTROL) => {
            Action::SearchAppend(c)
        }
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

fn translate_normal(
    event: KeyEvent,
    pending: Pending,
    builtins: &Builtins,
    pending_count: u32,
) -> Action {
    // Resolve any pending state first.
    match pending {
        Pending::AfterG => return resolve_after_g(event, builtins),
        Pending::AfterOperator(op) => return resolve_after_operator(event, builtins, op),
        Pending::AfterFindChar { kind, operator } => {
            return resolve_after_find_char(event, builtins, kind, operator);
        }
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

    // Numeric prefix: `1`-`9` always start (or extend) a count; `0` extends
    // an in-progress count but otherwise is line_start. This is vim's
    // standard count parsing, exactly.
    if let KeyCode::Char(c) = event.code
        && let Some(digit) = c.to_digit(10)
        && (digit > 0 || pending_count > 0)
    {
        return Action::PushDigit(digit as u8);
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
        KeyCode::Char('^') => invoke(builtins.first_non_blank),
        KeyCode::Char('w') => invoke(builtins.word_forward),
        KeyCode::Char('b') => invoke(builtins.word_backward),
        KeyCode::Char('e') => invoke(builtins.word_end),
        KeyCode::Char('G') => invoke(builtins.goto_last_line),

        // Pending key sequences
        KeyCode::Char('g') => Action::SetPending(Pending::AfterG),

        // Operator-leading keys
        KeyCode::Char('d') => Action::SetPending(Pending::AfterOperator(builtins.delete)),
        KeyCode::Char('c') => Action::SetPending(Pending::AfterOperator(builtins.change)),
        KeyCode::Char('y') => Action::SetPending(Pending::AfterOperator(builtins.yank)),

        // Paste
        KeyCode::Char('p') => Action::PasteAfter,
        KeyCode::Char('P') => Action::PasteBefore,

        // Linewise yank shortcut: `Y` is equivalent to `yy` in vim's defaults.
        KeyCode::Char('Y') => Action::Invoke(
            CommandInvocation::of(builtins.yank.0).with_range(lattice_grammar::Range::CurrentLine),
        ),

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

        // Search
        KeyCode::Char('/') => Action::EnterSearch(SearchDirection::Forward),
        KeyCode::Char('?') => Action::EnterSearch(SearchDirection::Backward),
        KeyCode::Char('n') => Action::SearchNext,
        KeyCode::Char('N') => Action::SearchPrevious,

        // Find-char on the current line
        KeyCode::Char('f') => Action::SetPending(Pending::AfterFindChar {
            kind: FindKind::Forward,
            operator: None,
        }),
        KeyCode::Char('F') => Action::SetPending(Pending::AfterFindChar {
            kind: FindKind::Backward,
            operator: None,
        }),
        KeyCode::Char('t') => Action::SetPending(Pending::AfterFindChar {
            kind: FindKind::TillForward,
            operator: None,
        }),
        KeyCode::Char('T') => Action::SetPending(Pending::AfterFindChar {
            kind: FindKind::TillBackward,
            operator: None,
        }),

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
        KeyCode::Char('b') => Target::Motion(builtins.word_backward, Args::None),
        KeyCode::Char('e') => Target::Motion(builtins.word_end, Args::None),
        KeyCode::Char('h') | KeyCode::Left => Target::Motion(builtins.char_left, Args::None),
        KeyCode::Char('l') | KeyCode::Right => Target::Motion(builtins.char_right, Args::None),
        KeyCode::Char('j') | KeyCode::Down => Target::Motion(builtins.line_down, Args::None),
        KeyCode::Char('k') | KeyCode::Up => Target::Motion(builtins.line_up, Args::None),
        KeyCode::Char('0') | KeyCode::Home => Target::Motion(builtins.line_start, Args::None),
        KeyCode::Char('$') | KeyCode::End => Target::Motion(builtins.line_end, Args::None),
        KeyCode::Char('^') => Target::Motion(builtins.first_non_blank, Args::None),
        KeyCode::Char('d') if op == builtins.delete => {
            // `dd` -- delete current line. The dispatcher's CurrentLine range
            // covers the line content; the trailing newline is a known
            // limitation tracked in DESIGN.md §14 for proper linewise vim
            // semantics.
            return Action::Invoke(
                CommandInvocation::of(op.0).with_range(lattice_grammar::Range::CurrentLine),
            );
        }
        KeyCode::Char('c') if op == builtins.change => {
            // `cc` -- change current line: clear the line content and enter
            // Insert (the `change` operator handles the mode transition).
            return Action::Invoke(
                CommandInvocation::of(op.0).with_range(lattice_grammar::Range::CurrentLine),
            );
        }
        KeyCode::Char('y') if op == builtins.yank => {
            // `yy` -- yank current line into the unnamed register (linewise).
            return Action::Invoke(
                CommandInvocation::of(op.0).with_range(lattice_grammar::Range::CurrentLine),
            );
        }
        KeyCode::Char('f') => {
            return Action::SetPending(Pending::AfterFindChar {
                kind: FindKind::Forward,
                operator: Some(op),
            });
        }
        KeyCode::Char('F') => {
            return Action::SetPending(Pending::AfterFindChar {
                kind: FindKind::Backward,
                operator: Some(op),
            });
        }
        KeyCode::Char('t') => {
            return Action::SetPending(Pending::AfterFindChar {
                kind: FindKind::TillForward,
                operator: Some(op),
            });
        }
        KeyCode::Char('T') => {
            return Action::SetPending(Pending::AfterFindChar {
                kind: FindKind::TillBackward,
                operator: Some(op),
            });
        }
        _ => return Action::SetPending(Pending::None),
    };
    Action::Invoke(CommandInvocation::of(op.0).with_target(target))
}

fn resolve_after_find_char(
    event: KeyEvent,
    builtins: &Builtins,
    kind: FindKind,
    operator: Option<OperatorId>,
) -> Action {
    if matches!(event.code, KeyCode::Esc) {
        return Action::SetPending(Pending::None);
    }
    let needle = match event.code {
        KeyCode::Char(c) => c,
        _ => return Action::SetPending(Pending::None),
    };
    let motion_id = match kind {
        FindKind::Forward => builtins.find_char_forward,
        FindKind::Backward => builtins.find_char_backward,
        FindKind::TillForward => builtins.till_char_forward,
        FindKind::TillBackward => builtins.till_char_backward,
    };
    match operator {
        None => Action::Invoke(CommandInvocation::of(motion_id.0).with_args(Args::Char(needle))),
        Some(op) => Action::Invoke(
            CommandInvocation::of(op.0).with_target(Target::Motion(motion_id, Args::Char(needle))),
        ),
    }
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
        TranslateContext {
            modal,
            pending,
            builtins: b,
            pending_count: 0,
        }
    }

    fn ctx_with_count<'a>(
        modal: ModalState,
        pending: Pending,
        b: &'a Builtins,
        pending_count: u32,
    ) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            pending,
            builtins: b,
            pending_count,
        }
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

    // ---- Search modal ----

    #[test]
    fn slash_in_normal_enters_forward_search() {
        let (_, b) = fixture();
        match translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('/')),
        ) {
            Action::EnterSearch(SearchDirection::Forward) => {}
            other => panic!("expected EnterSearch(Forward), got {other:?}"),
        }
    }

    #[test]
    fn question_in_normal_enters_backward_search() {
        let (_, b) = fixture();
        match translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('?')),
        ) {
            Action::EnterSearch(SearchDirection::Backward) => {}
            other => panic!("expected EnterSearch(Backward), got {other:?}"),
        }
    }

    #[test]
    fn n_in_normal_repeats_search_forward() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('n'))
            ),
            Action::SearchNext
        ));
    }

    #[test]
    fn capital_n_in_normal_repeats_search_reverse() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char('N'))
            ),
            Action::SearchPrevious
        ));
    }

    #[test]
    fn printable_char_in_search_appends_to_pattern() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        match translate(ctx(modal, Pending::None, &b), key(KeyCode::Char('f'))) {
            Action::SearchAppend(c) => assert_eq!(c, 'f'),
            other => panic!("expected SearchAppend, got {other:?}"),
        }
    }

    #[test]
    fn enter_in_search_submits() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        assert!(matches!(
            translate(ctx(modal, Pending::None, &b), key(KeyCode::Enter)),
            Action::SearchSubmit
        ));
    }

    #[test]
    fn esc_in_search_cancels() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Backward);
        assert!(matches!(
            translate(ctx(modal, Pending::None, &b), key(KeyCode::Esc)),
            Action::SearchCancel
        ));
    }

    #[test]
    fn backspace_in_search_pops_pattern() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        assert!(matches!(
            translate(ctx(modal, Pending::None, &b), key(KeyCode::Backspace)),
            Action::SearchBackspace
        ));
    }

    #[test]
    fn ctrl_c_in_search_quits_immediately() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        assert!(matches!(
            translate(ctx(modal, Pending::None, &b), ctrl(KeyCode::Char('c'))),
            Action::Quit
        ));
    }

    // ---- Count prefix (1-9, 0 with count in progress) ----

    #[test]
    fn digit_1_to_9_emits_push_digit_in_normal_mode() {
        let (_, b) = fixture();
        for digit in 1u8..=9 {
            let c = char::from_digit(digit as u32, 10).unwrap();
            let action = translate(
                ctx(ModalState::Normal, Pending::None, &b),
                key(KeyCode::Char(c)),
            );
            assert!(matches!(action, Action::PushDigit(d) if d == digit));
        }
    }

    #[test]
    fn zero_with_no_count_invokes_line_start() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('0')),
        );
        assert_eq!(invocation_command(&action), Some(b.line_start.0));
    }

    #[test]
    fn zero_with_count_in_progress_extends_count() {
        let (_, b) = fixture();
        // pending_count == 1 -> '0' becomes a digit, not line_start.
        let action = translate(
            ctx_with_count(ModalState::Normal, Pending::None, &b, 1),
            key(KeyCode::Char('0')),
        );
        assert!(matches!(action, Action::PushDigit(0)));
    }

    #[test]
    fn digit_after_count_extends_count() {
        let (_, b) = fixture();
        let action = translate(
            ctx_with_count(ModalState::Normal, Pending::None, &b, 12),
            key(KeyCode::Char('3')),
        );
        // Translate just emits the digit; App accumulates 12 -> 123.
        assert!(matches!(action, Action::PushDigit(3)));
    }

    #[test]
    fn motion_after_count_dispatches_motion() {
        let (_, b) = fixture();
        let action = translate(
            ctx_with_count(ModalState::Normal, Pending::None, &b, 3),
            key(KeyCode::Char('w')),
        );
        // Translate doesn't attach the count -- App applies it on Invoke.
        assert_eq!(invocation_command(&action), Some(b.word_forward.0));
    }

    // ---- Find-char / till-char (f, F, t, T) ----

    #[test]
    fn f_sets_pending_find_forward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('f')),
        );
        match action {
            Action::SetPending(Pending::AfterFindChar { kind, operator }) => {
                assert_eq!(kind, FindKind::Forward);
                assert!(operator.is_none());
            }
            other => panic!("expected SetPending(AfterFindChar Forward), got {other:?}"),
        }
    }

    #[test]
    fn capital_f_sets_pending_find_backward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('F')),
        );
        match action {
            Action::SetPending(Pending::AfterFindChar { kind, .. }) => {
                assert_eq!(kind, FindKind::Backward);
            }
            other => panic!("expected SetPending(AfterFindChar Backward), got {other:?}"),
        }
    }

    #[test]
    fn t_sets_pending_till_forward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('t')),
        );
        match action {
            Action::SetPending(Pending::AfterFindChar { kind, .. }) => {
                assert_eq!(kind, FindKind::TillForward);
            }
            other => panic!("expected SetPending(AfterFindChar TillForward), got {other:?}"),
        }
    }

    #[test]
    fn capital_t_sets_pending_till_backward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('T')),
        );
        match action {
            Action::SetPending(Pending::AfterFindChar { kind, .. }) => {
                assert_eq!(kind, FindKind::TillBackward);
            }
            other => panic!("expected SetPending(AfterFindChar TillBackward), got {other:?}"),
        }
    }

    #[test]
    fn f_then_char_resolves_to_motion_with_args_char() {
        let (_, b) = fixture();
        let pending = Pending::AfterFindChar {
            kind: FindKind::Forward,
            operator: None,
        };
        let action = translate(
            ctx(ModalState::Normal, pending, &b),
            key(KeyCode::Char('z')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.find_char_forward.0);
                assert_eq!(inv.args, lattice_grammar::Args::Char('z'));
            }
            other => panic!("expected Invoke(find_char_forward), got {other:?}"),
        }
    }

    #[test]
    fn df_then_char_composes_delete_with_find_target() {
        let (_, b) = fixture();
        // First press: `d` in Normal -> AfterOperator(delete).
        let after_d = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('d')),
        );
        let op = match after_d {
            Action::SetPending(Pending::AfterOperator(op)) => op,
            other => panic!("expected SetPending(AfterOperator), got {other:?}"),
        };
        // Second press: `f` in operator-pending -> AfterFindChar with stashed op.
        let after_df = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(op), &b),
            key(KeyCode::Char('f')),
        );
        let pending = match after_df {
            Action::SetPending(p) => p,
            other => panic!("expected SetPending, got {other:?}"),
        };
        // Third press: `x` -> Invoke delete with find_char_forward target.
        let after_dfx = translate(
            ctx(ModalState::Normal, pending, &b),
            key(KeyCode::Char('x')),
        );
        match after_dfx {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(id, args)) => {
                        assert_eq!(id, b.find_char_forward);
                        assert_eq!(args, lattice_grammar::Args::Char('x'));
                    }
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            other => panic!("expected Invoke(delete, find_target), got {other:?}"),
        }
    }

    #[test]
    fn esc_after_find_pending_clears_pending() {
        let (_, b) = fixture();
        let pending = Pending::AfterFindChar {
            kind: FindKind::Forward,
            operator: None,
        };
        let action = translate(ctx(ModalState::Normal, pending, &b), key(KeyCode::Esc));
        assert!(matches!(action, Action::SetPending(Pending::None)));
    }

    // ---- New motions: b, e, ^ ----

    #[test]
    fn b_invokes_word_backward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('b')),
        );
        assert_eq!(invocation_command(&action), Some(b.word_backward.0));
    }

    #[test]
    fn e_invokes_word_end() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('e')),
        );
        assert_eq!(invocation_command(&action), Some(b.word_end.0));
    }

    #[test]
    fn caret_invokes_first_non_blank() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('^')),
        );
        assert_eq!(invocation_command(&action), Some(b.first_non_blank.0));
    }

    #[test]
    fn db_resolves_to_delete_word_backward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
            key(KeyCode::Char('b')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.word_backward),
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn de_resolves_to_delete_word_end() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
            key(KeyCode::Char('e')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::Motion(id, _)) => assert_eq!(id, b.word_end),
                other => panic!("expected motion target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }

    // ---- change operator: c, cw, cc ----

    #[test]
    fn c_sets_pending_operator_change() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('c')),
        );
        match action {
            Action::SetPending(Pending::AfterOperator(op)) => assert_eq!(op, b.change),
            other => panic!("expected SetPending(AfterOperator(change)), got {other:?}"),
        }
    }

    #[test]
    fn cw_resolves_to_change_with_word_forward_target() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.change), &b),
            key(KeyCode::Char('w')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.word_forward),
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn cc_resolves_to_change_with_current_line_range() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.change), &b),
            key(KeyCode::Char('c')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            _ => panic!("expected Invoke"),
        }
    }

    // ---- yank operator + paste ----

    #[test]
    fn y_sets_pending_operator_yank() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('y')),
        );
        match action {
            Action::SetPending(Pending::AfterOperator(op)) => assert_eq!(op, b.yank),
            other => panic!("expected SetPending(AfterOperator(yank)), got {other:?}"),
        }
    }

    #[test]
    fn yw_resolves_to_yank_word_forward() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.yank), &b),
            key(KeyCode::Char('w')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.yank.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.word_forward),
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn yy_resolves_to_yank_current_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.yank), &b),
            key(KeyCode::Char('y')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.yank.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn capital_y_aliases_to_yank_current_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('Y')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.yank.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn p_lowercase_is_paste_after() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('p')),
        );
        assert!(matches!(action, Action::PasteAfter));
    }

    #[test]
    fn p_uppercase_is_paste_before() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::None, &b),
            key(KeyCode::Char('P')),
        );
        assert!(matches!(action, Action::PasteBefore));
    }

    #[test]
    fn dd_is_not_treated_as_change_current_line() {
        // Regression check: the `cc` arm should only fire for op == change.
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
            key(KeyCode::Char('c')),
        );
        // Delete operator + 'c' key: no specific motion, fallback clears pending.
        assert!(matches!(action, Action::SetPending(Pending::None)));
    }

    #[test]
    fn d_caret_resolves_to_delete_first_non_blank() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Normal, Pending::AfterOperator(b.delete), &b),
            key(KeyCode::Char('^')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::Motion(id, _)) => assert_eq!(id, b.first_non_blank),
                other => panic!("expected motion target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }
}
