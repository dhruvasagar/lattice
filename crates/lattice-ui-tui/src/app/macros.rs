//! Macro recording / playback -- App surface for `q`,
//! `q{reg}`, `@{reg}`, `@@`.
//!
//! Methods that live here:
//! - `do_start_macro_record`, `do_stop_macro_record`,
//!   `do_play_macro`. The `@@` `PlayLastMacro` action is
//!   dispatched in `App::apply` to `do_play_macro` with
//!   the cached register.
//!
//! What does NOT live here: the dispatch layer itself, the
//! `MacroRecording` struct (lives in `app.rs` next to the
//! `App` field), or `recording_insert` (insert-mode
//! keystroke recording, distinct from `q`-macros).


// 5.5.G.23.macros: `do_play_macro` migrated to
// [`lattice_host::dispatch::Editor::do_play_macro`]. Recorded actions
// flow through `out.next_actions`, which the dispatch wrapper drains
// via `self.apply(action)` with a `should_quit` short-circuit. The
// recording-suspend during replay also lives host-side.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::test_helpers::app_with;
    use crate::app::*;
    use lattice_grammar::{Args, CommandInvocation, ModalState, Target};

    #[test]
    fn start_macro_record_seeds_recording_state() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        assert!(a.editor.macro_recording.is_some());
        assert_eq!(a.editor.macro_recording.as_ref().unwrap().register, 'a');
    }

    #[test]
    fn invalid_macro_register_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord(' '));
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(a.editor.macro_recording.is_none());
    }

    #[test]
    fn second_q_during_recording_does_not_double_start() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        a.apply(Action::StartMacroRecord('b'));
        // Still recording into 'a'.
        assert_eq!(a.editor.macro_recording.as_ref().unwrap().register, 'a');
    }

    #[test]
    fn stop_macro_record_persists_actions_and_clears_recording() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::StopMacroRecord);
        assert!(a.editor.macro_recording.is_none());
        let actions = a.editor.macros.get(&'a').unwrap();
        assert!(!actions.is_empty());
    }

    #[test]
    fn play_macro_replays_recorded_actions() {
        let mut a = app_with("foo bar", 10);
        a.apply(Action::StartMacroRecord('a'));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_target(Target::Motion(a.editor.builtins.word_forward, Args::None));
        a.apply(Action::Invoke(inv));
        a.apply(Action::StopMacroRecord);
        // After dw: "bar".
        assert_eq!(a.editor.document.text(), "bar");
        // Replay -> deletes another word.
        a.apply(Action::PlayMacro('a'));
        assert_eq!(a.editor.document.text(), "");
    }

    #[test]
    fn play_unrecorded_macro_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::PlayMacro('z'));
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn at_at_replays_last_macro() {
        let mut a = app_with("foo bar baz qux", 10);
        a.apply(Action::StartMacroRecord('a'));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_target(Target::Motion(a.editor.builtins.word_forward, Args::None));
        a.apply(Action::Invoke(inv));
        a.apply(Action::StopMacroRecord);
        // First play.
        a.apply(Action::PlayMacro('a'));
        // @@ now repeats.
        a.apply(Action::PlayLastMacro);
        // After three dws total: "qux".
        assert_eq!(a.editor.document.text(), "qux");
    }

    #[test]
    fn play_last_macro_with_no_history_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::PlayLastMacro);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn macro_does_not_record_management_actions() {
        // StartMacroRecord, StopMacroRecord, PlayMacro, PlayLastMacro
        // must NOT appear inside the recorded action stream (otherwise
        // playback would recurse / break).
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        // Replay another (unrecorded) macro -- the play action must not
        // be captured.
        a.apply(Action::PlayLastMacro); // errors but is not recorded
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::StopMacroRecord);
        let actions = a.editor.macros.get(&'a').unwrap();
        for action in actions {
            assert!(!matches!(
                action,
                Action::StartMacroRecord(_)
                    | Action::StopMacroRecord
                    | Action::PlayMacro(_)
                    | Action::PlayLastMacro
            ));
        }
    }
}
