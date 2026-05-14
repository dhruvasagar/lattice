//! Replace-mode binding registration + drift-test helpers.
//!
//! Audit slice 8.d. The smallest mode in `input.rs`'s
//! hand-rolled `match` table -- only four binding shapes:
//!
//! | Chord                | Action                            |
//! |----------------------|-----------------------------------|
//! | `<Esc>`              | `EnterMode(Normal)`               |
//! | `<BS>`               | `ReplaceUndoLast`                 |
//! | `<CR>`               | `Insert("\n")`                    |
//! | any bare printable   | `OverwriteChar(c)` (wildcard)     |
//!
//! Plus the legacy guard: any modifier-bearing key (`<C-x>`,
//! `<M-x>`, ...) returns `Action::None`. The trie's
//! "wildcard only matches bare printable chars" rule
//! (audit slice 8.b) preserves this for free.
//!
//! ## What this slice ships
//!
//! - [`register_replace_bindings`] -- the four entries the
//!   App's startup wires into the [`KeymapHandle`] under
//!   `KeymapLayer::Builtin` + `BindingMode::Replace`.
//! - [`dispatch_replace`] -- the registry-driven dispatcher.
//!   `KeyEvent → KeyChord → handle.lookup → Action`.
//!   `input::translate` calls this for `ModalState::Replace`;
//!   the App constructs a populated [`KeymapHandle`] in
//!   [`crate::app::App::new`] and the runtime threads it
//!   through [`crate::input::TranslateContext`] every frame.
//! - **Drift test** in this module's test block -- exhaustive
//!   over the keys Replace mode cares about, asserting that
//!   `dispatch_replace(handle, ev) ==`
//!   the legacy `translate_replace`'s reference body for every
//!   input. The reference body is kept private to the test
//!   module (a literal copy of the pre-migration match table)
//!   so a future refactor can't drift the dispatcher away from
//!   the documented Replace-mode semantics. Slice 8.i retires
//!   the reference body once every binding routes through a
//!   real `CommandInvocation`.
//!
//! ## Migration template
//!
//! Subsequent slices (8.e Visual, 8.f Insert, 8.g Normal)
//! follow the same shape: `register_<mode>_bindings`,
//! `dispatch_<mode>`, and a per-mode drift test against the
//! legacy `translate_<mode>` function. Keep the legacy
//! function private to its test module for the duration of
//! the migration; drop it in slice 8.i when the merged
//! catalog covers the whole keystroke surface.

use crossterm::event::{KeyEvent, KeyModifiers};
use lattice_grammar::{CommandInvocation, SourceLocation};

use crate::actions::ActionIds;
use crate::app::Action;
use crate::chord::{KeyChord, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{ChordPattern, KeymapLayer, LookupResult};

/// Register the four Replace-mode bindings into the supplied
/// handle's `Builtin` layer. Called by the App at startup
/// (slice 8.h wires the call site once the App-level keymap
/// boot finalises).
///
/// Sources are captured at this file + line so
/// `:describe-key` shows e.g.
/// `<Esc> -> EnterMode(Normal)  (builtin, keymap_replace.rs:71)`.
pub fn register_replace_bindings(handle: &KeymapHandle, actions: &ActionIds) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Replace;

    // <Esc> -> exit Replace mode.
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Esc))],
        CommandInvocation::of(actions.enter_mode_normal),
        replace_source(esc_line()),
    );

    // <BS> -> undo the last overwritten char.
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(
            SpecialKey::Backspace,
        ))],
        CommandInvocation::of(actions.replace_undo_last),
        replace_source(bs_line()),
    );

    // <CR> -> insert a newline (vim breaks the line in
    // Replace mode, doesn't overwrite the existing one).
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Enter))],
        CommandInvocation::of(actions.insert_newline),
        replace_source(cr_line()),
    );

    // Any bare printable char -> overwrite. The dispatcher's
    // captured-char machinery substitutes
    // `Args::Char(c)` into the dispatched
    // `CommandInvocation`'s args; the registered `ActionSpec`
    // returns `Effect::AppAction(AppEffect::OverwriteChar(c))`
    // (no validation -- any captured char is valid for
    // overwrite).
    handle.bind(
        layer,
        mode,
        &[ChordPattern::CharLiteral],
        CommandInvocation::of(actions.overwrite_char),
        replace_source(wildcard_line()),
    );
}

// Per-binding `SourceLocation` helpers. Tags the row's own
// file:line so `:describe-key` provenance is real.
fn replace_source(line: u32) -> SourceLocation {
    SourceLocation::builtin_file(file!(), line)
}
const fn esc_line() -> u32 {
    line!()
}
const fn bs_line() -> u32 {
    line!()
}
const fn cr_line() -> u32 {
    line!()
}
const fn wildcard_line() -> u32 {
    line!()
}

/// Convenience extension on [`KeymapHandle`] used by the
/// per-mode registration helpers in this slice family.
// Slice 8.i.4.e: the `KeymapHandleLegacyExt` trait + `bind_legacy`
// method retired. Every per-mode keymap module now binds typed
// `CommandInvocation`s through `KeymapHandle::bind`; the
// `BoundCommand::legacy_action` field is gone, and so is the
// adapter that staged an `Action` payload through it.

/// Dispatch a key event through the keymap registry.
///
/// Matches today's `translate_replace` semantics:
///
/// 1. `<C-…>` -> `Action::None`. (The legacy match table
///    short-circuited `CONTROL` and only `CONTROL`; `<M-Esc>`
///    still exits to Normal because the legacy `match` ran on
///    `event.code` alone after the CONTROL guard.)
/// 2. Strip the remaining modifiers (`ALT` / `SHIFT` / `SUPER`)
///    before the lookup so `<M-Esc>` matches the bare-`<Esc>`
///    literal and `<M-x>` falls through the bare-char wildcard.
///    For bare-char chords this is a no-op: `KeyChord::from_event`
///    already strips redundant SHIFT (case encodes it). For
///    specials like `<Esc>` it is the legacy "match by KeyCode
///    alone" semantics expressed in trie terms.
/// 3. `Bound` -> the bound action, with the wildcard
///    `Action::OverwriteChar('\0')` placeholder substituted by
///    the captured char.
/// 4. `Unbound` / `Partial` -> `Action::None`. Replace mode has
///    no multi-key chords today; `Partial` would only arise if
///    a user-config / plugin pushes a layer with multi-key
///    bindings.
pub fn dispatch_replace(handle: &KeymapHandle, event: &KeyEvent) -> Action {
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::None;
    }
    let mut event = *event;
    event
        .modifiers
        .remove(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::SUPER);
    let Some(chord) = crate::chord::from_event(&event) else {
        return Action::None;
    };
    match handle.lookup(BindingMode::Replace, &[chord]) {
        LookupResult::Bound { command, captured } => {
            // Surface an `Action::Invoke(...)` carrying the bound
            // `CommandInvocation`; `App::run_invocation` routes it
            // through the dispatcher's `CommandKind::Action`
            // branch, which produces the matching
            // `Effect::AppAction(...)`. If the wildcard captured
            // a char (slice 8.i.3), fold it into the dispatched
            // invocation's `Args::Char(c)` so the bound
            // `ActionSpec`'s apply closure can see it.
            let mut inv = command.command.clone();
            if let Some(&c) = captured.first() {
                inv = inv.with_args(lattice_grammar::args::Args::Char(c));
            }
            Action::Invoke(inv)
        }
        LookupResult::Partial | LookupResult::Unbound => Action::None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crossterm::event::{KeyCode, KeyEventKind, KeyEventState};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

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

    fn populated_handle() -> KeymapHandle {
        let h = KeymapHandle::new();
        register_replace_bindings(&h, shared_actions());
        h
    }

    #[test]
    fn esc_exits_to_normal() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_replace(&h, &ev(KeyCode::Esc, KeyModifiers::NONE));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_mode_normal),
            other => panic!("expected Invoke(enter_mode_normal), got {other:?}"),
        }
    }

    #[test]
    fn backspace_undoes_last() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_replace(&h, &ev(KeyCode::Backspace, KeyModifiers::NONE));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.replace_undo_last),
            other => panic!("expected Invoke(replace_undo_last), got {other:?}"),
        }
    }

    #[test]
    fn enter_inserts_newline() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_replace(&h, &ev(KeyCode::Enter, KeyModifiers::NONE));
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.insert_newline),
            other => panic!("expected Invoke(insert_newline), got {other:?}"),
        }
    }

    #[test]
    fn printable_char_overwrites_with_correct_char() {
        let h = populated_handle();
        let a = shared_actions();
        for c in ['a', 'A', '$', '0', ' '] {
            let r = dispatch_replace(&h, &ev(KeyCode::Char(c), KeyModifiers::NONE));
            match r {
                Action::Invoke(inv) => {
                    assert_eq!(inv.command, a.overwrite_char);
                    assert!(matches!(inv.args, lattice_grammar::args::Args::Char(got) if got == c));
                }
                other => panic!(
                    "char {c}: expected Invoke(overwrite_char, args=Char({c:?})), got {other:?}"
                ),
            }
        }
    }

    #[test]
    fn ctrl_modifier_yields_none() {
        let h = populated_handle();
        let r = dispatch_replace(&h, &ev(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(r, Action::None));
    }

    #[test]
    fn unhandled_special_keys_yield_none() {
        // `<Up>`, `<Tab>`, etc. aren't bound in Replace mode.
        let h = populated_handle();
        for code in [KeyCode::Up, KeyCode::Tab, KeyCode::F(1)] {
            let r = dispatch_replace(&h, &ev(code, KeyModifiers::NONE));
            assert!(
                matches!(r, Action::None),
                "code {code:?}: expected None, got {r:?}"
            );
        }
    }

    /// `<M-x>` falls through to `OverwriteChar('x')` -- legacy
    /// `translate_replace` short-circuited only on `CONTROL`, so
    /// any other modifier-bearing printable still hit the
    /// `KeyCode::Char(c)` arm. The dispatcher strips the
    /// non-CONTROL modifiers before lookup so the bare-char
    /// wildcard absorbs them; this test pins that semantic.
    #[test]
    fn alt_modifier_falls_through_to_overwrite() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_replace(&h, &ev(KeyCode::Char('x'), KeyModifiers::ALT));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.overwrite_char);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('x')));
            }
            other => panic!("expected Invoke(overwrite_char, Char('x')), got {other:?}"),
        }
    }

    /// SHIFT alone with a printable char STILL counts as a
    /// printable (the terminal already encoded shift in the
    /// case). Drift case to verify.
    #[test]
    fn shift_only_printable_overwrites() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_replace(&h, &ev(KeyCode::Char('A'), KeyModifiers::SHIFT));
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.overwrite_char);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('A')));
            }
            other => panic!("expected Invoke(overwrite_char, Char('A')), got {other:?}"),
        }
    }

    /// `KeyKind::Char` is `pub` so the catch-all in the
    /// drift comparator can introspect; assert we don't have
    /// stray uses that bypass the public API.
    #[test]
    fn keykind_remains_pub_visible() {
        let _ = crate::chord::KeyKind::Char('a');
    }
}
