//! `:<mode-name>` says what it did.
//!
//! Every registered mode gets an auto-generated toggle ex-command (M.5.1).
//! It has been silent on success since it landed — it echoed when the name was
//! unknown, and said nothing at all when it worked. So the one case where you
//! cannot see the result for yourself is the case that told you nothing: a
//! mode with no visible surface (a gate, a marker) left you re-running the
//! command to find out which way you had just flipped it, which flips it back.
//!
//! The echo has to report the state the buffer is actually in *afterwards*,
//! not the state the toggle intended. Activation can refuse — a missing
//! capability, an `ActivationPolicy` that declines — and an "enabled" printed
//! over a refusal is worse than the silence it replaced.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("hello\n"))
}

fn echo(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

#[test]
fn toggling_a_minor_on_says_it_is_enabled() {
    let mut editor = boot();
    let _ = editor.toggle_mode_by_name("wrap-mode");
    let msg = echo(&editor);
    assert!(
        msg.contains("wrap-mode") && msg.contains("enabled"),
        "expected an enabled echo naming the mode; got {msg:?}"
    );
}

#[test]
fn toggling_a_minor_off_says_it_is_disabled() {
    let mut editor = boot();
    let _ = editor.toggle_mode_by_name("wrap-mode");
    let _ = editor.toggle_mode_by_name("wrap-mode");
    let msg = echo(&editor);
    assert!(
        msg.contains("wrap-mode") && msg.contains("disabled"),
        "expected a disabled echo naming the mode; got {msg:?}"
    );
}

/// The pair is what makes the echo useful: two invocations must not produce
/// the same words, or the message answers "something happened" rather than
/// "you are now in this state".
#[test]
fn the_two_directions_do_not_read_the_same() {
    let mut editor = boot();
    let _ = editor.toggle_mode_by_name("wrap-mode");
    let on = echo(&editor);
    let _ = editor.toggle_mode_by_name("wrap-mode");
    let off = echo(&editor);
    assert_ne!(on, off, "the two directions must be distinguishable");
}

/// A major is not a toggle — re-invoking it reloads rather than turning it
/// off — so it must not claim to have been "disabled" on the second run.
#[test]
fn a_major_never_claims_to_have_been_disabled() {
    let mut editor = boot();
    let _ = editor.toggle_mode_by_name("text-mode");
    let first = echo(&editor);
    let _ = editor.toggle_mode_by_name("text-mode");
    let second = echo(&editor);
    for msg in [&first, &second] {
        assert!(
            !msg.contains("disabled"),
            "a major mode cannot be toggled off; got {msg:?}"
        );
    }
}

/// The unknown-name path already echoed, and must keep its own wording rather
/// than being overwritten by a state report about a mode that does not exist.
#[test]
fn an_unknown_mode_still_says_so() {
    let mut editor = boot();
    let _ = editor.toggle_mode_by_name("no-such-mode");
    let msg = echo(&editor);
    assert!(
        msg.contains("not a registered mode"),
        "expected the unknown-mode error; got {msg:?}"
    );
    assert!(
        !msg.contains("enabled") && !msg.contains("disabled"),
        "must not report a state for a mode that does not exist; got {msg:?}"
    );
}

/// A refused activation must not be reported as success. `ActivationPolicy::
/// Manual` modes still activate on request, so the refusal this guards is the
/// general one: if the buffer is not in the mode afterwards, the echo must not
/// say it is.
#[test]
fn the_echo_reports_the_state_that_actually_resulted() {
    let mut editor = boot();
    let buffer = editor.document_buffer_id;
    let _ = editor.toggle_mode_by_name("wrap-mode");

    let active = editor
        .active_modes
        .get(&buffer)
        .map(|m| m.is_active(lattice_mode::ModeId::new("wrap-mode")))
        .unwrap_or(false);
    let msg = echo(&editor);

    assert_eq!(
        active,
        msg.contains("enabled"),
        "the echo and the buffer disagree: active={active}, echo={msg:?}"
    );
}
