//! CG.1 — foreground cancellation (`<C-g>`), driven through real keys.
//!
//! Design: `docs/dev/architecture/cancellation.md`; sequencing:
//! `docs/dev/operations/slice-plans/cancellation.md`.
//!
//! The unit tests for `cancel_foreground` /
//! `reset_to_normal` live next to those methods in `lattice-host`, and
//! the binding itself is unit-tested in `lattice_host::keymap_cancel`.
//! This module covers the seam neither can reach: **press key →
//! `input::translate` → `App::apply` → `Editor::cancel_foreground`**.
//!
//! A binding that resolves to the right `Action` but never reaches the
//! handler — or a modal state with no dispatch arm to route it — looks
//! identical to a working one from the handler side. That has shipped
//! here before (`ModalState::Prompt` had no `translate` arm at all, so
//! every key in an open prompt was swallowed). Every test presses keys;
//! none call the cancel methods directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lattice_grammar::{ModalState, VisualKind};

use crate::app::test_helpers::*;

fn ctrl_g() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
}

/// CG.12: some foreground work for `<C-g>` to reach — a keyed task begun
/// through the editor's slot. Hold the ticket: a dropped ticket frees its
/// slot, and `<C-g>` would then have nothing to cancel.
fn running(a: &crate::App) -> lattice_mode::TaskTicket {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    a.editor
        .foreground_cancel
        .begin(lattice_mode::TaskKey::supersede("test", n.to_string()))
        .expect("a fresh key is let through")
}

fn esc() -> KeyEvent {
    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
}

/// The armed token must be flipped by a real keypress. This is the whole
/// point of the slice — CG.2/3/4 all hang off `active_cancel` being
/// reachable from the keyboard.
#[test]
fn ctrl_g_cancels_the_armed_foreground_token() {
    let mut a = app_with("hello world\nsecond line\n", 10);
    let task = running(&a);
    let token = task.token().clone();
    assert!(!token.is_cancelled(), "a freshly armed token is live");

    press(&mut a, ctrl_g());

    assert!(
        token.is_cancelled(),
        "<C-g> must flip the running task's token"
    );
    assert!(!task.is_current(), "and its result would land nowhere");
}

/// The binding is `Builtin`, not `emacs-keys-mode`, so it must not
/// depend on `:set emacs-keys`. A user who turns the tribute off still
/// has a cancel key.
#[test]
fn ctrl_g_cancels_with_emacs_keys_disabled() {
    let mut a = app_with("hello world\n", 10);
    submit_ex(&mut a, "set noemacs-keys");
    let task = running(&a);
    let token = task.token().clone();

    press(&mut a, ctrl_g());

    assert!(
        token.is_cancelled(),
        "cancel is a Builtin binding — `:set noemacs-keys` must not \
         take away the only way to stop a running search"
    );
}

/// With nothing armed the press degrades to a mode reset, so it is safe
/// to hit speculatively.
#[test]
fn ctrl_g_when_idle_is_harmless() {
    let mut a = app_with("hello world\n", 10);
    let before = a.editor.cursor.byte;

    press(&mut a, ctrl_g());

    assert!(matches!(a.editor.modal, ModalState::Normal));
    assert_eq!(
        a.editor.cursor.byte, before,
        "an idle <C-g> in Normal must not move the cursor — `enter_mode` \
         pulls back one byte on entering Normal, so `reset_to_normal` \
         has to skip it when already Normal"
    );
}

/// `<Esc>` deliberately does NOT cancel.
///
/// An earlier revision of this slice folded cancellation into Esc. It
/// was reverted because vim users press Esc reflexively and constantly,
/// so a long-running search would die to a habitual double-tap carrying
/// no intent to cancel. This pins that Esc stayed inert — the whole
/// reason cancel got its own chord.
#[test]
fn esc_does_not_cancel() {
    let mut a = app_with("hello world\n", 10);
    let task = running(&a);
    let token = task.token().clone();

    press(&mut a, esc());
    press(&mut a, esc());

    assert!(
        !token.is_cancelled(),
        "reflexive <Esc> must never kill in-flight foreground work"
    );
    assert!(task.is_current(), "and must leave it running");
}

/// `<C-g>` returns to Normal from Insert — the `keyboard-quit` contract
/// is flip *and* reset, unlike a bare token flip.
#[test]
fn ctrl_g_returns_to_normal_from_insert() {
    let mut a = app_with("hello world\n", 10);
    let task = running(&a);
    let token = task.token().clone();
    press_chars(&mut a, "i");
    assert!(matches!(a.editor.modal, ModalState::Insert));

    press(&mut a, ctrl_g());

    assert!(matches!(a.editor.modal, ModalState::Normal));
    assert!(token.is_cancelled());
}

/// A half-typed count is exactly the kind of stuck state cancel clears.
#[test]
fn ctrl_g_clears_a_pending_count() {
    let mut a = app_with("a\nb\nc\nd\ne\nf\ng\nh\n", 10);
    press_chars(&mut a, "12");
    assert_eq!(a.editor.pending_count, 12, "count accumulated");

    press(&mut a, ctrl_g());

    assert_eq!(a.editor.pending_count, 0, "<C-g> must clear the count");
}

/// Mid-chord (`d` then `<C-g>`) the trie sees an unbound continuation,
/// so the press aborts the operator without also cancelling. That is
/// vim's rule for an invalid continuation, and it leaves the user in
/// Normal where a second press does cancel.
///
/// Pinned rather than left implicit: it is a real two-press edge, and
/// the reasoning for not closing it (translate would have to know which
/// `CommandId` is cancel) is recorded in `input::compute_normal_action`.
/// A future slice that closes it should flip this test, not delete it.
#[test]
fn ctrl_g_in_operator_pending_aborts_the_operator_first() {
    let mut a = app_with("hello world\nsecond line\n", 10);
    let task = running(&a);
    let token = task.token().clone();
    press_chars(&mut a, "d");
    assert!(!a.editor.partial_chord.is_empty(), "`d` is pending");

    press(&mut a, ctrl_g());
    assert!(
        a.editor.partial_chord.is_empty(),
        "<C-g> must abort the pending operator"
    );
    assert!(
        !token.is_cancelled(),
        "documented gap: the first press only aborts the chord"
    );

    press(&mut a, ctrl_g());
    assert!(
        token.is_cancelled(),
        "the user is never stuck — just slower"
    );
}

/// CG.2: the Editor's slot and the registered service must be the SAME
/// `Arc`.
///
/// This is the test that matters most in CG.2, because the failure is
/// silent: if boot registered a different `ForegroundCancel` than the
/// one the `Editor` holds, every crate still compiles, providers still
/// arm, `<C-g>` still runs — and nothing is ever cancelled. It is the
/// `ServiceRegistry` Arc/`TypeId` trap in its most expensive form.
///
/// So this arms the way a provider does — through
/// `services.get::<ForegroundCancelHandle>()`, exactly as project
/// search's spawn sites do — and then cancels the way the user does, by
/// pressing the key.
#[test]
fn a_token_armed_through_the_service_is_cancelled_by_the_key() {
    let mut a = app_with("hello world\n", 10);

    let fc = a
        .editor
        .services
        .get::<lattice_mode::ForegroundCancelHandle>()
        .expect("boot must register ForegroundCancelHandle");
    let task = fc
        .begin(lattice_mode::TaskKey::supersede("search", "view 1"))
        .expect("let through");
    let token = task.token().clone();
    assert!(!token.is_cancelled());

    press(&mut a, ctrl_g());

    assert!(
        token.is_cancelled(),
        "a provider arming through the service registry must land in the \
         very slot <C-g> cancels — not a sibling copy of it"
    );
}

/// CG.12: `<C-g>` stops the plugin call running when it is pressed — the
/// interrupt token a guest call snapshots, reached through the same service
/// slot the plugin host reads.
#[test]
fn ctrl_g_interrupts_the_running_plugin_call() {
    let mut a = app_with("hello world\n", 10);
    let fc = a
        .editor
        .services
        .get::<lattice_mode::ForegroundCancelHandle>()
        .expect("boot must register ForegroundCancelHandle");
    let plugin_call = fc.current_token().expect("always present");

    press(&mut a, ctrl_g());

    assert!(plugin_call.is_cancelled());
    assert!(
        !fc.current_token().unwrap().is_cancelled(),
        "a call started after the press is not reached by it"
    );
}

/// SN.3d owns `<C-g>` in Visual. Binding cancel there would have
/// silently deleted the only selection-preserving path into Select.
#[test]
fn ctrl_g_still_toggles_visual_to_select() {
    let mut a = app_with("hello world\nsecond line\n", 10);
    let task = running(&a);
    let token = task.token().clone();
    press_chars(&mut a, "v");

    press(&mut a, ctrl_g());

    assert!(
        matches!(a.editor.modal, ModalState::Select(VisualKind::Charwise)),
        "<C-g> must remain the Visual→Select toggle, not cancel"
    );
    assert!(
        !token.is_cancelled(),
        "and must not cancel as a side effect"
    );
}

/// The accepted cost of leaving Visual/Select to the toggle: cancelling
/// from a selection takes `<Esc>` then `<C-g>`. Pinned so the escape
/// route is known to work rather than assumed.
#[test]
fn cancelling_from_visual_takes_esc_then_ctrl_g() {
    let mut a = app_with("hello world\nsecond line\n", 10);
    let task = running(&a);
    let token = task.token().clone();
    press_chars(&mut a, "v");

    press(&mut a, esc());
    press(&mut a, ctrl_g());

    assert!(matches!(a.editor.modal, ModalState::Normal));
    assert!(token.is_cancelled());
}

/// The regression that moved this slice off `<C-c>`.
///
/// `<C-c>` is a mode *prefix*, and `KeymapTrie::lookup` returns `Bound`
/// at a terminal node regardless of its children — so a depth-1 `<C-c>`
/// binding made `<C-c>g` unreachable and broke magit's dispatch
/// transient. A mode owning `<C-c>` terminally is fine (those layers are
/// K.1.c-scoped); `Builtin` is not, because it is global.
#[test]
fn builtin_never_binds_ctrl_c_terminally() {
    use lattice_host::keymap::BindingMode;
    use lattice_host::keymap_trie::{KeymapLayer, LookupResult};

    let a = app_with("hello\n", 10);
    for mode in [
        BindingMode::Normal,
        BindingMode::Insert,
        BindingMode::Visual,
        BindingMode::Select,
        BindingMode::Replace,
    ] {
        if let LookupResult::Bound { command, .. } = a
            .editor
            .keymap
            .lookup(mode, &[lattice_host::chord::KeyChord::ctrl('c')])
        {
            assert_ne!(
                command.layer,
                KeymapLayer::Builtin,
                "<C-c> must never resolve terminally at Builtin in \
                 {mode:?} — it is a prefix (<C-c>g, <C-c><C-c>, …) and a \
                 global terminal binding shadows every one of those \
                 chords in every buffer"
            );
        }
    }
}

/// The prefix that actually broke. Proves the chord survives, not just
/// that the trie node is shaped right.
#[test]
fn ctrl_c_g_still_reaches_the_magit_dispatch_prefix() {
    use lattice_host::keymap::BindingMode;
    use lattice_host::keymap_trie::LookupResult;

    let a = app_with("hello\n", 10);
    assert!(
        matches!(
            a.editor.keymap.lookup(
                BindingMode::Normal,
                &[lattice_host::chord::KeyChord::ctrl('c')]
            ),
            LookupResult::Partial | LookupResult::Bound { .. }
        ),
        "<C-c> must still open a path to its children"
    );
    assert!(
        matches!(
            a.editor.keymap.lookup(
                BindingMode::Normal,
                &[
                    lattice_host::chord::KeyChord::ctrl('c'),
                    lattice_host::chord::KeyChord::char('g'),
                ]
            ),
            LookupResult::Bound { .. }
        ),
        "<C-c>g (magit dispatch) must stay reachable"
    );
}

/// A stuck `:` line is one of the states cancel exists for, and the
/// minibuffer states own real buffers — `reset_to_normal` runs the
/// command line's own teardown, which a bare mode-flip would not.
#[test]
fn ctrl_g_dismisses_the_command_line() {
    let mut a = app_with("hello\n", 10);
    let before = a.editor.document_buffer_id;
    let task = running(&a);
    let token = task.token().clone();
    a.apply(crate::app::Action::EnterCommandLine);
    press_chars(&mut a, "e foo");
    assert!(a.editor.command_line_active());

    press(&mut a, ctrl_g());

    assert!(
        !a.editor.command_line_active(),
        "<C-g> must close the `:` line"
    );
    assert_eq!(
        a.editor.document_buffer_id, before,
        "the prior editing buffer must be restored"
    );
    assert!(token.is_cancelled());
}
