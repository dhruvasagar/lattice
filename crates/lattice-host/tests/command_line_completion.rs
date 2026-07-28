//! `:` line completion at the host level — the shared handler both
//! renderer peers drive (`action:command-line-complete` →
//! `Editor::open_completion_popup`).
//!
//! Renderer-independent by construction: these assertions are about the
//! editor's `completion_state` / command-line text, not about how a peer
//! paints the popup.

use lattice_host::editor::Editor;

fn cmdline(line: &str) -> Editor {
    let mut e = Editor::boot(lattice_core::Document::from_text("x\n"));
    e.modal = lattice_grammar::ModalState::Command;
    e.set_command_line_text(line);
    e
}

fn candidate_texts(e: &Editor) -> Vec<String> {
    e.completion_state
        .as_ref()
        .map(|s| s.candidates.iter().map(|c| c.raw.text.clone()).collect())
        .unwrap_or_default()
}

/// `gen:modes` backs `:describe-mode`'s `name` arg: every registered mode
/// completes there, including the ones feature crates add at boot.
#[test]
fn describe_mode_arg_slot_lists_registered_modes() {
    let mut e = cmdline("describe-mode ");
    e.open_completion_popup();
    let texts = candidate_texts(&e);
    assert!(texts.contains(&"text-mode".to_string()), "{texts:?}");
    assert!(texts.contains(&"help-mode".to_string()), "{texts:?}");
}

/// Regression: `<Tab>` on an ALREADY-complete command name was a silent
/// no-op (single candidate == typed text ⇒ auto-insert rewrote the same
/// string and returned), so the arg slot — where the mode names live —
/// was unreachable by `<Tab>` alone. It must step right instead.
#[test]
fn tab_on_complete_command_name_steps_into_the_arg_slot() {
    let mut e = cmdline("describe-mode");
    e.open_completion_popup();
    assert_eq!(e.command_line(), "describe-mode ");
    let texts = candidate_texts(&e);
    assert!(texts.contains(&"text-mode".to_string()), "{texts:?}");
}

/// The step-right only fires when the first arg has a *registered*
/// completion source, so arg-less commands never grow a trailing space.
#[test]
fn tab_on_argless_command_leaves_the_line_alone() {
    let mut e = cmdline("list-modes");
    e.open_completion_popup();
    assert_eq!(e.command_line(), "list-modes");
}

/// An un-completable required arg (`:describe-key` takes a `Chord`, which
/// the submit path captures) is likewise left alone.
#[test]
fn tab_on_chord_arg_command_leaves_the_line_alone() {
    let mut e = cmdline("describe-key");
    e.open_completion_popup();
    assert_eq!(e.command_line(), "describe-key");
}

/// A genuine prefix expansion still expands (and then steps into the arg
/// slot, since `:describe-mode` has one).
#[test]
fn tab_still_expands_a_unique_prefix() {
    let mut e = cmdline("describe-mod");
    e.open_completion_popup();
    assert_eq!(e.command_line(), "describe-mode ");
}
