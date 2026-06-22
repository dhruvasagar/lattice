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
    use crate::actions::ActionIds;
    use crate::app::Action;
    use crate::chord::KeyChord;
    use crate::keymap_registry::KeymapHandle;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use lattice_grammar::{CommandRegistry, VisualKind, builtins::Builtins, builtins::populate};
    use lattice_syntax::SyntaxTextObjectIds;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        let raw = KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        crate::chord::from_event(&raw).expect("test event converts to a chord")
    }

    fn fixture() -> (CommandRegistry, Builtins, ActionIds, SyntaxTextObjectIds) {
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        let a = crate::actions::populate(&mut r, &b);
        let so = lattice_syntax::register_syntax_text_objects(&mut r);
        (r, b, a, so)
    }

    fn populated_handle(b: &Builtins, a: &ActionIds, so: &SyntaxTextObjectIds) -> KeymapHandle {
        let h = KeymapHandle::new();
        register_visual_bindings(&h, b, a, so);
        // Operators (`d` / `c` / `y` / `>` / `<`) bind into Visual via
        // `register_operator_bindings` (called by `register_normal_bindings`), not
        // `register_visual_bindings` -- an operator acts on the selection
        // by design. Tests here dispatch those operator chords in Visual,
        // so the Normal catalog must be registered too. The `x` / `s`
        // Visual-only aliases still come from `register_visual_bindings`.
        crate::keymap_normal::register_normal_bindings(&h, b, a, so);
        h
    }

    /// Dispatch a fresh (non-mid-sequence) Visual chord -- empty
    /// `partial_chord`. The bulk of the catalog is single-key.
    fn dv(h: &KeymapHandle, chord: &KeyChord, kind: VisualKind) -> Action {
        dispatch_visual(h, chord, kind, &[])
    }

    #[test]
    fn esc_exits_visual_in_all_kinds() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            let r = dv(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), kind);
            match r {
                Action::Invoke(inv) => assert_eq!(inv.command, a.exit_visual),
                other => panic!("kind={kind:?}: expected Invoke(exit_visual), got {other:?}"),
            }
        }
    }

    #[test]
    fn lowercase_v_toggles_out_of_visual() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
            &h,
            &ev(KeyCode::Char('s'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.change.0),
            other => panic!("expected Invoke(change), got {other:?}"),
        }
    }

    /// Concern #1 contract: an operator acts on the Visual selection BY
    /// DESIGN. A contributed, MULTI-KEY operator (narrow's `zn` shape)
    /// registered through `register_operator_bindings` resolves in Visual to
    /// `op.with_range(Selection)` with ZERO per-operator Visual binding
    /// hand-listed — exactly the path `d`/`c`/`y` now use. Reuses
    /// `b.delete`'s OperatorId under the novel `zn` prefix; the contract
    /// under test is keymap generation across modes, not the effect.
    #[test]
    fn contributed_operator_acts_on_visual_selection_by_design() {
        let (_, b, a, so) = fixture();
        let _ = &a;
        let h = KeymapHandle::new();
        let z = crate::keymap_trie::ChordPattern::Literal(KeyChord::char('z'));
        let n = crate::keymap_trie::ChordPattern::Literal(KeyChord::char('n'));
        crate::keymap_normal::register_operator_bindings(&h, &[z, n.clone()], b.delete, n, &b, &so);

        // Visual `zn`: `z` absorbs as a partial, `n` resolves the pair to
        // the operator carrying `Range::Selection`.
        let prefix = [ev(KeyCode::Char('z'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('n'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(
                    matches!(inv.range, Some(lattice_grammar::Range::Selection)),
                    "a contributed operator must carry Range::Selection in Visual"
                );
            }
            other => panic!("expected Invoke(op, Selection), got {other:?}"),
        }

        // ...and the SAME registration still yields the Normal
        // operator-pending family: `zn` + motion `j` targets the motion.
        // Proves one `register_operator_bindings` call wires BOTH modes — Visual
        // selection-operability is intrinsic, not a second binding.
        let znj = lattice_host::keymap_normal::lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z'), KeyChord::char('n')],
            &KeyChord::char('j'),
            &[],
        );
        match znj {
            Action::Invoke(inv) => assert_eq!(inv.command, b.delete.0),
            other => panic!("expected Normal `znj` Invoke(op), got {other:?}"),
        }
    }

    #[test]
    fn capital_i_only_in_blockwise() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        // Charwise: I has no binding -> None.
        let r = dv(
            &h,
            &ev(KeyCode::Char('I'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        assert!(matches!(r, Action::None), "charwise I: {r:?}");
        // Linewise: same.
        let r = dv(
            &h,
            &ev(KeyCode::Char('I'), KeyModifiers::NONE),
            VisualKind::Linewise,
        );
        assert!(matches!(r, Action::None), "linewise I: {r:?}");
        // Blockwise: I -> EnterBlockVisualInsert.
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
            &h,
            &ev(KeyCode::Char('A'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        assert!(matches!(r, Action::None));
        let r = dv(
            &h,
            &ev(KeyCode::Char('A'), KeyModifiers::NONE),
            VisualKind::Blockwise,
        );
        assert!(matches!(r, Action::EnterBlockVisualAppend));
    }

    #[test]
    fn ctrl_modifier_yields_none() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
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
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
            &h,
            &ev(KeyCode::Char('h'), KeyModifiers::ALT),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    // ---- Text objects in Visual mode (visual-foundation slice).
    //
    // `viw` / `vaw` / `vi{` / `vaf` / `vaC` ... are two-key chords
    // (`i` / `a` prefix + object char) resolved via the same
    // partial-chord machinery Normal mode uses. The first key absorbs
    // into the host's `partial_chord`; the second resolves the pair.
    // There is NO per-object code -- every row in
    // `keymap_normal::text_object_rows` works automatically.

    /// Bare `i` (in Visual) is a text-object prefix: it must absorb
    /// into `partial_chord`, not no-op.
    #[test]
    fn bare_i_absorbs_as_text_object_prefix() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
            &h,
            &ev(KeyCode::Char('i'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::AbsorbPartialChord(c) => {
                assert_eq!(c, ev(KeyCode::Char('i'), KeyModifiers::NONE));
            }
            other => panic!("expected AbsorbPartialChord(i), got {other:?}"),
        }
    }

    /// Bare `a` likewise absorbs as a text-object prefix.
    #[test]
    fn bare_a_absorbs_as_text_object_prefix() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let r = dv(
            &h,
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        assert!(
            matches!(r, Action::AbsorbPartialChord(_)),
            "expected AbsorbPartialChord(a), got {r:?}"
        );
    }

    /// `viw` -> with `partial_chord = [i]`, the `w` resolves to a
    /// bare `inner_word` text-object invocation (no operator, no
    /// range -- the grammar's `execute_text_object` sets the span).
    #[test]
    fn viw_resolves_to_bare_inner_word() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let prefix = [ev(KeyCode::Char('i'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('w'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.inner_word.0);
                assert!(inv.range.is_none(), "bare text object carries no range");
                assert!(inv.target.is_none(), "bare text object carries no target");
            }
            other => panic!("expected Invoke(inner_word), got {other:?}"),
        }
    }

    /// `vaw` -> around-word via the `a` prefix.
    #[test]
    fn vaw_resolves_to_bare_around_word() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let prefix = [ev(KeyCode::Char('a'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('w'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.around_word.0),
            other => panic!("expected Invoke(around_word), got {other:?}"),
        }
    }

    /// `vi{` -> inner brace (a bracket-family alias). Confirms the
    /// shared table's alias rows reach Visual mode too.
    #[test]
    fn vi_brace_resolves_to_inner_brace() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let prefix = [ev(KeyCode::Char('i'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('{'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.inner_brace.0),
            other => panic!("expected Invoke(inner_brace), got {other:?}"),
        }
    }

    /// `vaf` -> around-function: the tree-sitter structural object,
    /// proving the syntax rows are wired identically in Visual.
    #[test]
    fn vaf_resolves_to_around_function() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let prefix = [ev(KeyCode::Char('a'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('f'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, so.around_function.0),
            other => panic!("expected Invoke(around_function), got {other:?}"),
        }
    }

    /// `vaC` -> around-comment (the N.1.6 comment object), capital C.
    #[test]
    fn va_capital_c_resolves_to_around_comment() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let prefix = [ev(KeyCode::Char('a'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('C'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.around_comment.0),
            other => panic!("expected Invoke(around_comment), got {other:?}"),
        }
    }

    /// Garbage after a text-object prefix aborts (returns `None`), so
    /// the host clears `partial_chord` -- matching vim cancelling the
    /// prefix on an unbound second key.
    #[test]
    fn unbound_after_prefix_yields_none() {
        let (_, b, a, so) = fixture();
        let h = populated_handle(&b, &a, &so);
        let prefix = [ev(KeyCode::Char('i'), KeyModifiers::NONE)];
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('z'), KeyModifiers::NONE),
            VisualKind::Charwise,
            &prefix,
        );
        assert!(matches!(r, Action::None), "expected None, got {r:?}");
    }
}
