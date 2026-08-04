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
        "action:magit-global-file-delete",
        "action:magit-global-file-delete-execute",
    ),
    (
        "action:magit-global-file-checkout",
        "action:magit-global-file-checkout-execute",
    ),
    (
        "action:magit-branch-delete",
        "action:magit-branch-delete-execute",
    ),
    // MG.32: the branch submenu's `x`. A SEPARATE pair from the buffer
    // chord above, and it has to be: one `CommandId` maps to one
    // handler, and that one reads the branch under the cursor —
    // correct for a chord in the branch list, and unreachable from a
    // menu opened anywhere else. This half takes its target from the
    // `:magit-branch-delete <name>` the picker routes through.
    (
        "magit-branch-delete",
        "action:magit-global-branch-delete-execute",
    ),
    ("action:magit-stash-drop", "action:magit-stash-drop-execute"),
    (
        "action:magit-rebase-abort",
        "action:magit-rebase-abort-execute",
    ),
    // MG.21i: removing a submodule deletes its whole working tree,
    // including anything uncommitted inside it, and git keeps no copy.
    (
        "action:magit-submodule-remove",
        "action:magit-submodule-remove-execute",
    ),
    // MG.20: `reset --hard` discards uncommitted work irrecoverably —
    // the same bar `x` / branch-delete / stash-drop are held to.
    // `--soft` and `--mixed` keep your changes, so they act directly.
    ("action:magit-reset-hard", "action:magit-reset-hard-execute"),
];

/// Build the two-step confirm for a destructive action.
///
/// `prompt` must name the target; `yes_action` must be the `execute`
/// half of a [`DESTRUCTIVE_ACTIONS`] row — asserted in debug builds so
/// a new destructive action that skips the table (and therefore skips
/// the registration guard below) fails loudly for the author rather
/// than quietly for the user.
pub(crate) fn ask(prompt: String, yes_action: &str) -> Effect {
    ask_with(prompt, yes_action, lattice_grammar::Args::None)
}

/// IX.2: the common case — one string target, carried in slot 0.
///
/// Every execute half migrated to this reads it back with
/// [`carried_target`], falling back to re-derivation when absent, so a
/// path that carries nothing still works.
pub(crate) fn ask_target(prompt: String, yes_action: &str, target: impl Into<String>) -> Effect {
    ask_with(
        prompt,
        yes_action,
        lattice_grammar::Args::List(vec![lattice_grammar::ArgValue::String(target.into())]),
    )
}

/// The target an [`ask_target`] confirm carried, if it did.
///
/// `None` means the confirm was raised by a path that carries nothing,
/// and the caller re-derives — the pre-IX.1 behaviour, kept so
/// migration is per-action rather than all-or-nothing.
pub(crate) fn carried_target(ctx: &lattice_mode::ActionContext<'_>) -> Option<String> {
    ctx.arg_str(0).map(str::to_string)
}

/// IX.1: the two-step with the execute half's **target carried along**
/// instead of re-derived when it fires.
///
/// Prefer this wherever the target can be named. A yes-half that
/// re-derives reads context that is not stable across the wait — a
/// background refresh can rebuild the buffer and move the cursor while
/// the dialog is open, so the action lands somewhere the prompt did not
/// name. Carrying closes that window by construction.
///
/// Carry the payload, not a pointer to it: a path, a SHA, a synthesized
/// patch — never a cursor row or a row span, which a rebuild
/// invalidates.
pub(crate) fn ask_with(prompt: String, yes_action: &str, args: lattice_grammar::Args) -> Effect {
    debug_assert!(
        DESTRUCTIVE_ACTIONS.iter().any(|(_, e)| *e == yes_action),
        "`{yes_action}` is not listed in confirm::DESTRUCTIVE_ACTIONS — \
         add it there so its registration is covered"
    );
    Effect::Confirm {
        prompt,
        yes_action: yes_action.to_string(),
        args,
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
    /// MG.32: ex-commands are registered too, because an ask half is
    /// not always an action. The branch submenu's `x` asks through
    /// `:magit-branch-delete <name>` — forced, not chosen: a picker's
    /// accept can only reach an operation via `InvokeCommand`, which
    /// dispatches ex-commands. Registering only actions here would make
    /// this guard reject a legitimate row.
    #[test]
    fn every_confirm_pair_resolves_in_the_command_registry() {
        let mut registry = CommandRegistry::new();
        crate::register_action_commands(&mut registry);
        crate::register_ex_commands(&mut registry, Default::default());
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
            Effect::Confirm {
                prompt,
                yes_action,
                args,
            } => {
                assert_eq!(prompt, "Delete branch feature/foo?");
                assert_eq!(yes_action, "action:magit-branch-delete-execute");
                assert!(
                    matches!(args, lattice_grammar::Args::None),
                    "`ask` carries nothing — the yes-half re-derives, which is \
                     the pre-IX.1 behaviour `ask_with` exists to replace"
                );
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    /// IX.1: `ask_with` carries the target, so the execute half acts on
    /// what the prompt named rather than re-deriving it.
    #[test]
    fn ask_with_carries_its_target_to_the_yes_action() {
        let effect = ask_with(
            "Discard changes to src/main.rs?".to_string(),
            "action:magit-global-file-discard-execute",
            lattice_grammar::Args::List(vec![lattice_grammar::ArgValue::String(
                "src/main.rs".to_string(),
            )]),
        );
        match effect {
            Effect::Confirm { args, .. } => {
                let list = args.as_list().expect("a carried target");
                assert!(matches!(
                    &list[0],
                    lattice_grammar::ArgValue::String(p) if p == "src/main.rs"
                ));
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
