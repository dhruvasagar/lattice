//! Crossterm-coupled test harness for the renderer-neutral
//! `lattice_host::keymap_visual` catalog. Production code
//! moved to lattice-host in slice 5.4 / slice 4; the tests
//! stay here because their `ev()` helper builds `KeyChord`
//! values via `crate::chord::from_event(&KeyEvent { ... })`.

pub use lattice_host::keymap_visual::*;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, unused_imports)]
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use lattice_grammar::{CommandRegistry, VisualKind, builtins::Builtins, builtins::populate};
    use crate::actions::ActionIds;
    use crate::app::Action;
    use crate::chord::KeyChord;
    use crate::keymap_registry::KeymapHandle;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        let raw = KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        crate::chord::from_event(&raw).expect("test event converts to a chord")
    }

    fn fixture() -> (CommandRegistry, Builtins, ActionIds) {
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        let a = crate::actions::populate(&mut r, &b);
        (r, b, a)
    }

    fn populated_handle(b: &Builtins, a: &ActionIds) -> KeymapHandle {
        let h = KeymapHandle::new();
        register_visual_bindings(&h, b, a);
        h
    }

    #[test]
    fn esc_exits_visual_in_all_kinds() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            let r = dispatch_visual(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), kind);
            match r {
                Action::Invoke(inv) => assert_eq!(inv.command, a.exit_visual),
                other => panic!("kind={kind:?}: expected Invoke(exit_visual), got {other:?}"),
            }
        }
    }

    #[test]
    fn lowercase_v_toggles_out_of_visual() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('v'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.exit_visual),
            other => panic!("expected Invoke(exit_visual), got {other:?}"),
        }
    }

    #[test]
    fn uppercase_v_toggles_out_of_visual() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('V'), KeyModifiers::NONE),
            VisualKind::Linewise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.exit_visual),
            other => panic!("expected Invoke(exit_visual), got {other:?}"),
        }
    }

    #[test]
    fn motion_h_invokes_char_left() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('h'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn arrow_left_aliases_to_char_left() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Left, KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn delete_in_visual_carries_selection_range() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(inv.range, Some(lattice_grammar::Range::Selection)));
            }
            other => panic!("expected Invoke(delete, Selection), got {other:?}"),
        }
    }

    #[test]
    fn x_in_visual_aliases_to_delete() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('x'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.delete.0),
            other => panic!("expected Invoke(delete), got {other:?}"),
        }
    }

    #[test]
    fn s_in_visual_aliases_to_change() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('s'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.change.0),
            other => panic!("expected Invoke(change), got {other:?}"),
        }
    }

    #[test]
    fn capital_i_only_in_blockwise() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        // Charwise: I has no binding -> None.
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('I'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        assert!(matches!(r, Action::None), "charwise I: {r:?}");
        // Linewise: same.
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('I'), KeyModifiers::NONE),
            VisualKind::Linewise,
        );
        assert!(matches!(r, Action::None), "linewise I: {r:?}");
        // Blockwise: I -> EnterBlockVisualInsert.
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('I'), KeyModifiers::NONE),
            VisualKind::Blockwise,
        );
        assert!(
            matches!(r, Action::EnterBlockVisualInsert),
            "block I: {r:?}"
        );
    }

    #[test]
    fn capital_a_only_in_blockwise() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('A'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        assert!(matches!(r, Action::None));
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('A'), KeyModifiers::NONE),
            VisualKind::Blockwise,
        );
        assert!(matches!(r, Action::EnterBlockVisualAppend));
    }

    #[test]
    fn ctrl_modifier_yields_none() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('h'), KeyModifiers::CONTROL),
            VisualKind::Charwise,
        );
        assert!(matches!(r, Action::None));
    }

    /// Modifier transparency: `<M-h>` falls through to char_left
    /// just like the legacy `translate_visual` did. Same rationale
    /// as Replace mode (slice 8.d): the legacy match table only
    /// short-circuited CONTROL.
    #[test]
    fn alt_h_in_visual_invokes_char_left() {
        let (_, b, a) = fixture();
        let h = populated_handle(&b, &a);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('h'), KeyModifiers::ALT),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }
}
