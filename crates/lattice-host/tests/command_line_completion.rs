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

/// DAM.1: on the command-name slot a literal prefix beats a fuzzy
/// subsequence.
///
/// Candidate matching is fuzzy, so `describe-mode` also matches
/// `describe-active-modes`. Two candidates would skip the
/// single-candidate branch and silently break the `<Tab>` behaviour
/// the two tests above pin. This is a general hazard, not a
/// one-off: any future command whose name fuzzily contains an
/// existing one re-triggers it.
#[test]
fn exact_command_name_wins_over_a_fuzzy_sibling() {
    let mut e = cmdline("describe-mode");
    e.open_completion_popup();
    // `describe-active-modes` fuzzy-matches the typed text...
    assert_eq!(
        e.command_line(),
        "describe-mode ",
        "an exactly-typed command name must still step into its arg slot",
    );
    // ...and the arg slot we landed in is the mode-name one.
    let texts = candidate_texts(&e);
    assert!(texts.contains(&"text-mode".to_string()), "{texts:?}");
}

/// The same rule at a partial prefix: `describe-mod` literally
/// prefixes exactly one command, so it expands even though it also
/// fuzzy-matches `describe-active-modes`.
#[test]
fn unique_literal_prefix_wins_over_a_fuzzy_sibling() {
    let mut e = cmdline("describe-mod");
    e.open_completion_popup();
    assert_eq!(e.command_line(), "describe-mode ");
}

/// The rule requires a *literal prefix*, not merely a unique fuzzy
/// match. `dscrbmode` is a subsequence of `describe-mode` and of
/// nothing else, but it prefixes no command — so it must open the
/// popup for the user to choose, not silently rewrite the line.
///
/// This is the half of the rule that keeps it conservative: fuzzy
/// matching still needs confirmation; only prefixes auto-commit.
#[test]
fn a_fuzzy_only_match_does_not_auto_insert() {
    let mut e = cmdline("dscrbmode");
    e.open_completion_popup();
    assert_eq!(
        e.command_line(),
        "dscrbmode",
        "a non-prefix fuzzy match must not rewrite the line",
    );
    let texts = candidate_texts(&e);
    assert!(
        texts.contains(&"describe-mode".to_string()),
        "the fuzzy match should still be offered in the popup: {texts:?}",
    );
}

/// The preference is scoped to the command-name slot: an arg slot
/// with several matches still opens a popup rather than collapsing
/// to one candidate.
#[test]
fn arg_slot_completion_still_lists_multiple_matches() {
    let mut e = cmdline("describe-mode ma");
    e.open_completion_popup();
    let texts = candidate_texts(&e);
    assert!(
        texts.len() > 1,
        "arg slot should still offer multiple candidates: {texts:?}",
    );
}

/// `:describe-active-modes` is reachable by name and carries no args
/// (so `<Tab>` never grows a trailing space, the `:list-modes`
/// contract).
#[test]
fn describe_active_modes_is_argless() {
    let mut e = cmdline("describe-active-modes");
    e.open_completion_popup();
    assert_eq!(e.command_line(), "describe-active-modes");
}
