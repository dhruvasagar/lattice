//! Crossterm → KeyChord adapter shim for the host-side dispatch.
//!
//! Phase 5.4 / slice 5 moved the renderer-neutral dispatch logic
//! (`TranslateContext`, `translate`, every per-mode translator)
//! into [`lattice_host::input`]. This module now contains only:
//!
//! - A re-export of [`TranslateContext`] so the runtime / tests
//!   keep their `crate::input::TranslateContext` path working.
//! - A thin `translate(ctx, event: KeyEvent) -> Action` that
//!   converts the crossterm event into a canonical [`KeyChord`]
//!   via [`crate::chord::from_event`] and forwards to
//!   [`lattice_host::input::translate`]. Events that have no
//!   chord representation (release events on terminals that emit
//!   them, modifier-only presses) return [`Action::None`].
//!
//! The future `lattice-ui-gpui` ships its own analogous shim
//! over GPUI key events feeding the same host-side dispatch.

use crossterm::event::KeyEvent;

use crate::app::Action;

pub use lattice_host::input::TranslateContext;

pub fn translate(ctx: TranslateContext<'_>, event: KeyEvent) -> Action {
    let Some(chord) = crate::chord::from_event(&event) else {
        return Action::None;
    };
    lattice_host::input::translate(ctx, chord)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, unused_imports)]
    use super::*;
    use crate::buffers::BufferKind;
    use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
    use crate::keymap_registry::KeymapHandle;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lattice_grammar::CommandRegistry;
    use lattice_grammar::ModalState;
    use lattice_grammar::SearchDirection;
    use lattice_grammar::Target;
    use lattice_grammar::VisualKind;
    use lattice_grammar::builtins::{Builtins, populate};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// Process-wide shared [`Builtins`] + [`crate::actions::ActionIds`].
    /// Built once on first access from a single shared registry;
    /// returned as a static reference so every test fixture in
    /// this module sees the same id space. `fixture()` returns a
    /// copy; the scenario-specific `shared_keymap_*` helpers below
    /// register against the same ids so trie-bound
    /// `CommandInvocation` ids stay in lockstep with what each
    /// test compares against.
    fn shared_init() -> &'static (Builtins, crate::actions::ActionIds) {
        use std::sync::OnceLock;
        static INIT: OnceLock<(Builtins, crate::actions::ActionIds)> = OnceLock::new();
        INIT.get_or_init(|| {
            let mut r = CommandRegistry::new();
            let b = populate(&mut r);
            let _ex = lattice_grammar::ex_commands::populate(&mut r);
            let a = crate::actions::populate(&mut r, &b);
            (b, a)
        })
    }

    fn shared_builtins() -> &'static Builtins {
        &shared_init().0
    }

    fn shared_actions() -> &'static crate::actions::ActionIds {
        &shared_init().1
    }

    /// Build a fresh `KeymapHandle` populated with every catalog
    /// the per-mode dispatchers consult: Replace, Visual, Insert,
    /// Normal. Each scenario-specific helper below starts from
    /// this and pushes the relevant minor-mode overlays.
    fn build_base_keymap() -> KeymapHandle {
        let h = KeymapHandle::new();
        let b = shared_builtins();
        let a = shared_actions();
        crate::keymap_replace::register_replace_bindings(&h, a);
        crate::keymap_visual::register_visual_bindings(&h, b, a);
        crate::keymap_insert::register_insert_bindings(&h, a);
        crate::keymap_normal::register_normal_bindings(&h, b, a);
        h
    }

    /// Base keymap -- no minor-mode overlays pushed. Default
    /// scenario for the bulk of `ctx*` test builders.
    fn shared_keymap_base() -> &'static KeymapHandle {
        use std::sync::OnceLock;
        static H: OnceLock<KeymapHandle> = OnceLock::new();
        H.get_or_init(build_base_keymap)
    }

    /// Base keymap + completion-popup minor-mode layer.
    fn shared_keymap_with_popup() -> &'static KeymapHandle {
        use std::sync::OnceLock;
        static H: OnceLock<KeymapHandle> = OnceLock::new();
        H.get_or_init(|| {
            let h = build_base_keymap();
            h.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode(
                    crate::keymap_insert::completion_popup_mode_id(),
                ),
                "completion-popup",
                crate::keymap_insert::completion_popup_layer_bindings(shared_actions()),
            );
            h
        })
    }

    /// Base keymap + active-snippet minor-mode layer.
    fn shared_keymap_with_snippet() -> &'static KeymapHandle {
        use std::sync::OnceLock;
        static H: OnceLock<KeymapHandle> = OnceLock::new();
        H.get_or_init(|| {
            let h = build_base_keymap();
            h.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode(
                    crate::keymap_insert::active_snippet_mode_id(),
                ),
                "active-snippet",
                crate::keymap_insert::active_snippet_layer_bindings(shared_actions()),
            );
            h
        })
    }

    /// Base keymap + both overlays. Push order matches
    /// `App::sync_keymap_overlays`: snippet first, popup
    /// second, so popup wins on overlapping chords.
    fn shared_keymap_with_both_overlays() -> &'static KeymapHandle {
        use std::sync::OnceLock;
        static H: OnceLock<KeymapHandle> = OnceLock::new();
        H.get_or_init(|| {
            let h = build_base_keymap();
            h.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode(
                    crate::keymap_insert::active_snippet_mode_id(),
                ),
                "active-snippet",
                crate::keymap_insert::active_snippet_layer_bindings(shared_actions()),
            );
            h.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode(
                    crate::keymap_insert::completion_popup_mode_id(),
                ),
                "completion-popup",
                crate::keymap_insert::completion_popup_layer_bindings(shared_actions()),
            );
            h
        })
    }

    fn fixture() -> (CommandRegistry, Builtins) {
        // Tests discard the registry (every caller binds `_`);
        // we still return one for signature compat. The shared
        // `Builtins` carries the canonical ids the keymap
        // registry references.
        let r = CommandRegistry::new();
        (r, *shared_builtins())
    }

    fn test_keymap() -> &'static KeymapHandle {
        shared_keymap_base()
    }

    fn ctx<'a>(modal: ModalState, b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    /// Slice 8.i.4.a: build a `TranslateContext` simulating
    /// "the user has just pressed `partial_chord` and the trie
    /// returned `Partial`, so `App::partial_chord` is now this
    /// slice." Replaces `ctx(modal, Pending::AfterG, b)` and
    /// siblings for the 9 migrated simple-prefix Pending
    /// variants. Tests using parameterised pendings
    /// (`AfterOperator(_)`, `AfterTextObject{_}`,
    /// `AfterFindChar{_}`, `AfterCtrlX`) keep using `ctx` until
    /// 8.i.4.b retires those.
    fn ctx_partial<'a>(
        modal: ModalState,
        partial: &'a [crate::chord::KeyChord],
        b: &'a Builtins,
    ) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: partial,
            active_minor_modes: &[],
        }
    }

    fn ctx_with_count<'a>(
        modal: ModalState,
        b: &'a Builtins,
        pending_count: u32,
    ) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    /// Test-fixture: a `TranslateContext` with explicit
    /// pending + op counts. Currently unused (the migrated
    /// operator-flow tests build the context inline) but kept
    /// alongside the other fixture builders for symmetry; the
    /// next op-count regression test will reach for it.
    #[allow(dead_code)]
    fn ctx_with_op_count<'a>(
        modal: ModalState,
        b: &'a Builtins,
        pending_count: u32,
        op_count: u32,
    ) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count,
            op_count,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    fn ctx_recording<'a>(modal: ModalState, b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: true,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    /// Slice 8.i.4.c: helper for tests that simulate "after
    /// operator was pressed": pass the operator's chord prefix
    /// as `partial` and the latched op_count as `op_count`.
    /// Replaces `ctx_with_op_count(_, Pending::AfterOperator(_),
    /// _, _, _)` for the migrated AfterOperator flow.
    fn ctx_partial_with_op_count<'a>(
        modal: ModalState,
        partial: &'a [crate::chord::KeyChord],
        b: &'a Builtins,
        pending_count: u32,
        op_count: u32,
    ) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count,
            op_count,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: partial,
            active_minor_modes: &[],
        }
    }

    fn ctx_chord_capture<'a>(b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext {
            modal: ModalState::Command,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: true,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
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
                translate(ctx(modal, &b), ctrl(KeyCode::Char('c'))),
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
            let action = translate(ctx(ModalState::Normal, &b), key(code));
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
            let action = translate(ctx(ModalState::Normal, &b), key(code));
            assert_eq!(invocation_command(&action), Some(expected));
        }
    }

    #[test]
    fn zero_and_dollar_invoke_line_start_and_end() {
        let (_, b) = fixture();
        assert_eq!(
            invocation_command(&translate(
                ctx(ModalState::Normal, &b),
                key(KeyCode::Char('0'))
            )),
            Some(b.line_start.0)
        );
        assert_eq!(
            invocation_command(&translate(
                ctx(ModalState::Normal, &b),
                key(KeyCode::Char('$'))
            )),
            Some(b.line_end.0)
        );
    }

    #[test]
    fn capital_g_invokes_goto_last_line() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('G')));
        assert_eq!(invocation_command(&action), Some(b.goto_last_line.0));
    }

    #[test]
    fn first_g_absorbs_partial_chord() {
        // Slice 8.i.4.a: pressing `g` returns
        // `Action::AbsorbPartialChord(g_chord)` instead of
        // `Action::SetPending(Pending::AfterG)`. The trie's
        // `Partial` result drives the App's `partial_chord`
        // stack directly.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('g')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('g')
        ));
    }

    #[test]
    fn second_g_with_pending_resolves_to_goto_first_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('g')),
        );
        assert_eq!(invocation_command(&action), Some(b.goto_first_line.0));
    }

    #[test]
    fn unrelated_key_after_pending_g_clears_pending() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
                key(KeyCode::Char('z'))
            ),
            Action::None
        ));
    }

    // ---- Mode entry ----

    #[test]
    fn i_enters_insert_mode() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('i'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_mode_insert),
            other => panic!("expected Invoke(enter_mode_insert), got {other:?}"),
        }
    }

    #[test]
    fn a_enters_append_mode() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('a'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_append),
            other => panic!("expected Invoke(enter_append), got {other:?}"),
        }
    }

    #[test]
    fn o_opens_line_below() {
        let (_, b) = fixture();
        let a = shared_actions();
        // Slice 8.i.1.a: `o` / `O` are now `CommandKind::Action`
        // dispatch (`Effect::AppAction(AppEffect::OpenLine{Below,Above})`)
        // routed through `run_invocation`, surfaced at the
        // input layer as `Action::Invoke`.
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('o'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.open_line_below),
            other => panic!("expected Invoke(open_line_below), got {other:?}"),
        }
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('O'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.open_line_above),
            other => panic!("expected Invoke(open_line_above), got {other:?}"),
        }
    }

    // ---- Operator-pending state ----

    #[test]
    fn d_invokes_absorb_operator_delete() {
        // Slice 8.i.4.c: pressing `d` returns
        // `Action::Invoke(absorb_operator_delete)` instead of
        // `Action::SetPending(Pending::AfterOperator(delete))`.
        // The bound `ActionSpec` returns
        // `Effect::AppAction(AppEffect::AbsorbOperatorPrefix(delete))`,
        // which `App::apply_app_effect` translates into
        // `partial_chord = [d]` + `op_count` latching.
        let (_, b) = fixture();
        let a = shared_actions();
        let _ = b;
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('d')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_delete),
            _ => panic!("expected Invoke(absorb_operator_delete)"),
        }
    }

    #[test]
    fn dw_resolves_to_delete_with_word_forward_target() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
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
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
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
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
                key(KeyCode::Esc)
            ),
            Action::None
        ));
    }

    #[test]
    fn x_resolves_directly_to_delete_char_right() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('x')));
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
        let a = shared_actions();
        match translate(ctx(ModalState::Insert, &b), key(KeyCode::Esc)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_mode_normal),
            other => panic!("expected Invoke(enter_mode_normal), got {other:?}"),
        }
    }

    #[test]
    fn printable_char_in_insert_inserts_text() {
        let (_, b) = fixture();
        match translate(ctx(ModalState::Insert, &b), key(KeyCode::Char('h'))) {
            Action::Insert(s) => assert_eq!(s, "h"),
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn enter_in_insert_inserts_newline() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Insert, &b), key(KeyCode::Enter)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.insert_newline),
            other => panic!("expected Invoke(insert_newline), got {other:?}"),
        }
    }

    #[test]
    fn backspace_in_insert_deletes_char_backward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Insert, &b), key(KeyCode::Backspace)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.delete_char_backward),
            other => panic!("expected Invoke(delete_char_backward), got {other:?}"),
        }
    }

    // ---- Undo / Redo ----

    #[test]
    fn u_in_normal_undoes() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('u'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.undo),
            other => panic!("expected Invoke(undo), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_r_in_normal_redoes() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('r'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.redo),
            other => panic!("expected Invoke(redo), got {other:?}"),
        }
    }

    // ---- Command modal ----

    #[test]
    fn colon_in_normal_enters_command_line() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char(':'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_command_line),
            other => panic!("expected Invoke(enter_command_line), got {other:?}"),
        }
    }

    #[test]
    fn printable_char_in_command_appends() {
        let (_, b) = fixture();
        match translate(ctx(ModalState::Command, &b), key(KeyCode::Char('w'))) {
            Action::CommandLineAppend(c) => assert_eq!(c, 'w'),
            other => panic!("expected CommandLineAppend, got {other:?}"),
        }
    }

    #[test]
    fn enter_in_command_submits() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, &b), key(KeyCode::Enter)),
            Action::CommandLineSubmit
        ));
    }

    #[test]
    fn esc_in_command_cancels() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, &b), key(KeyCode::Esc)),
            Action::CommandLineCancel
        ));
    }

    #[test]
    fn backspace_in_command_pops() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, &b), key(KeyCode::Backspace)),
            Action::CommandLineBackspace
        ));
    }

    #[test]
    fn up_in_command_emits_history_prev() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, &b), key(KeyCode::Up)),
            Action::CommandLineHistoryPrev
        ));
    }

    #[test]
    fn down_in_command_emits_history_next() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, &b), key(KeyCode::Down)),
            Action::CommandLineHistoryNext
        ));
    }

    #[test]
    fn ctrl_c_in_command_quits_immediately() {
        // Universal ctrl+c quits regardless of mode -- the user shouldn't
        // need to cancel the command line first.
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx(ModalState::Command, &b), ctrl(KeyCode::Char('c'))),
            Action::Quit
        ));
    }

    // ---- Search modal ----

    #[test]
    fn slash_in_normal_enters_forward_search() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('/'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_search_forward),
            other => panic!("expected Invoke(enter_search_forward), got {other:?}"),
        }
    }

    #[test]
    fn question_in_normal_enters_backward_search() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('?'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_search_backward),
            other => panic!("expected Invoke(enter_search_backward), got {other:?}"),
        }
    }

    #[test]
    fn n_in_normal_repeats_search_forward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('n'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.search_next),
            other => panic!("expected Invoke(search_next), got {other:?}"),
        }
    }

    #[test]
    fn capital_n_in_normal_repeats_search_reverse() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('N'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.search_previous),
            other => panic!("expected Invoke(search_previous), got {other:?}"),
        }
    }

    #[test]
    fn printable_char_in_search_appends_to_pattern() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        match translate(ctx(modal, &b), key(KeyCode::Char('f'))) {
            Action::SearchAppend(c) => assert_eq!(c, 'f'),
            other => panic!("expected SearchAppend, got {other:?}"),
        }
    }

    #[test]
    fn enter_in_search_submits() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        assert!(matches!(
            translate(ctx(modal, &b), key(KeyCode::Enter)),
            Action::SearchSubmit
        ));
    }

    #[test]
    fn esc_in_search_cancels() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Backward);
        assert!(matches!(
            translate(ctx(modal, &b), key(KeyCode::Esc)),
            Action::SearchCancel
        ));
    }

    #[test]
    fn backspace_in_search_pops_pattern() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        assert!(matches!(
            translate(ctx(modal, &b), key(KeyCode::Backspace)),
            Action::SearchBackspace
        ));
    }

    #[test]
    fn ctrl_c_in_search_quits_immediately() {
        let (_, b) = fixture();
        let modal = ModalState::Search(SearchDirection::Forward);
        assert!(matches!(
            translate(ctx(modal, &b), ctrl(KeyCode::Char('c'))),
            Action::Quit
        ));
    }

    // ---- WORD motions / D/C/S / J / ;/, ----

    #[test]
    fn capital_w_invokes_big_word_forward() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('W')));
        assert_eq!(invocation_command(&action), Some(b.big_word_forward.0));
    }

    #[test]
    fn capital_b_invokes_big_word_backward() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('B')));
        assert_eq!(invocation_command(&action), Some(b.big_word_backward.0));
    }

    #[test]
    fn capital_e_invokes_big_word_end() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('E')));
        assert_eq!(invocation_command(&action), Some(b.big_word_end.0));
    }

    #[test]
    fn capital_d_invokes_delete_to_line_end() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('D')));
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.line_end),
                    other => panic!("expected line_end target, got {other:?}"),
                }
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn capital_c_invokes_change_to_line_end() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('C')));
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.line_end),
                    other => panic!("expected line_end target, got {other:?}"),
                }
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn capital_s_invokes_change_current_line() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('S')));
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn capital_j_emits_join_with_space() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('J'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.join_lines_with_space),
            other => panic!("expected Invoke(join_lines_with_space), got {other:?}"),
        }
    }

    #[test]
    fn gj_after_g_emits_join_without_space() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('J')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.join_lines_bare),
            other => panic!("expected Invoke(join_lines_bare), got {other:?}"),
        }
    }

    #[test]
    fn semicolon_emits_find_repeat_no_reverse() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char(';'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.find_repeat_forward),
            other => panic!("expected Invoke(find_repeat_forward), got {other:?}"),
        }
    }

    #[test]
    fn comma_emits_find_repeat_reverse() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char(','))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.find_repeat_reverse),
            other => panic!("expected Invoke(find_repeat_reverse), got {other:?}"),
        }
    }

    #[test]
    fn d_capital_w_resolves_to_delete_big_word_forward() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
            key(KeyCode::Char('W')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::Motion(id, _)) => assert_eq!(id, b.big_word_forward),
                other => panic!("expected motion target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }

    // ---- Macros: q, @ ----

    #[test]
    fn q_in_normal_when_not_recording_absorbs_partial_chord() {
        // Slice 8.i.4.a: `q` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('q')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('q')
        ));
    }

    #[test]
    fn q_in_normal_while_recording_stops() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_recording(ModalState::Normal, &b),
                key(KeyCode::Char('q'))
            ),
            Action::StopMacroRecord
        ));
    }

    #[test]
    fn at_in_normal_absorbs_partial_chord() {
        // Slice 8.i.4.a: `@` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('@')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('@')
        ));
    }

    #[test]
    fn letter_after_q_starts_recording() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('q')], &b),
            key(KeyCode::Char('a')),
        ) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.start_macro_record);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(start_macro_record, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn letter_after_at_plays_macro() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('@')], &b),
            key(KeyCode::Char('q')),
        ) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.play_macro);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('q')));
            }
            other => panic!("expected Invoke(play_macro, Char('q')), got {other:?}"),
        }
    }

    #[test]
    fn at_at_plays_last_macro() {
        // Slice 8.i.3: dispatcher returns Invoke(play_macro,
        // Char('@')); ActionSpec maps `@` to AppEffect::PlayLastMacro.
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('@')], &b),
            key(KeyCode::Char('@')),
        ) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.play_macro);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('@')));
            }
            other => panic!("expected Invoke(play_macro, Char('@')), got {other:?}"),
        }
    }

    #[test]
    fn esc_after_macro_pending_clears() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('q')], &b),
                key(KeyCode::Esc)
            ),
            Action::None
        ));
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('@')], &b),
                key(KeyCode::Esc)
            ),
            Action::None
        ));
    }

    // ---- Folds: zf zo zc za zR zM zd ----

    #[test]
    fn zf_after_z_emits_create_fold_from_visual() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('f')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.create_fold_from_visual),
            other => panic!("expected Invoke(create_fold_from_visual), got {other:?}"),
        }
    }

    #[test]
    fn zo_after_z_emits_open_fold() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('o')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.open_fold_at_cursor),
            other => panic!("expected Invoke(open_fold_at_cursor), got {other:?}"),
        }
    }

    #[test]
    fn zc_after_z_emits_close_fold() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('c')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.close_fold_at_cursor),
            other => panic!("expected Invoke(close_fold_at_cursor), got {other:?}"),
        }
    }

    #[test]
    fn za_after_z_emits_toggle_fold() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('a')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.toggle_fold_at_cursor),
            other => panic!("expected Invoke(toggle_fold_at_cursor), got {other:?}"),
        }
    }

    #[test]
    fn capital_z_r_after_z_opens_all() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('R')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.open_all_folds),
            other => panic!("expected Invoke(open_all_folds), got {other:?}"),
        }
    }

    #[test]
    fn capital_z_m_after_z_closes_all() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('M')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.close_all_folds),
            other => panic!("expected Invoke(close_all_folds), got {other:?}"),
        }
    }

    #[test]
    fn zd_after_z_deletes_fold() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('d')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.delete_fold_at_cursor),
            other => panic!("expected Invoke(delete_fold_at_cursor), got {other:?}"),
        }
    }

    // ---- Blockwise visual ----

    #[test]
    fn ctrl_v_enters_blockwise_visual() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('v'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_visual_blockwise),
            other => panic!("expected Invoke(enter_visual_blockwise), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_q_is_alternate_blockwise_visual() {
        // Many terminals (Konsole, Windows Terminal, tmux paste-key)
        // intercept Ctrl+V for clipboard paste before it reaches us.
        // Vim binds Ctrl+Q as the alternate enter-block-visual key for
        // exactly this reason.
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('q'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_visual_blockwise),
            other => panic!("expected Invoke(enter_visual_blockwise), got {other:?}"),
        }
    }

    #[test]
    fn lowercase_q_without_ctrl_still_absorbs_macro_record_prefix() {
        // Slice 8.i.4.a: `q` migrated to partial_chord. Guard
        // against the Ctrl+Q binding accidentally swallowing
        // the bare `q` that starts macro recording.
        let (_, b) = fixture();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('q'))) {
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('q') => {}
            other => panic!("expected AbsorbPartialChord(q), got {other:?}"),
        }
    }

    // ---- Help buffer (DESIGN.md §5.11, §5.9) ----
    //
    // Help is a regular buffer routed through `translate_normal` via
    // `App::active_buffer`. Only three buffer-local bindings differ
    // from the document path: `Esc` / `q` dismiss, `<CR>` follows
    // the link under the cursor. Everything else (motions, page
    // motions, `<C-o>` / `<C-i>`, `gg` / `G`) flows through the same
    // chord grammar -- the apply layer decides which cursor moves.

    fn ctx_help_active<'a>(modal: ModalState, b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Help,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    /// Slice 8.i.4.a: help-active variant of `ctx_partial`.
    fn ctx_help_active_partial<'a>(
        modal: ModalState,
        partial: &'a [crate::chord::KeyChord],
        b: &'a Builtins,
    ) -> TranslateContext<'a> {
        TranslateContext {
            modal,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Help,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            keymap: test_keymap(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: partial,
            active_minor_modes: &[],
        }
    }

    #[test]
    fn help_active_does_not_intercept_q_to_dismiss() {
        let (_, b) = fixture();
        // Reverted contract: `q` no longer auto-dismisses help /
        // log buffers. They should behave like other buffers --
        // only Esc and `:bd` close them. Pressing `q` while in a
        // log buffer would otherwise destroy the user's view
        // unexpectedly. With the early-return for `q` removed,
        // `q` falls through to its normal Normal-mode meaning
        // (macro-record start). Macros in a help buffer are a
        // no-op since the buffer is read-only; harmless.
        assert!(!matches!(
            translate(
                ctx_help_active(ModalState::Normal, &b),
                key(KeyCode::Char('q'))
            ),
            Action::HelpDismiss
        ));
    }

    #[test]
    fn help_active_intercepts_esc_to_dismiss() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx_help_active(ModalState::Normal, &b), key(KeyCode::Esc)),
            Action::HelpDismiss
        ));
    }

    #[test]
    fn help_active_routes_enter_to_follow_link() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx_help_active(ModalState::Normal, &b), key(KeyCode::Enter)),
            Action::FollowLink
        ));
    }

    #[test]
    fn help_active_routes_jk_through_normal_motions() {
        // `j` in help is the *same* line_down motion as in Normal --
        // active_buffer routing in the apply layer redirects which
        // cursor moves; the chord grammar is unchanged.
        let (_, b) = fixture();
        let action = translate(
            ctx_help_active(ModalState::Normal, &b),
            key(KeyCode::Char('j')),
        );
        assert_eq!(invocation_command(&action), Some(b.line_down.0));
    }

    #[test]
    fn help_active_routes_gg_through_chord_grammar() {
        // First `g` absorbs into partial_chord (same as Normal);
        // second resolves to goto_first_line. The buffer-local
        // handler must NOT collapse a bare `g` into `gg` -- that
        // was the bug fc872ec papered over with a help-specific
        // chord engine. Slice 8.i.4.a: the AfterG path is now
        // partial_chord-driven.
        let (_, b) = fixture();
        let first = translate(
            ctx_help_active(ModalState::Normal, &b),
            key(KeyCode::Char('g')),
        );
        assert!(matches!(
            first,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('g')
        ));
        let second = translate(
            ctx_help_active_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('g')),
        );
        assert_eq!(invocation_command(&second), Some(b.goto_first_line.0));
    }

    #[test]
    fn help_active_routes_capital_g_to_goto_last_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx_help_active(ModalState::Normal, &b),
            key(KeyCode::Char('G')),
        );
        assert_eq!(invocation_command(&action), Some(b.goto_last_line.0));
    }

    #[test]
    fn help_active_routes_ctrl_o_to_jump_history_back() {
        // `<C-o>` and `<C-i>` walk the unified position history --
        // crossing the document <-> help boundary is what
        // active_buffer routing makes possible.
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_help_active(ModalState::Normal, &b),
            ctrl(KeyCode::Char('o')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_history_back),
            other => panic!("expected Invoke(jump_history_back), got {other:?}"),
        }
    }

    // ---- Pane navigation (DESIGN.md §5.9, B.1.b) ----

    #[test]
    fn ctrl_w_absorbs_partial_chord() {
        // Slice 8.i.4.a: `<C-w>` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('w')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::ctrl('w')
        ));
    }

    #[test]
    fn ctrl_w_l_navigates_right() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::ctrl('w')], &b),
            key(KeyCode::Char('l')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_right),
            other => panic!("expected Invoke(navigate_pane_right), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_ctrl_l_also_navigates_right() {
        // Vim accepts the "Ctrl held throughout" form (`<C-w><C-l>`)
        // as well as the "release then press" form (`<C-w>l`).
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::ctrl('w')], &b),
            ctrl(KeyCode::Char('l')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_right),
            other => panic!("expected Invoke(navigate_pane_right), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_ctrl_j_navigates_down() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::ctrl('w')], &b),
            ctrl(KeyCode::Char('j')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_down),
            other => panic!("expected Invoke(navigate_pane_down), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_w_cycles_to_next_pane() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::ctrl('w')], &b),
            key(KeyCode::Char('w')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.next_pane),
            other => panic!("expected Invoke(next_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_capital_w_cycles_to_prev_pane() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::ctrl('w')], &b),
            key(KeyCode::Char('W')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.prev_pane),
            other => panic!("expected Invoke(prev_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_c_still_quits_when_help_is_active() {
        // The universal escape hatch sits above the help intercept.
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_help_active(ModalState::Normal, &b),
                ctrl(KeyCode::Char('c'))
            ),
            Action::Quit
        ));
    }

    // ---- Keymap drift detection (DESIGN.md §5.2.3, §5.11) ----

    /// Parse a chord-notation string from `keymap::default_keymap()` into
    /// a sequence of `KeyEvent`s. Recognises:
    /// - bare chars: `j` / `dw` / `gg`
    /// - special keys: `<Esc>`, `<CR>`, `<Tab>`, `<BS>`,
    ///   `<Up>`/`<Down>`/`<Left>`/`<Right>`, `<Home>`/`<End>`,
    ///   `<PageUp>`/`<PageDown>`
    /// - control chords: `<C-d>`, `<C-v>`, `<C-r>`, ...
    fn parse_chord_for_test(chord: &str) -> Vec<KeyEvent> {
        // `<` and `>` are valid bare chords (indent-left / indent-right
        // operators). Treat a single-char chord as a literal character
        // so the escape parser doesn't try to interpret `<` as the
        // start of a `<Special>` token.
        if chord.chars().count() == 1 {
            let c = chord.chars().next().expect("len == 1");
            return vec![KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)];
        }
        let mut out = Vec::new();
        let mut chars = chord.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '<' {
                out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                continue;
            }
            let mut body = String::new();
            for n in chars.by_ref() {
                if n == '>' {
                    break;
                }
                body.push(n);
            }
            let evt = match body.as_str() {
                "Esc" => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                "CR" | "Enter" => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                "Tab" => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                "BS" => KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                "Up" => KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                "Down" => KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                "Left" => KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
                "Right" => KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                "Home" => KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
                "End" => KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
                "PageUp" => KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
                "PageDown" => KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                other => {
                    if let Some(rest) = other.strip_prefix("S-") {
                        // Shift-modified specials: `<S-Tab>` is the
                        // primary user; crossterm reports it as
                        // `BackTab` (no SHIFT modifier on the event).
                        let evt = match rest {
                            "Tab" => KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
                            _ => match rest.chars().next() {
                                Some(c) => KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT),
                                None => continue,
                            },
                        };
                        out.push(evt);
                        continue;
                    }
                    if let Some(rest) = other.strip_prefix("C-") {
                        // Recognise `Space` as a token before falling
                        // back to the single-char path so `<C-Space>`
                        // parses to `Char(' ') + CONTROL`. Same shape
                        // crossterm reports.

                        match rest {
                            "Space" => KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
                            // Issue #29 slice 3 (2026-05-22): Ctrl-modified
                            // PageUp / PageDown for tab navigation.
                            "PageUp" => KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL),
                            "PageDown" => KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL),
                            _ => match rest.chars().next() {
                                Some(c) => KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
                                None => continue,
                            },
                        }
                    } else {
                        // Unrecognised special-key notation -- skip
                        // rather than panic; the drift test will fail
                        // with a clearer message about the descriptor.
                        continue;
                    }
                }
            };
            out.push(evt);
        }
        out
    }

    /// Walk a chord through `translate()` from the descriptor's
    /// starting mode, updating pending state across multi-key
    /// sequences. Returns the final Action.
    fn simulate_chord(
        chord: &str,
        mode: crate::keymap::BindingMode,
        builtins: &Builtins,
    ) -> Action {
        use crate::keymap::BindingMode;
        let modal = match mode {
            BindingMode::Visual => ModalState::Visual(lattice_grammar::VisualKind::Charwise),
            // CompletionPopup minor mode rides on top of Insert.
            BindingMode::Insert | BindingMode::CompletionPopup | BindingMode::AfterCtrlX => {
                ModalState::Insert
            }
            BindingMode::Replace => ModalState::Replace,
            BindingMode::Command => ModalState::Command,
            BindingMode::Search => ModalState::Search(lattice_grammar::SearchDirection::Forward),
            // After-* modes are pending substates of Normal: their
            // chords include the prefix (`gg`, `gU`, `zz`, ...) so we
            // start the walk from Normal pending=None and let
            // translate() set the pending state mid-sequence.
            _ => ModalState::Normal,
        };
        let active_buffer = if matches!(mode, BindingMode::Help) {
            BufferKind::Help
        } else {
            BufferKind::Document
        };
        // After-* modes whose chord doesn't start with the prefix
        // (e.g. `AfterCtrlX` whose chord is `<C-x><C-o>` -- which
        // *does* start with `<C-x>`) need the prefix in the chord.
        // The completion-popup minor mode is signalled host-side
        // by `App.insert_completion.is_some()`; in this harness we
        // toggle the equivalent context flag.
        let insert_completion_open = matches!(mode, BindingMode::CompletionPopup);
        let snippet_active = matches!(mode, BindingMode::Snippet);
        // Snippet minor mode rides on Insert.
        let modal = if snippet_active {
            ModalState::Insert
        } else {
            modal
        };
        // Slice 8.f: the minor-mode overlays no longer ride on
        // the legacy `insert_completion_open` / `snippet_active`
        // flags; they're `KeymapLayer::MinorMode` layers pushed
        // onto the registry. Pick the scenario-matched shared
        // keymap so the descriptor's chord resolves through the
        // intended layer.
        let keymap_for_mode: &'static KeymapHandle = if insert_completion_open {
            shared_keymap_with_popup()
        } else if snippet_active {
            shared_keymap_with_snippet()
        } else {
            shared_keymap_base()
        };
        let mut partial_chord: Vec<crate::chord::KeyChord> = Vec::new();
        let mut last = Action::None;
        for event in parse_chord_for_test(chord) {
            let ctx = TranslateContext {
                modal,
                builtins,
                pending_count: 0,
                op_count: 0,
                recording_macro: false,
                active_buffer,
                completion_open: false,
                chord_capture: false,
                picker_open: false,
                insert_completion_open,
                snippet_active,
                keymap: keymap_for_mode,
                terminal_insert_active: false,
                terminal_esc_exits: false,
                terminal_app_cursor_keys: false,
                terminal_insert_exit_pending: false,
            terminal_visual_active: false,
                partial_chord: &partial_chord,
                active_minor_modes: &[],
            };
            last = translate(ctx, event);
            // Mirror `App::apply`'s partial_chord lifecycle
            // (slice 8.i.4): AbsorbPartialChord appends to the
            // chord stack; any other action resolves it (clear).
            match &last {
                Action::AbsorbPartialChord(c) => partial_chord.push(*c),
                _ => partial_chord.clear(),
            }
        }
        last
    }

    #[test]
    fn keymap_descriptors_dont_drift_from_translate() {
        // One of two complementary catalog drift checks (see also
        // `every_catalog_command_resolves_in_registry` below). This
        // one runs each descriptor's chord through `translate()` and
        // asserts the chord still resolves to a non-`None` Action --
        // i.e. the catalog's chord notation matches what the
        // dispatcher accepts. Catches:
        //   - removed bindings (descriptor still in table)
        //   - moved bindings (descriptor in wrong mode)
        //   - typo'd chord notation
        // The companion test catches the orthogonal failure: a
        // descriptor that names a command which doesn't exist in the
        // registry. Both stay in place until `default_keymap()`
        // becomes the trie's source-of-truth (post-1.0); at that
        // point the chord side becomes tautological and this test
        // retires.
        let (_, b) = fixture();
        for entry in crate::keymap::default_keymap() {
            let action = simulate_chord(entry.chord, entry.mode, &b);
            assert!(
                !matches!(action, Action::None),
                "keymap descriptor `{}` ({}) doc=`{}` produced Action::None -- \
                 binding may have been removed or moved",
                entry.chord,
                entry.mode.label(),
                entry.doc,
            );
        }
    }

    #[test]
    fn every_catalog_command_resolves_in_registry() {
        // Companion to `keymap_descriptors_dont_drift_from_translate`.
        // Every catalog entry that names a canonical command via
        // `command: Some(name)` must resolve to a real registry entry.
        // Catches the orthogonal drift the chord-side check misses:
        //   - descriptor names a command that was renamed at the
        //     registry side without updating the catalog,
        //   - descriptor names a command that doesn't exist at all
        //     (typo, copy-paste from a sibling entry),
        //   - a registry refactor dropped a command but the catalog
        //     still claims it.
        // Synthetic-action descriptors (`PushDigit`, `SetPending`,
        // mode-entry primitives, ...) carry `command: None` and are
        // skipped -- they don't have a registry-resolvable name.
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        let _ex = lattice_grammar::ex_commands::populate(&mut r);
        let _a = crate::actions::populate(&mut r, &b);
        for entry in crate::keymap::default_keymap() {
            let Some(name) = entry.command else {
                continue;
            };
            assert!(
                r.id_by_name(name).is_some(),
                "keymap descriptor `{}` ({}) names command `{}` -- \
                 not found in CommandRegistry. Possible rename or \
                 typo; catalog is out of sync with the registry.",
                entry.chord,
                entry.mode.label(),
                name,
            );
        }
    }

    // ---- Mark history (g; / g,) ----

    #[test]
    fn g_semicolon_after_g_walks_mark_history_back() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char(';')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.walk_mark_history_back),
            other => panic!("expected Invoke(walk_mark_history_back), got {other:?}"),
        }
    }

    #[test]
    fn g_comma_after_g_walks_mark_history_forward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char(',')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.walk_mark_history_forward),
            other => panic!("expected Invoke(walk_mark_history_forward), got {other:?}"),
        }
    }

    // ---- LSP navigation (gd / gD / gy / gI / gr) ----
    // MO.1: these bindings moved to LspMode::keymap() (MinorMode layer).
    // Without lsp-mode active the Builtin layer has no entry → Action::None.

    #[test]
    fn gd_without_lsp_mode_is_unresolved() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
                key(KeyCode::Char('d')),
            ),
            Action::None
        ));
    }

    #[test]
    fn g_d_without_lsp_mode_is_unresolved() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
                key(KeyCode::Char('D')),
            ),
            Action::None
        ));
    }

    #[test]
    fn gy_without_lsp_mode_is_unresolved() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
                key(KeyCode::Char('y')),
            ),
            Action::None
        ));
    }

    #[test]
    fn g_i_without_lsp_mode_is_unresolved() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
                key(KeyCode::Char('I')),
            ),
            Action::None
        ));
    }

    #[test]
    fn gr_without_lsp_mode_is_unresolved() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
                key(KeyCode::Char('r')),
            ),
            Action::None
        ));
    }

    // ---- Insert-mode completion (Phase 4.2.g.1) ----

    fn ctx_insert_completion<'a>(b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext {
            modal: ModalState::Insert,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: true,
            snippet_active: false,
            // Slice 8.f: the popup overlay rides as a
            // `KeymapLayer::MinorMode` layer pushed on the
            // shared base handle; the legacy
            // `insert_completion_open` flag stays for
            // back-compat but no longer affects dispatch.
            keymap: shared_keymap_with_popup(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    #[test]
    fn ctrl_space_in_insert_triggers_completion() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Insert, &b), ctrl(KeyCode::Char(' '))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_trigger),
            other => panic!("expected Invoke(completion_trigger), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_x_in_insert_absorbs_partial_chord() {
        // Slice 8.i.4.b: `<C-x>` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Insert, &b), ctrl(KeyCode::Char('x')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::ctrl('x')
        ));
    }

    #[test]
    fn ctrl_x_ctrl_o_no_longer_invokes() {
        // CSM.K1: `<C-x><C-o>` (vim omni-completion alias)
        // retired. `<C-Space>` is the sole popup trigger; the
        // chord is unbound now (the dispatcher returns
        // something other than `Invoke`).
        let (_, b) = fixture();
        let r = translate(
            ctx_partial(ModalState::Insert, &[crate::chord::KeyChord::ctrl('x')], &b),
            ctrl(KeyCode::Char('o')),
        );
        assert!(
            !matches!(r, Action::Invoke(_)),
            "<C-x><C-o> should no longer resolve to an Invoke; got {r:?}",
        );
    }

    #[test]
    fn ctrl_x_followed_by_unrecognised_clears_partial_chord() {
        // Slice 8.i.4: with `partial_chord = [<C-x>]` and an
        // unrecognised second key, dispatch_insert returns
        // `Action::None`. App::apply's
        // non-`AbsorbPartialChord(_)` rule clears partial_chord.
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Insert, &[crate::chord::KeyChord::ctrl('x')], &b,),
                ctrl(KeyCode::Char('z'))
            ),
            Action::None
        ));
    }

    #[test]
    fn popup_open_ctrl_n_navigates_next() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('n')));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_next),
            other => panic!("expected Invoke(completion_next), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_ctrl_p_navigates_prev() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('p')));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_prev),
            other => panic!("expected Invoke(completion_prev), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_tab_accepts() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), key(KeyCode::Tab));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_ctrl_y_accepts() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('y')));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_enter_accepts() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), key(KeyCode::Enter));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_ctrl_e_cancels_keeps_insert() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('e')));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_cancel),
            other => panic!("expected Invoke(completion_cancel), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_esc_cancels_and_exits_insert() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), key(KeyCode::Esc));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_cancel_and_exit_insert)
            }
            other => panic!("expected Invoke(completion_cancel_and_exit_insert), got {other:?}"),
        }
    }

    #[test]
    fn popup_open_ctrl_d_toggles_docs_only_inside_minor_mode() {
        let (_, b) = fixture();
        let a = shared_actions();
        // Inside the popup minor mode -- claim it.
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('d')));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_toggle_docs)
            }
            other => panic!("expected Invoke(completion_toggle_docs), got {other:?}"),
        }
        // OUTSIDE the minor mode (Normal mode) -- the popup
        // layer doesn't fire; falls through to Normal-mode
        // half-page-down. This verifies the layer's
        // confinement.
        let half_down = translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('d')));
        if let Action::Invoke(inv) = half_down {
            assert_ne!(inv.command, a.completion_toggle_docs);
        }
    }

    /// CSM.K2: inside the popup `<C-f>` is the path filter
    /// chord (was docs-scroll-down before; that moved to
    /// `PageDown`).
    #[test]
    fn popup_open_ctrl_f_filters_to_path() {
        use lattice_grammar::args::Args;
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('f')));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_filter_to_source);
                match inv.args {
                    Args::String(s) => {
                        assert_eq!(s, lattice_completion::insert::PATH_SOURCE_ID)
                    }
                    other => panic!("expected Args::String, got {other:?}"),
                }
            }
            other => {
                panic!("expected Invoke(completion_filter_to_source, \"gen:path\"), got {other:?}")
            }
        }
    }

    /// CSM.K2: inside the popup `<C-b>` is the buffer-words
    /// filter chord (was docs-scroll-up; moved to `PageUp`).
    #[test]
    fn popup_open_ctrl_b_filters_to_buffer_words() {
        use lattice_grammar::args::Args;
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char('b')));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_filter_to_source);
                match inv.args {
                    Args::String(s) => {
                        assert_eq!(s, lattice_completion::insert::BufferWordsSource::ID)
                    }
                    other => panic!("expected Args::String, got {other:?}"),
                }
            }
            other => panic!(
                "expected Invoke(completion_filter_to_source, \"gen:buffer-words\"), got {other:?}"
            ),
        }
    }

    /// CSM.K2: inside the popup `<C-Space>` clears the active
    /// source filter (was re-trigger before; the re-trigger
    /// binding lives one layer down on base Insert).
    #[test]
    fn popup_open_ctrl_space_clears_filter() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_insert_completion(&b), ctrl(KeyCode::Char(' ')));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_filter_clear),
            other => panic!("expected Invoke(completion_filter_clear), got {other:?}"),
        }
    }

    // ---- Active-snippet minor mode (Phase 4.2.g.4) ----

    fn ctx_snippet_active<'a>(b: &'a Builtins) -> TranslateContext<'a> {
        TranslateContext {
            modal: ModalState::Insert,
            builtins: b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Document,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: true,
            // Slice 8.f: snippet overlay rides as a
            // `KeymapLayer::MinorMode` layer pushed on the
            // shared base handle.
            keymap: shared_keymap_with_snippet(),
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            partial_chord: &[],
            active_minor_modes: &[],
        }
    }

    #[test]
    fn snippet_active_tab_jumps_to_next_placeholder() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_snippet_active(&b), key(KeyCode::Tab));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_next_placeholder)
            }
            other => panic!("expected Invoke(snippet_next_placeholder), got {other:?}"),
        }
    }

    #[test]
    fn snippet_active_back_tab_jumps_to_prev_placeholder() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_snippet_active(&b), key(KeyCode::BackTab));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_prev_placeholder)
            }
            other => panic!("expected Invoke(snippet_prev_placeholder), got {other:?}"),
        }
    }

    #[test]
    fn snippet_active_esc_leaves_snippet() {
        let (_, b) = fixture();
        let a = shared_actions();
        let r = translate(ctx_snippet_active(&b), key(KeyCode::Esc));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.snippet_leave),
            other => panic!("expected Invoke(snippet_leave), got {other:?}"),
        }
    }

    #[test]
    fn snippet_active_other_keys_fall_through_to_insert() {
        let (_, b) = fixture();
        // A regular printable char inside a placeholder should
        // still hit the Insert-mode handler so the user can
        // overtype the default.
        let action = translate(ctx_snippet_active(&b), key(KeyCode::Char('x')));
        assert!(matches!(action, Action::Insert(s) if s == "x"));
    }

    #[test]
    fn snippet_active_yields_to_completion_popup() {
        // When both layers are active the popup wins for
        // `<Tab>` (popup uses Tab to accept, snippet uses Tab
        // to step). Otherwise navigating snippet placeholders
        // through the popup would be impossible. Slice 8.f:
        // the gating used to live in `translate`; now the
        // layer-stack push order in
        // `App::sync_keymap_overlays` (snippet first, popup
        // second) ensures popup wins. The shared
        // `with_both_overlays` keymap mirrors that order.
        let (_, b) = fixture();
        let a = shared_actions();
        let mut ctx = ctx_snippet_active(&b);
        ctx.insert_completion_open = true;
        ctx.keymap = shared_keymap_with_both_overlays();
        let action = translate(ctx, key(KeyCode::Tab));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn snippet_active_only_in_insert_mode() {
        // Active-snippet layer must never claim keys outside
        // Insert mode; otherwise a stuck snippet could swallow
        // Normal-mode `<Tab>` (which is `<C-i>` -- jump-list
        // forward).
        let (_, b) = fixture();
        let a = shared_actions();
        let mut ctx = ctx_snippet_active(&b);
        ctx.modal = ModalState::Normal;
        let action = translate(ctx, key(KeyCode::Tab));
        // Normal-mode `<Tab>` is `<C-i>` -- jump history forward,
        // not the snippet placeholder action.
        if let Action::Invoke(inv) = action {
            assert_ne!(inv.command, a.snippet_next_placeholder);
        }
    }

    #[test]
    fn ctrl_x_ctrl_s_resolves_to_snippet_expand() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Insert, &[crate::chord::KeyChord::ctrl('x')], &b),
            ctrl(KeyCode::Char('s')),
        ) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.snippet_expand),
            other => panic!("expected Invoke(snippet_expand), got {other:?}"),
        }
    }

    // ---- Position history ----

    #[test]
    fn ctrl_o_emits_jump_history_back() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('o'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_history_back),
            other => panic!("expected Invoke(jump_history_back), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_i_emits_jump_history_forward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('i'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_history_forward),
            other => panic!("expected Invoke(jump_history_forward), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_l_emits_redraw_screen() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('l'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.redraw_screen),
            other => panic!("expected Invoke(redraw_screen), got {other:?}"),
        }
    }

    #[test]
    fn tab_in_normal_emits_jump_history_forward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Tab)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_history_forward),
            other => panic!("expected Invoke(jump_history_forward), got {other:?}"),
        }
    }

    // ---- Register prefix ----

    #[test]
    fn quote_in_normal_absorbs_partial_chord() {
        // Slice 8.i.4.a: `"` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('"')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('"')
        ));
    }

    #[test]
    fn lowercase_letter_after_quote_selects_named_register() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('"')], &b),
            key(KeyCode::Char('a')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(select_register, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn digit_after_quote_selects_numbered_register() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('"')], &b),
            key(KeyCode::Char('0')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('0')));
            }
            other => panic!("expected Invoke(select_register, Char('0')), got {other:?}"),
        }
    }

    #[test]
    fn underscore_after_quote_selects_black_hole() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('"')], &b),
            key(KeyCode::Char('_')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('_')));
            }
            other => panic!("expected Invoke(select_register, Char('_')), got {other:?}"),
        }
    }

    #[test]
    fn plus_after_quote_selects_system() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('"')], &b),
            key(KeyCode::Char('+')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('+')));
            }
            other => panic!("expected Invoke(select_register, Char('+')), got {other:?}"),
        }
    }

    #[test]
    fn invalid_char_after_quote_passes_to_actionspec() {
        // Slice 8.i.3: validation lives in the bound `ActionSpec`,
        // which calls `Register::from_input_char(c)`. The dispatcher
        // returns `Invoke(select_register, Char(c))` regardless;
        // the spec returns `Effect::None` when the char doesn't
        // name a register.
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('"')], &b),
            key(KeyCode::Char('@')),
        ) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('@')));
            }
            other => panic!("expected Invoke(select_register, Char('@')), got {other:?}"),
        }
    }

    // ---- ~ toggle case at cursor ----

    #[test]
    fn tilde_emits_toggle_case_at_cursor() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('~'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.toggle_case_at_cursor),
            other => panic!("expected Invoke(toggle_case_at_cursor), got {other:?}"),
        }
    }

    // ---- Word-search and matching-bracket ----

    #[test]
    fn star_emits_search_word_forward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('*'))) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.search_word_under_cursor_forward)
            }
            other => panic!("expected Invoke(search_word_under_cursor_forward), got {other:?}"),
        }
    }

    #[test]
    fn hash_emits_search_word_backward() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('#'))) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.search_word_under_cursor_backward)
            }
            other => panic!("expected Invoke(search_word_under_cursor_backward), got {other:?}"),
        }
    }

    #[test]
    fn percent_emits_match_bracket() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('%'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.match_bracket),
            other => panic!("expected Invoke(match_bracket), got {other:?}"),
        }
    }

    // ---- Viewport motions: H, M, L, z*, Ctrl-F/B/Y/E ----

    #[test]
    fn capital_h_emits_jump_viewport_top() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('H')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_viewport_top),
            other => panic!("expected Invoke(jump_viewport_top), got {other:?}"),
        }
    }

    #[test]
    fn capital_m_emits_jump_viewport_middle() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('M')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_viewport_middle),
            other => panic!("expected Invoke(jump_viewport_middle), got {other:?}"),
        }
    }

    #[test]
    fn capital_l_emits_jump_viewport_bottom() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('L')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.jump_viewport_bottom),
            other => panic!("expected Invoke(jump_viewport_bottom), got {other:?}"),
        }
    }

    #[test]
    fn z_absorbs_partial_chord() {
        // Slice 8.i.4.a: `z` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('z')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('z')
        ));
    }

    #[test]
    fn zz_emits_scroll_cursor_center() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('z')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_center),
            other => panic!("expected Invoke(scroll_cursor_to_center), got {other:?}"),
        }
    }

    #[test]
    fn zt_emits_scroll_cursor_top() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('t')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_top),
            other => panic!("expected Invoke(scroll_cursor_to_top), got {other:?}"),
        }
    }

    #[test]
    fn zb_emits_scroll_cursor_bottom() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
            key(KeyCode::Char('b')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_bottom),
            other => panic!("expected Invoke(scroll_cursor_to_bottom), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_f_emits_page_down() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('f'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.page_down),
            other => panic!("expected Invoke(page_down), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_b_emits_page_up() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('b'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.page_up),
            other => panic!("expected Invoke(page_up), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_e_emits_scroll_line_down() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('e'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_line_down),
            other => panic!("expected Invoke(scroll_line_down), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_y_emits_scroll_line_up() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), ctrl(KeyCode::Char('y'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_line_up),
            other => panic!("expected Invoke(scroll_line_up), got {other:?}"),
        }
    }

    #[test]
    fn esc_after_z_pending_clears() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('z')], &b),
                key(KeyCode::Esc)
            ),
            Action::None
        ));
    }

    // ---- Replace mode ----

    #[test]
    fn capital_r_enters_replace_mode() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('R'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_mode_replace),
            other => panic!("expected Invoke(enter_mode_replace), got {other:?}"),
        }
    }

    #[test]
    fn char_in_replace_emits_overwrite() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Replace, &b), key(KeyCode::Char('z'))) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.overwrite_char);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('z')));
            }
            other => panic!("expected Invoke(overwrite_char, Char('z')), got {other:?}"),
        }
    }

    #[test]
    fn esc_in_replace_returns_to_normal() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Replace, &b), key(KeyCode::Esc)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_mode_normal),
            other => panic!("expected Invoke(enter_mode_normal), got {other:?}"),
        }
    }

    #[test]
    fn backspace_in_replace_emits_replace_undo_last() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Replace, &b), key(KeyCode::Backspace)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.replace_undo_last),
            other => panic!("expected Invoke(replace_undo_last), got {other:?}"),
        }
    }

    #[test]
    fn enter_in_replace_inserts_newline() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Replace, &b), key(KeyCode::Enter)) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.insert_newline),
            other => panic!("expected Invoke(insert_newline), got {other:?}"),
        }
    }

    /// Slice 8.d wiring: an empty `KeymapHandle` (no Replace
    /// catalog registered) routes every Replace key event to
    /// `Action::None`. The `ctx_*` builders use `test_keymap()`
    /// (populated); this test pins that the dispatcher genuinely
    /// reads from the handle by overriding it with an empty one.
    #[test]
    fn replace_dispatch_reads_from_handle_not_baked_in() {
        let (_, b) = fixture();
        let empty = KeymapHandle::new();
        let mut c = ctx(ModalState::Replace, &b);
        c.keymap = &empty;
        match translate(c, key(KeyCode::Char('z'))) {
            Action::None => {}
            other => panic!("empty handle must yield None for Replace dispatch, got {other:?}"),
        }
    }

    /// Slice 8.d also tightens the Replace mode's "modifier
    /// transparency" semantic at trie level: `<C-x>` is the only
    /// hard guard; `<M-x>` falls through to OverwriteChar('x') just
    /// like the legacy `translate_replace` did. Pinned end-to-end
    /// through the `translate` boundary so a future refactor can't
    /// regress it without tripping this test.
    #[test]
    fn alt_x_in_replace_overwrites_with_x() {
        let (_, b) = fixture();
        let a = shared_actions();
        let mut event = key(KeyCode::Char('x'));
        event.modifiers = KeyModifiers::ALT;
        match translate(ctx(ModalState::Replace, &b), event) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.overwrite_char);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('x')));
            }
            other => panic!("expected Invoke(overwrite_char, Char('x')), got {other:?}"),
        }
    }

    // ---- Marks ----

    #[test]
    fn m_in_normal_absorbs_partial_chord() {
        // Slice 8.i.4.a: `m` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('m')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('m')
        ));
    }

    #[test]
    fn apostrophe_in_normal_absorbs_partial_chord() {
        // Slice 8.i.4.a: `'` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('\'')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('\'')
        ));
    }

    #[test]
    fn backtick_in_normal_absorbs_partial_chord() {
        // Slice 8.i.4.a: `` ` `` migrated to partial_chord.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('`')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('`')
        ));
    }

    #[test]
    fn ma_after_m_emits_set_mark() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('m')], &b),
            key(KeyCode::Char('a')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.set_mark);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(set_mark, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn jump_mark_line_routes_correctly() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[crate::chord::KeyChord::char('\'')],
                &b,
            ),
            key(KeyCode::Char('z')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.jump_to_mark_line);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('z')));
            }
            other => panic!("expected Invoke(jump_to_mark_line, Char('z')), got {other:?}"),
        }
    }

    #[test]
    fn jump_mark_exact_routes_correctly() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('`')], &b),
            key(KeyCode::Char('A')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.jump_to_mark_exact);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('A')));
            }
            other => panic!("expected Invoke(jump_to_mark_exact, Char('A')), got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_set_mark_pending() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('m')], &b),
                key(KeyCode::Esc)
            ),
            Action::None
        ));
    }

    #[test]
    fn non_alpha_after_set_mark_passes_char_to_actionspec() {
        // Slice 8.i.3: dispatcher returns Invoke(set_mark) with
        // the captured char regardless of validity; the bound
        // ActionSpec returns Effect::None for non-alphanumeric
        // chars, and App::apply clears the pending state on
        // every Invoke.
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('m')], &b),
            key(KeyCode::Char(' ')),
        ) {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.set_mark);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char(' ')));
            }
            other => panic!("expected Invoke(set_mark, Char(' ')), got {other:?}"),
        }
    }

    // ---- gv reselect ----

    #[test]
    fn gv_after_g_emits_reselect_visual() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('v')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.reselect_last_visual),
            other => panic!("expected Invoke(reselect_last_visual), got {other:?}"),
        }
    }

    // ---- Indent and case operators ----

    #[test]
    fn gt_invokes_absorb_operator_indent_right() {
        // Slice 8.i.4.c: `>` -> Invoke(absorb_operator_indent_right).
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('>')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_indent_right),
            other => panic!("expected Invoke(absorb_operator_indent_right), got {other:?}"),
        }
    }

    #[test]
    fn lt_invokes_absorb_operator_indent_left() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('<')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_indent_left),
            other => panic!("expected Invoke(absorb_operator_indent_left), got {other:?}"),
        }
    }

    #[test]
    fn double_gt_resolves_to_indent_right_current_line() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('>')], &b),
            key(KeyCode::Char('>')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.indent_right.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn gu_after_g_invokes_absorb_operator_lower() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('u')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_lower),
            other => panic!("expected Invoke(absorb_operator_lower), got {other:?}"),
        }
    }

    #[test]
    fn capital_g_then_capital_u_invokes_absorb_operator_upper() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('U')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_upper),
            other => panic!("expected Invoke(absorb_operator_upper), got {other:?}"),
        }
    }

    #[test]
    fn g_tilde_after_g_invokes_absorb_operator_toggle_case() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('g')], &b),
            key(KeyCode::Char('~')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_toggle_case),
            other => panic!("expected Invoke(absorb_operator_toggle_case), got {other:?}"),
        }
    }

    #[test]
    fn guu_resolves_to_lower_current_line() {
        let (_, b) = fixture();
        // After `gu`, pending = AfterOperator(lower). Pressing `u` doubles.
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('g'),
                    crate::chord::KeyChord::char('u'),
                ],
                &b,
            ),
            key(KeyCode::Char('u')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.lower.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::CurrentLine));
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn g_capital_u_w_resolves_to_upper_word_forward() {
        let (_, b) = fixture();
        // After `gU`, pending = AfterOperator(upper). Pressing `w` is the motion.
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('g'),
                    crate::chord::KeyChord::char('U'),
                ],
                &b,
            ),
            key(KeyCode::Char('w')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.upper.0);
                match inv.target {
                    Some(Target::Motion(id, _)) => assert_eq!(id, b.word_forward),
                    other => panic!("expected motion target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    // ---- Text object chord routing ----

    #[test]
    fn i_in_operator_pending_absorbs_partial_chord() {
        // Slice 8.i.4.c: pressing `i` after `d` returns
        // `AbsorbPartialChord(i)`. The trie returns `Partial`
        // for `[d, i]` because `[d, i, w]` etc. are bound; the
        // next key resolves the full text-object path.
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
            key(KeyCode::Char('i')),
        );
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('i')
        ));
    }

    #[test]
    fn a_in_operator_pending_absorbs_partial_chord() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
            key(KeyCode::Char('a')),
        );
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('a')
        ));
    }

    #[test]
    fn diw_resolves_to_delete_inner_word() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('d'),
                    crate::chord::KeyChord::char('i'),
                ],
                &b,
            ),
            key(KeyCode::Char('w')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::TextObject(id, _)) => assert_eq!(id, b.inner_word),
                    other => panic!("expected text-object target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn da_quote_resolves_to_delete_around_double_quote() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('d'),
                    crate::chord::KeyChord::char('a'),
                ],
                &b,
            ),
            key(KeyCode::Char('"')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::TextObject(id, _)) => assert_eq!(id, b.around_quote_double),
                other => panic!("expected text-object target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn ci_paren_resolves_to_change_inner_paren() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('c'),
                    crate::chord::KeyChord::char('i'),
                ],
                &b,
            ),
            key(KeyCode::Char('(')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                match inv.target {
                    Some(Target::TextObject(id, _)) => assert_eq!(id, b.inner_paren),
                    other => panic!("expected text-object target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn diW_resolves_to_delete_inner_big_word() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('d'),
                    crate::chord::KeyChord::char('i'),
                ],
                &b,
            ),
            key(KeyCode::Char('W')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::TextObject(id, _)) => assert_eq!(id, b.inner_big_word),
                    other => panic!("expected text-object target, got {other:?}"),
                }
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn daW_resolves_to_delete_around_big_word() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('d'),
                    crate::chord::KeyChord::char('a'),
                ],
                &b,
            ),
            key(KeyCode::Char('W')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::TextObject(id, _)) => assert_eq!(id, b.around_big_word),
                other => panic!("expected text-object target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn ci_angle_resolves_to_change_inner_angle() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('c'),
                    crate::chord::KeyChord::char('i'),
                ],
                &b,
            ),
            key(KeyCode::Char('<')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::TextObject(id, _)) => assert_eq!(id, b.inner_angle),
                other => panic!("expected text-object target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn da_angle_via_closer_resolves_to_delete_around_angle() {
        // Both `<` and `>` should resolve to the angle text object.
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('d'),
                    crate::chord::KeyChord::char('a'),
                ],
                &b,
            ),
            key(KeyCode::Char('>')),
        );
        match action {
            Action::Invoke(inv) => match inv.target {
                Some(Target::TextObject(id, _)) => assert_eq!(id, b.around_angle),
                other => panic!("expected text-object target, got {other:?}"),
            },
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn esc_after_text_object_partial_chord_clears() {
        // Slice 8.i.4.c: with `partial_chord = [d, i]`, pressing
        // <Esc> dispatches to an unbound `[d, i, Esc]` path.
        // Returns `SetPending(None)` from
        // `lookup_normal_with_prefix`, which `App::apply` turns
        // into a `partial_chord.clear()` (the
        // non-`AbsorbPartialChord(_)` clear-rule).
        let (_, b) = fixture();
        assert!(matches!(
            translate(
                ctx_partial(
                    ModalState::Normal,
                    &[
                        crate::chord::KeyChord::char('d'),
                        crate::chord::KeyChord::char('i'),
                    ],
                    &b,
                ),
                key(KeyCode::Esc),
            ),
            Action::None
        ));
    }

    // ---- Dot-repeat ----

    #[test]
    fn dot_in_normal_emits_repeat_last_change() {
        let (_, b) = fixture();
        let a = shared_actions();
        match translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('.'))) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.repeat_last_change),
            other => panic!("expected Invoke(repeat_last_change), got {other:?}"),
        }
    }

    // ---- Visual mode entry / exit ----

    #[test]
    fn v_in_normal_enters_charwise_visual() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('v')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_visual_charwise),
            other => panic!("expected Invoke(enter_visual_charwise), got {other:?}"),
        }
    }

    #[test]
    fn capital_v_in_normal_enters_linewise_visual() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('V')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_visual_linewise),
            other => panic!("expected Invoke(enter_visual_linewise), got {other:?}"),
        }
    }

    #[test]
    fn esc_in_visual_exits_to_normal() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx(ModalState::Visual(VisualKind::Charwise), &b),
            key(KeyCode::Esc),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.exit_visual),
            other => panic!("expected Invoke(exit_visual), got {other:?}"),
        }
    }

    #[test]
    fn v_in_visual_toggles_off() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(
            ctx(ModalState::Visual(VisualKind::Charwise), &b),
            key(KeyCode::Char('v')),
        );
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.exit_visual),
            other => panic!("expected Invoke(exit_visual), got {other:?}"),
        }
    }

    #[test]
    fn motion_in_visual_returns_invocation() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Visual(VisualKind::Charwise), &b),
            key(KeyCode::Char('w')),
        );
        assert_eq!(invocation_command(&action), Some(b.word_forward.0));
    }

    #[test]
    fn d_in_visual_invokes_delete_with_selection_range() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Visual(VisualKind::Charwise), &b),
            key(KeyCode::Char('d')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::Selection));
            }
            other => panic!("expected Invoke(delete, Selection), got {other:?}"),
        }
    }

    #[test]
    fn y_in_visual_invokes_yank_with_selection_range() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Visual(VisualKind::Charwise), &b),
            key(KeyCode::Char('y')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.yank.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::Selection));
            }
            other => panic!("expected Invoke(yank, Selection), got {other:?}"),
        }
    }

    #[test]
    fn c_in_visual_invokes_change_with_selection_range() {
        let (_, b) = fixture();
        let action = translate(
            ctx(ModalState::Visual(VisualKind::Charwise), &b),
            key(KeyCode::Char('c')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                assert_eq!(inv.range, Some(lattice_grammar::Range::Selection));
            }
            other => panic!("expected Invoke(change, Selection), got {other:?}"),
        }
    }

    #[test]
    fn gt_in_visual_invokes_indent_right_with_selection_range() {
        let (_, b) = fixture();
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            let action = translate(ctx(ModalState::Visual(kind), &b), key(KeyCode::Char('>')));
            match action {
                Action::Invoke(inv) => {
                    assert_eq!(inv.command, b.indent_right.0, "kind = {kind:?}");
                    assert_eq!(inv.range, Some(lattice_grammar::Range::Selection));
                }
                other => panic!(
                    "kind = {kind:?}, expected Invoke(indent_right, Selection), got {other:?}"
                ),
            }
        }
    }

    #[test]
    fn lt_in_visual_invokes_indent_left_with_selection_range() {
        let (_, b) = fixture();
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            let action = translate(ctx(ModalState::Visual(kind), &b), key(KeyCode::Char('<')));
            match action {
                Action::Invoke(inv) => {
                    assert_eq!(inv.command, b.indent_left.0, "kind = {kind:?}");
                    assert_eq!(inv.range, Some(lattice_grammar::Range::Selection));
                }
                other => panic!(
                    "kind = {kind:?}, expected Invoke(indent_left, Selection), got {other:?}"
                ),
            }
        }
    }

    // ---- Count prefix (1-9, 0 with count in progress) ----

    #[test]
    fn digit_1_to_9_emits_push_digit_in_normal_mode() {
        let (_, b) = fixture();
        for digit in 1u8..=9 {
            let c = char::from_digit(digit as u32, 10).unwrap();
            let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char(c)));
            assert!(matches!(action, Action::PushDigit(d) if d == digit));
        }
    }

    #[test]
    fn zero_with_no_count_invokes_line_start() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('0')));
        assert_eq!(invocation_command(&action), Some(b.line_start.0));
    }

    #[test]
    fn zero_with_count_in_progress_extends_count() {
        let (_, b) = fixture();
        // pending_count == 1 -> '0' becomes a digit, not line_start.
        let action = translate(
            ctx_with_count(ModalState::Normal, &b, 1),
            key(KeyCode::Char('0')),
        );
        assert!(matches!(action, Action::PushDigit(0)));
    }

    #[test]
    fn digit_after_count_extends_count() {
        let (_, b) = fixture();
        let action = translate(
            ctx_with_count(ModalState::Normal, &b, 12),
            key(KeyCode::Char('3')),
        );
        // Translate just emits the digit; App accumulates 12 -> 123.
        assert!(matches!(action, Action::PushDigit(3)));
    }

    #[test]
    fn motion_after_count_dispatches_motion() {
        let (_, b) = fixture();
        let action = translate(
            ctx_with_count(ModalState::Normal, &b, 3),
            key(KeyCode::Char('w')),
        );
        // Slice 8.g.iv: translate attaches the in-progress count
        // (`pending_count`) to the resolved invocation before
        // returning. App's dispatcher reads `inv.count` directly.
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.word_forward.0);
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(3)));
            }
            other => panic!("expected Invoke(word_forward, count=3), got {other:?}"),
        }
    }

    /// Slice 8.g.iv / 8.i.4.c end-to-end: `2d3w` walks
    /// operator-pending resolution. `op_count=2 * motion_count=3
    /// = 6` should be attached at translate time, with
    /// `Range::None` and the correct `Target::Motion(word_forward)`.
    /// Slice 8.i.4.c migrated the operator-pending state from
    /// `Pending::AfterOperator(_)` to `App::partial_chord`; this
    /// test now passes the operator's chord prefix via
    /// `ctx_partial_with_op_count`.
    #[test]
    fn op_count_times_motion_count_attaches_at_translate() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial_with_op_count(
                ModalState::Normal,
                &[crate::chord::KeyChord::char('d')],
                &b,
                3,
                2,
            ),
            key(KeyCode::Char('w')),
        );
        match action {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(
                    inv.target,
                    Some(Target::Motion(m, _)) if m == b.word_forward
                ));
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(6)));
            }
            other => panic!("expected Invoke(delete, word_forward, count=6), got {other:?}"),
        }
    }

    // ---- Find-char / till-char (f, F, t, T) ----

    #[test]
    fn f_absorbs_partial_chord() {
        // Slice 8.i.4.c: `f` is a Partial trie node (because
        // `[f, *]` is bound as a CharLiteral wildcard).
        // Pressing `f` returns `AbsorbPartialChord(f)` instead
        // of `SetPending(AfterFindChar { kind: Forward, ... })`.
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('f')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('f')
        ));
    }

    #[test]
    fn capital_f_absorbs_partial_chord() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('F')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('F')
        ));
    }

    #[test]
    fn t_absorbs_partial_chord() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('t')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('t')
        ));
    }

    #[test]
    fn capital_t_absorbs_partial_chord() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('T')));
        assert!(matches!(
            action,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('T')
        ));
    }

    #[test]
    fn f_then_char_resolves_to_motion_with_args_char() {
        // Slice 8.i.4.c: with `partial_chord = [f]`, pressing
        // `z` resolves `[f, z]` -> Invoke(find_char_forward,
        // args=Char('z')).
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('f')], &b),
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
        // Slice 8.i.4.c: end-to-end `dfx` walks
        // partial_chord absorption rather than
        // SetPending(AfterOperator) -> SetPending(AfterFindChar)
        // chains.
        let (_, b) = fixture();
        // First press: `d` -> Invoke(absorb_operator_delete);
        // App's apply_app_effect pushes [d] to partial_chord
        // and latches op_count.
        let after_d = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('d')));
        match after_d {
            Action::Invoke(_) => {}
            other => panic!("expected Invoke(absorb_operator_delete), got {other:?}"),
        };
        // Second press: `f` in partial_chord = [d] -> AbsorbPartialChord(f).
        let after_df = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
            key(KeyCode::Char('f')),
        );
        assert!(matches!(
            after_df,
            Action::AbsorbPartialChord(c) if c == crate::chord::KeyChord::char('f')
        ));
        // Third press: `x` in partial_chord = [d, f] -> Invoke
        // delete with find_char_forward target.
        let after_dfx = translate(
            ctx_partial(
                ModalState::Normal,
                &[
                    crate::chord::KeyChord::char('d'),
                    crate::chord::KeyChord::char('f'),
                ],
                &b,
            ),
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
    fn esc_after_find_partial_clears_partial_chord() {
        // Slice 8.i.4: with partial_chord = [f] and Esc as the
        // second key, `[f, Esc]` is unbound; lookup returns
        // `Action::None`. App::apply clears partial_chord via
        // the non-`AbsorbPartialChord(_)` rule.
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('f')], &b),
            key(KeyCode::Esc),
        );
        assert!(matches!(action, Action::None));
    }

    // ---- New motions: b, e, ^ ----

    #[test]
    fn b_invokes_word_backward() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('b')));
        assert_eq!(invocation_command(&action), Some(b.word_backward.0));
    }

    #[test]
    fn e_invokes_word_end() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('e')));
        assert_eq!(invocation_command(&action), Some(b.word_end.0));
    }

    #[test]
    fn caret_invokes_first_non_blank() {
        let (_, b) = fixture();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('^')));
        assert_eq!(invocation_command(&action), Some(b.first_non_blank.0));
    }

    #[test]
    fn db_resolves_to_delete_word_backward() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
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
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
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
    fn c_invokes_absorb_operator_change() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('c')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_change),
            other => panic!("expected Invoke(absorb_operator_change), got {other:?}"),
        }
    }

    #[test]
    fn cw_resolves_to_change_with_word_forward_target() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('c')], &b),
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
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('c')], &b),
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
    fn y_invokes_absorb_operator_yank() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('y')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_yank),
            other => panic!("expected Invoke(absorb_operator_yank), got {other:?}"),
        }
    }

    #[test]
    fn yw_resolves_to_yank_word_forward() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('y')], &b),
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
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('y')], &b),
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
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('Y')));
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
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('p')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.paste_after),
            other => panic!("expected Invoke(paste_after), got {other:?}"),
        }
    }

    #[test]
    fn p_uppercase_is_paste_before() {
        let (_, b) = fixture();
        let a = shared_actions();
        let action = translate(ctx(ModalState::Normal, &b), key(KeyCode::Char('P')));
        match action {
            Action::Invoke(inv) => assert_eq!(inv.command, a.paste_before),
            other => panic!("expected Invoke(paste_before), got {other:?}"),
        }
    }

    #[test]
    fn dd_is_not_treated_as_change_current_line() {
        // Regression check: the `cc` arm should only fire for op == change.
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
            key(KeyCode::Char('c')),
        );
        // Delete operator + 'c' key: no specific motion, fallback clears pending.
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn d_caret_resolves_to_delete_first_non_blank() {
        let (_, b) = fixture();
        let action = translate(
            ctx_partial(ModalState::Normal, &[crate::chord::KeyChord::char('d')], &b),
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

    // ---- Chord-capture (DESIGN.md §B.1, ArgKind::Chord) ----

    #[test]
    fn chord_capture_translates_ctrl_letter_to_chord_token() {
        let (_, b) = fixture();
        let action = translate(ctx_chord_capture(&b), ctrl(KeyCode::Char('c')));
        match action {
            Action::CommandLineAppendChord(s) => assert_eq!(s, "<C-c>"),
            other => panic!("expected CommandLineAppendChord, got {other:?}"),
        }
    }

    #[test]
    fn chord_capture_translates_plain_letter_unwrapped() {
        let (_, b) = fixture();
        let action = translate(ctx_chord_capture(&b), key(KeyCode::Char('g')));
        match action {
            Action::CommandLineAppendChord(s) => assert_eq!(s, "g"),
            other => panic!("expected CommandLineAppendChord, got {other:?}"),
        }
    }

    #[test]
    fn chord_capture_reserves_esc_for_cancel() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx_chord_capture(&b), key(KeyCode::Esc)),
            Action::CommandLineCancel
        ));
    }

    #[test]
    fn chord_capture_reserves_enter_for_submit() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx_chord_capture(&b), key(KeyCode::Enter)),
            Action::CommandLineSubmit
        ));
    }

    #[test]
    fn chord_capture_reserves_backspace_for_delete_chord() {
        let (_, b) = fixture();
        assert!(matches!(
            translate(ctx_chord_capture(&b), key(KeyCode::Backspace)),
            Action::CommandLineDeleteChord
        ));
    }

    #[test]
    fn chord_capture_translates_special_keys_with_angles() {
        let (_, b) = fixture();
        // Up arrow -- the canonical chord is `<Up>`, not Esc.
        let action = translate(ctx_chord_capture(&b), key(KeyCode::Up));
        match action {
            Action::CommandLineAppendChord(s) => assert_eq!(s, "<Up>"),
            other => panic!("expected CommandLineAppendChord, got {other:?}"),
        }
    }

    /// Terminal-mode T2.b: `a` / `I` / `A` in Normal-in-terminal
    /// all funnel into `EnterTerminalInsert`. The four vim insert-
    /// entry chords collapse to one action because the terminal
    /// grid has no before-cursor / after-cursor or BOL / EOL
    /// distinction — the shell owns the cursor.
    #[test]
    fn terminal_normal_a_upper_i_upper_a_also_enter_terminal_insert() {
        let (_, b) = fixture();
        for c in ['a', 'I', 'A'] {
            let ctx = TranslateContext {
                modal: ModalState::Normal,
                builtins: &b,
                pending_count: 0,
                op_count: 0,
                recording_macro: false,
                active_buffer: BufferKind::Terminal,
                completion_open: false,
                chord_capture: false,
                picker_open: false,
                insert_completion_open: false,
                snippet_active: false,
                terminal_insert_active: false,
                terminal_esc_exits: false,
                terminal_app_cursor_keys: false,
                terminal_insert_exit_pending: false,
            terminal_visual_active: false,
                keymap: test_keymap(),
                partial_chord: &[],
                active_minor_modes: &[],
            };
            let action = translate(ctx, key(KeyCode::Char(c)));
            assert!(
                matches!(action, Action::EnterTerminalInsert),
                "expected EnterTerminalInsert for `{c}`, got {action:?}",
            );
        }
    }

    /// Terminal-mode T2.a: when active_buffer is Terminal and
    /// `terminal-insert-mode` is OFF, pressing `i` enters
    /// Terminal-Insert (analogous to vim's `i` entering Insert).
    #[test]
    fn terminal_normal_i_enters_terminal_insert() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: false,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        assert!(matches!(
            translate(ctx, key(KeyCode::Char('i'))),
            Action::EnterTerminalInsert,
        ));
    }

    /// Terminal-mode T2.a: with `terminal-insert-mode` active,
    /// printable chars + control chars encode to ANSI bytes
    /// wrapped in `Action::TerminalInput`, bypassing the modal
    /// dispatchers entirely. This is what makes `ls<CR>` route
    /// to the shell instead of moving the cursor.
    #[test]
    fn terminal_insert_encodes_chars_to_terminal_input() {
        let (_, b) = fixture();
        let mk_ctx = || TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        match translate(mk_ctx(), key(KeyCode::Char('l'))) {
            Action::TerminalInput(bytes) => assert_eq!(bytes, b"l".to_vec()),
            other => panic!("expected TerminalInput, got {other:?}"),
        }
        match translate(mk_ctx(), key(KeyCode::Enter)) {
            Action::TerminalInput(bytes) => assert_eq!(bytes, vec![b'\r']),
            other => panic!("expected TerminalInput, got {other:?}"),
        }
    }

    /// Terminal-mode T2.a — critical override: `<C-c>` in
    /// Terminal-Insert sends SIGINT to the child (`\x03`); it
    /// must NOT trigger the universal `Action::Quit` hatch. A
    /// vim user reflex of `<C-c>` to interrupt a long shell
    /// command would otherwise close the editor.
    #[test]
    fn terminal_insert_ctrl_c_sends_sigint_byte_not_quit() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        match translate(ctx, ctrl(KeyCode::Char('c'))) {
            Action::TerminalInput(bytes) => assert_eq!(bytes, vec![0x03]),
            other => panic!("expected TerminalInput([0x03]), got {other:?}"),
        }
    }

    /// Terminal-mode T2.c — `<C-\>` in Terminal-Insert ARMS
    /// the two-key exit chord; the next key resolves it.
    /// Single `<C-\>` no longer exits on its own (vim parity).
    #[test]
    fn terminal_insert_ctrl_backslash_arms_exit_chord() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        assert!(matches!(
            translate(ctx, ctrl(KeyCode::Char('\\'))),
            Action::TerminalArmExitChord,
        ));
    }

    /// Terminal-mode T2.c — once `<C-\>` has armed the chord,
    /// pressing `<C-n>` confirms the exit.
    #[test]
    fn terminal_insert_armed_then_ctrl_n_exits() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: true,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        assert!(matches!(
            translate(ctx, ctrl(KeyCode::Char('n'))),
            Action::ExitTerminalInsert,
        ));
    }

    /// Terminal-mode T2.c — armed then ANY other key sends
    /// `\x1c` (lost `<C-\>`) plus the other key's PTY bytes.
    #[test]
    fn terminal_insert_armed_then_other_key_emits_lost_prefix_plus_byte() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: true,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        match translate(ctx, key(KeyCode::Char('a'))) {
            Action::TerminalInput(bytes) => assert_eq!(bytes, vec![0x1c, b'a']),
            other => panic!("expected TerminalInput([0x1c, 'a']), got {other:?}"),
        }
    }

    /// Terminal-mode T2.c — DECCKM flips arrow keys to SS3.
    #[test]
    fn terminal_insert_arrow_uses_ss3_when_decckm_set() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: true,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        match translate(ctx, key(KeyCode::Up)) {
            Action::TerminalInput(bytes) => assert_eq!(bytes, b"\x1bOA".to_vec()),
            other => panic!("expected SS3 ESC O A, got {other:?}"),
        }
    }

    /// Terminal-mode T2.b.0 — `<Esc>` exits Terminal-Insert when
    /// `terminal.esc-exits` is on (the default). Matches the
    /// table-stakes behaviour users expect from `:terminal` →
    /// `i` → type → `<Esc>` → `:q`.
    #[test]
    fn terminal_insert_esc_exits_when_option_set() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: true,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        assert!(matches!(
            translate(ctx, key(KeyCode::Esc)),
            Action::ExitTerminalInsert,
        ));
    }

    /// Terminal-mode T2.b.0 — `<Esc>` encodes to `\x1b` and goes
    /// to the PTY when `terminal.esc-exits` is off. Nested vim /
    /// htop / less inside the terminal keep their own Esc
    /// semantics; user exits via `<C-\>` (T2.a) or `<C-\><C-n>`
    /// (T2.c).
    #[test]
    fn terminal_insert_esc_encodes_when_option_unset() {
        let (_, b) = fixture();
        let ctx = TranslateContext {
            modal: ModalState::Normal,
            builtins: &b,
            pending_count: 0,
            op_count: 0,
            recording_macro: false,
            active_buffer: BufferKind::Terminal,
            completion_open: false,
            chord_capture: false,
            picker_open: false,
            insert_completion_open: false,
            snippet_active: false,
            terminal_insert_active: true,
            terminal_esc_exits: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_visual_active: false,
            keymap: test_keymap(),
            partial_chord: &[],
            active_minor_modes: &[],
        };
        match translate(ctx, key(KeyCode::Esc)) {
            Action::TerminalInput(bytes) => assert_eq!(bytes, vec![0x1b]),
            other => panic!("expected TerminalInput([0x1b]), got {other:?}"),
        }
    }
}
