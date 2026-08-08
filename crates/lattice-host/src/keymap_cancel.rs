//! CG.1 — the foreground-cancel binding (`<C-g>`).
//!
//! Design: `docs/dev/architecture/cancellation.md`; sequencing:
//! `docs/dev/operations/slice-plans/cancellation.md`.
//!
//! ## The chord
//!
//! `<C-g>` — emacs `keyboard-quit`. Registered at
//! `KeymapLayer::Builtin`, so it works unconditionally rather than
//! depending on `:set emacs-keys`.
//!
//! It is the only apt chord that is actually free. `<C-c>` — vim's own
//! interrupt, and the obvious first choice — cannot be used: it is a
//! mode *prefix* here (`<C-c>g` / `<C-c>f` magit dispatch, `<C-c><C-c>`
//! / `<C-c><C-k>` commit-rebase-notes), and `KeymapTrie::lookup` returns
//! `Bound` at a terminal node regardless of its children, so a depth-1
//! binding makes every one of those chords unreachable. Of the remaining
//! free CTRL chords (`a c g j k m x z`): `c` and `x` are prefixes,
//! `j` / `m` are the literal LF / CR terminals send for Enter, `z` is
//! the suspend convention, `a` is vim's increment and `k` is Insert's
//! kill-to-end-of-line.
//!
//! ## Modes covered
//!
//! `Normal`, `Insert`, `Replace` — and therefore `ModalState::Command` /
//! `Search(_)` / `Prompt` too, since those dispatch through
//! `keymap_insert::dispatch_insert`, which looks up
//! `BindingMode::Insert`.
//!
//! **Not `Visual` or `Select`.** SN.3d owns `<C-g>` there as the
//! Visual↔Select toggle — vim-canonical, and the only path between the
//! two modes that preserves the selection (`select-mode.md` §4). Those
//! arms are hardcoded ahead of the trie lookup in `dispatch_visual` /
//! `native_select_action`, so they would win anyway; leaving the modes
//! out of this set keeps the intent explicit rather than accidental.
//!
//! The cost is that Visual and Select have no cancel chord: from there
//! it is `<Esc>` then `<C-g>`. Accepted deliberately — Visual is a
//! transient state a user is rarely parked in while waiting on a scan,
//! and the alternative was relocating a vim-canonical chord.
//!
//! ## Why not `<Esc>`
//!
//! An earlier revision of this slice folded cancellation into `<Esc>`,
//! which is universal and never a prefix. It was reverted: vim users
//! press `<Esc>` reflexively and constantly, so a long-running search
//! would die to a habitual double-tap that carried no intent to cancel.
//! Cancellation needs a key the user only presses on purpose.

use lattice_grammar::{CommandInvocation, SourceLocation};

use crate::actions::ActionIds;
use crate::chord::KeyChord;
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{ChordPattern, KeymapLayer};

/// Every mode `<C-g>` resolves to `action:cancel` in. See the module
/// docs for why `Visual` / `Select` are absent and why `Command` /
/// `Search` / `Prompt` need no entry of their own.
pub const CANCEL_MODES: &[BindingMode] = &[
    BindingMode::Normal,
    BindingMode::Insert,
    BindingMode::Replace,
];

/// Register `<C-g>` → `action:cancel` under `KeymapLayer::Builtin` for
/// every mode in [`CANCEL_MODES`].
pub fn register_cancel_bindings(handle: &KeymapHandle, actions: &ActionIds) {
    for mode in CANCEL_MODES {
        handle.bind(
            KeymapLayer::Builtin,
            *mode,
            &[ChordPattern::Literal(KeyChord::ctrl('g'))],
            CommandInvocation::of(actions.cancel),
            SourceLocation::builtin_file(file!(), cancel_line()),
        );
    }
}

/// Line reported by `:describe-key <C-g>`. A `const fn` for the same
/// reason `keymap_replace` uses them: `line!()` inside the `bind` call
/// would report the argument's line, which drifts on every reformat.
const fn cancel_line() -> u32 {
    line!()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::keymap_trie::LookupResult;

    fn shared_actions() -> &'static ActionIds {
        use std::sync::OnceLock;
        static A: OnceLock<ActionIds> = OnceLock::new();
        A.get_or_init(|| {
            let mut r = lattice_grammar::CommandRegistry::new();
            let b = lattice_grammar::builtins::populate(&mut r);
            let _ = lattice_grammar::ex_commands::populate(&mut r);
            crate::actions::populate(&mut r, &b)
        })
    }

    fn handle() -> KeymapHandle {
        let h = KeymapHandle::new();
        register_cancel_bindings(&h, shared_actions());
        h
    }

    #[test]
    fn ctrl_g_is_bound_in_every_cancel_mode() {
        let h = handle();
        let actions = shared_actions();
        for mode in CANCEL_MODES {
            match h.lookup(*mode, &[KeyChord::ctrl('g')]) {
                LookupResult::Bound { command, .. } => assert_eq!(
                    command.command.command, actions.cancel,
                    "<C-g> in {mode:?} must resolve to action:cancel"
                ),
                other => panic!("<C-g> unbound in {mode:?}: {other:?}"),
            }
        }
    }

    /// SN.3d owns `<C-g>` in Visual and Select. Their handlers are
    /// hardcoded ahead of the trie so a stray registration here would
    /// not actually break the toggle — which is exactly why it needs a
    /// test: the damage would be silent, surfacing only as a confusing
    /// `:describe-key` and as a trap for whoever later removes those
    /// hardcoded arms.
    #[test]
    fn visual_and_select_are_left_to_the_sn3d_toggle() {
        let h = handle();
        for mode in [BindingMode::Visual, BindingMode::Select] {
            assert!(
                !CANCEL_MODES.contains(&mode),
                "{mode:?} must stay out of the cancel set"
            );
            assert!(
                matches!(
                    h.lookup(mode, &[KeyChord::ctrl('g')]),
                    LookupResult::Unbound
                ),
                "<C-g> must stay unbound in {mode:?} (SN.3d's toggle)"
            );
        }
    }

    /// `<C-c>` is a mode prefix. A terminal binding at Builtin resolves
    /// before its own children and would make `<C-c>g` (magit dispatch)
    /// and friends unreachable — the regression that moved this slice
    /// off `<C-c>` in the first place.
    #[test]
    fn ctrl_c_is_never_claimed_here() {
        let h = handle();
        for mode in CANCEL_MODES {
            assert!(
                matches!(
                    h.lookup(*mode, &[KeyChord::ctrl('c')]),
                    LookupResult::Unbound
                ),
                "<C-c> must stay free in {mode:?} — it is a prefix"
            );
        }
    }
}
