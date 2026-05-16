//! Crossterm-coupled test harness for the renderer-neutral
//! `lattice_host::keymap_replace` catalog. Production code
//! moved to lattice-host in slice 5.4 / slice 4; the tests
//! stay here because their `ev()` helper builds `KeyChord`
//! values via `crate::chord::from_event(&KeyEvent { ... })`.

pub use lattice_host::keymap_replace::*;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, unused_imports)]
    use super::*;
    use crate::actions::ActionIds;
    use crate::app::Action;
    use crate::chord::KeyChord;
    use crate::keymap_registry::KeymapHandle;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        let raw = KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        crate::chord::from_event(&raw).expect("test event converts to a chord")
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
