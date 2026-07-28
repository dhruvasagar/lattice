//! MG.12 — destructive-action parity.
//!
//! magit mutates a repository from a lot of single keystrokes. Most
//! of those are reversible (stage, unstage, checkout, stash create),
//! and asking about them would be noise. A few are not: they throw
//! away work that git itself cannot hand back. Those all go through
//! one shape — the chord's handler does **no** git call at all and
//! returns `Effect::Confirm`; the git call lives in a separate
//! `-execute` action named as that confirm's `yes_action`. Answering
//! `n` therefore cannot mutate anything, because the only code that
//! mutates was never reached.
//!
//! Before this slice, magit-status's `x` (discard) asked, magit-branch's
//! `d` force-deleted immediately, and magit-stash's `d` dropped without
//! asking — three safety postures for one class of act. The
//! inconsistency was the bug, not any single binding.
//!
//! The prompt always names its target (`Delete branch feature/foo?`,
//! not `Delete branch?`) so the question is answerable without looking
//! away from it — the confirm transient covers the buffer the target
//! was read from.

use lattice_grammar::Effect;

/// Every ask → execute pair in magit.
///
/// `ask` is what a chord or transient item is bound to; `execute` is
/// the `yes_action` it hands to `Effect::Confirm`. Both names must be
/// registered as actions or `Editor::do_confirm` bails at
/// "confirm: unknown action `…`" — a failure that only shows up when a
/// user actually presses the key. The table exists so one test proves
/// registration for all of them at once, and so adding a destructive
/// action without wiring its confirm is visible in one place.
pub(crate) const DESTRUCTIVE_ACTIONS: &[(&str, &str)] = &[
    ("action:magit-discard", "action:magit-discard-execute"),
    (
        "action:magit-global-file-discard",
        "action:magit-global-file-discard-execute",
    ),
    (
        "action:magit-branch-delete",
        "action:magit-branch-delete-execute",
    ),
    ("action:magit-stash-drop", "action:magit-stash-drop-execute"),
    (
        "action:magit-rebase-abort",
        "action:magit-rebase-abort-execute",
    ),
];

/// Build the two-step confirm for a destructive action.
///
/// `prompt` must name the target; `yes_action` must be the `execute`
/// half of a [`DESTRUCTIVE_ACTIONS`] row — asserted in debug builds so
/// a new destructive action that skips the table (and therefore skips
/// the registration guard below) fails loudly for the author rather
/// than quietly for the user.
pub(crate) fn ask(prompt: String, yes_action: &str) -> Effect {
    debug_assert!(
        DESTRUCTIVE_ACTIONS.iter().any(|(_, e)| *e == yes_action),
        "`{yes_action}` is not listed in confirm::DESTRUCTIVE_ACTIONS — \
         add it there so its registration is covered"
    );
    Effect::Confirm {
        prompt,
        yes_action: yes_action.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_grammar::CommandRegistry;

    /// The failure this guards is silent from the code's side and
    /// loud from the user's: `do_confirm` resolves `yes_action`
    /// through the command registry, so an unregistered execute half
    /// turns the whole destructive action into an error message.
    #[test]
    fn every_confirm_pair_resolves_in_the_command_registry() {
        let mut registry = CommandRegistry::new();
        crate::register_action_commands(&mut registry);
        for (ask_name, execute_name) in DESTRUCTIVE_ACTIONS {
            assert!(
                registry.id_by_name(ask_name).is_some(),
                "destructive action `{ask_name}` is not registered"
            );
            assert!(
                registry.id_by_name(execute_name).is_some(),
                "`{ask_name}`'s yes-action `{execute_name}` is not registered — \
                 pressing the chord would fail at `confirm: unknown action`"
            );
        }
    }

    #[test]
    fn ask_builds_a_confirm_carrying_the_prompt_and_yes_action() {
        let effect = ask(
            "Delete branch feature/foo?".to_string(),
            "action:magit-branch-delete-execute",
        );
        match effect {
            Effect::Confirm { prompt, yes_action } => {
                assert_eq!(prompt, "Delete branch feature/foo?");
                assert_eq!(yes_action, "action:magit-branch-delete-execute");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    /// The debug assert in [`ask`] is the compile-time-adjacent half of
    /// the guard: a destructive action wired without a table row cannot
    /// reach a user in a debug build.
    #[test]
    #[should_panic(expected = "not listed in confirm::DESTRUCTIVE_ACTIONS")]
    fn ask_rejects_a_yes_action_missing_from_the_table() {
        let _ = ask("Do something?".to_string(), "action:not-in-the-table");
    }
}
