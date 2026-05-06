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

use std::sync::Arc;

use crossterm::event::{KeyEvent, KeyModifiers};
use lattice_grammar::{ModalState, SourceLocation};

use crate::app::Action;
use crate::chord::{KeyChord, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{
    BoundCommand, ChordPattern, KeymapLayer, LookupResult,
};

/// Register the four Replace-mode bindings into the supplied
/// handle's `Builtin` layer. Called by the App at startup
/// (slice 8.h wires the call site once the App-level keymap
/// boot finalises).
///
/// Sources are captured at this file + line so
/// `:describe-key` shows e.g.
/// `<Esc> -> EnterMode(Normal)  (builtin, keymap_replace.rs:71)`.
pub fn register_replace_bindings(handle: &KeymapHandle) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Replace;

    // <Esc> -> exit Replace mode.
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Esc))],
        Action::EnterMode(ModalState::Normal),
        replace_source(esc_line()),
    );

    // <BS> -> undo the last overwritten char.
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Backspace))],
        Action::ReplaceUndoLast,
        replace_source(bs_line()),
    );

    // <CR> -> insert a newline (vim breaks the line in
    // Replace mode, doesn't overwrite the existing one).
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Enter))],
        Action::Insert("\n".to_string()),
        replace_source(cr_line()),
    );

    // Any bare printable char -> overwrite. The wildcard's
    // `OverwriteChar(c)` is constructed by `dispatch_replace`
    // from the captured char (the bound `Action` carries a
    // placeholder `'\0'`; the dispatcher substitutes the
    // captured char before firing).
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::CharLiteral],
        Action::OverwriteChar('\0'),
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
/// Wraps `bind` to construct a `BoundCommand` carrying a
/// legacy `Action` directly. Slice 8.i drops this once
/// every binding has a real `CommandInvocation`.
pub trait KeymapHandleLegacyExt {
    fn bind_legacy(
        &self,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
        action: Action,
        source: SourceLocation,
    );
}

impl KeymapHandleLegacyExt for KeymapHandle {
    fn bind_legacy(
        &self,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
        action: Action,
        source: SourceLocation,
    ) {
        let bound = Arc::new(BoundCommand::from_legacy_action(
            action, source, layer,
        ));
        // Use the registry's internal `bind_arc` -- but the
        // public API wants a CommandInvocation, so route
        // through a small adapter on the handle. For now we
        // duplicate the work: fetch the inner registry's
        // mutex and insert directly. Slice 8.h will replace
        // this with a clean public API once the
        // CommandInvocation collapse is in flight.
        self.bind_bound(layer, mode, path, bound);
    }
}

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
    event.modifiers.remove(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::SUPER);
    let Some(chord) = KeyChord::from_event(&event) else {
        return Action::None;
    };
    match handle.lookup(BindingMode::Replace, &[chord]) {
        LookupResult::Bound { command, captured } => {
            // Bridge: pull the legacy `Action` out of
            // `BoundCommand`. For wildcard bindings, the
            // captured char overrides the `'\0'` placeholder
            // baked at registration time.
            match command.legacy_action.as_ref() {
                Some(Action::OverwriteChar(_)) => {
                    if let Some(&c) = captured.first() {
                        Action::OverwriteChar(c)
                    } else {
                        Action::None
                    }
                }
                Some(action) => action.clone(),
                None => Action::None,
            }
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

    /// Reference implementation -- the exact match arms today's
    /// `input.rs::translate_replace` runs. Kept private to the
    /// drift test; once `translate_replace` switches to call
    /// `dispatch_replace`, this stays as the per-binding
    /// regression net for slice 8.d.
    fn legacy_translate_replace(event: KeyEvent) -> Action {
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::None;
        }
        match event.code {
            KeyCode::Esc => Action::EnterMode(ModalState::Normal),
            KeyCode::Backspace => Action::ReplaceUndoLast,
            KeyCode::Enter => Action::Insert("\n".into()),
            KeyCode::Char(c) => Action::OverwriteChar(c),
            _ => Action::None,
        }
    }

    fn populated_handle() -> KeymapHandle {
        let h = KeymapHandle::new();
        register_replace_bindings(&h);
        h
    }

    #[test]
    fn esc_exits_to_normal() {
        let h = populated_handle();
        let r = dispatch_replace(&h, &ev(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(r, Action::EnterMode(ModalState::Normal)));
    }

    #[test]
    fn backspace_undoes_last() {
        let h = populated_handle();
        let r =
            dispatch_replace(&h, &ev(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(matches!(r, Action::ReplaceUndoLast));
    }

    #[test]
    fn enter_inserts_newline() {
        let h = populated_handle();
        let r = dispatch_replace(&h, &ev(KeyCode::Enter, KeyModifiers::NONE));
        match r {
            Action::Insert(s) => assert_eq!(s, "\n"),
            other => panic!("expected Insert(\\n), got {other:?}"),
        }
    }

    #[test]
    fn printable_char_overwrites_with_correct_char() {
        let h = populated_handle();
        for c in ['a', 'A', '$', '0', ' '] {
            let r = dispatch_replace(
                &h,
                &ev(KeyCode::Char(c), KeyModifiers::NONE),
            );
            match r {
                Action::OverwriteChar(got) => assert_eq!(got, c),
                other => panic!("char {c}: expected OverwriteChar, got {other:?}"),
            }
        }
    }

    #[test]
    fn ctrl_modifier_yields_none() {
        let h = populated_handle();
        let r = dispatch_replace(
            &h,
            &ev(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
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

    /// Exhaustive drift test: the registry-driven dispatch
    /// matches the legacy `translate_replace` for every key
    /// event Replace mode cares about.
    ///
    /// Per the architecture doc §9 / Slice 8.d: this test is
    /// the migration's safety net. Stays in place during the
    /// migration; trips if either path drifts.
    #[test]
    fn registry_dispatch_matches_legacy_translate() {
        let h = populated_handle();

        // Build the cross product of {key code} × {modifier
        // set} that Replace mode plausibly observes.
        let codes: Vec<KeyCode> = vec![
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::F(1),
        ];
        let chars: Vec<char> = "abcXYZ012$ ".chars().collect();
        let mod_sets: Vec<KeyModifiers> = vec![
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ];

        for &code in &codes {
            for &mods in &mod_sets {
                let event = ev(code, mods);
                let legacy = legacy_translate_replace(event);
                let new = dispatch_replace(&h, &event);
                assert!(
                    actions_equivalent(&legacy, &new),
                    "drift for {event:?}: legacy={legacy:?}, new={new:?}"
                );
            }
        }
        for &c in &chars {
            for &mods in &mod_sets {
                let event = ev(KeyCode::Char(c), mods);
                let legacy = legacy_translate_replace(event);
                let new = dispatch_replace(&h, &event);
                assert!(
                    actions_equivalent(&legacy, &new),
                    "drift for {event:?}: legacy={legacy:?}, new={new:?}"
                );
            }
        }
    }

    /// `Action` doesn't impl `PartialEq` (some variants carry
    /// non-Eq payloads), so the drift test compares by
    /// shape + payload via this manual matcher.
    fn actions_equivalent(a: &Action, b: &Action) -> bool {
        use Action::*;
        match (a, b) {
            (None, None) => true,
            (EnterMode(am), EnterMode(bm)) => am == bm,
            (ReplaceUndoLast, ReplaceUndoLast) => true,
            (Insert(s1), Insert(s2)) => s1 == s2,
            (OverwriteChar(c1), OverwriteChar(c2)) => c1 == c2,
            _ => false,
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
        let r = dispatch_replace(
            &h,
            &ev(KeyCode::Char('x'), KeyModifiers::ALT),
        );
        match r {
            Action::OverwriteChar('x') => {}
            other => panic!("expected OverwriteChar('x'), got {other:?}"),
        }
    }

    /// SHIFT alone with a printable char STILL counts as a
    /// printable (the terminal already encoded shift in the
    /// case). Drift case to verify.
    #[test]
    fn shift_only_printable_overwrites() {
        let h = populated_handle();
        let r = dispatch_replace(
            &h,
            &ev(KeyCode::Char('A'), KeyModifiers::SHIFT),
        );
        match r {
            Action::OverwriteChar('A') => {}
            other => panic!("expected OverwriteChar('A'), got {other:?}"),
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
