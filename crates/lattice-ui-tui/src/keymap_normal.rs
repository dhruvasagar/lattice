//! Crossterm-coupled test harness for the renderer-neutral
//! `lattice_host::keymap_normal` catalog. Slice 5.4 / slice 3
//! moved the production code to lattice-host; the tests stay
//! here because their `ev()` helper builds `KeyChord` values
//! via `crate::chord::from_event(&KeyEvent { ... })`, and the
//! crossterm adapter only exists in this crate.
//!
//! `pub use` re-exports every public item from the host so
//! call sites that referenced `lattice_ui_tui::keymap_normal::*`
//! before the move keep resolving without source changes.

pub use lattice_host::keymap_normal::*;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, unused_imports)]
    use super::*;
    use crate::actions::ActionIds;
    use crate::app::{Action, FindKind};
    use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
    use crate::keymap::BindingMode;
    use crate::keymap_registry::KeymapHandle;
    use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, LookupResult};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use lattice_grammar::CommandRegistry;
    use lattice_grammar::Target;
    use lattice_grammar::builtins::Builtins;
    use lattice_grammar::command::CommandInvocation;

    /// Build a canonical `KeyChord` from crossterm pieces. Slice
    /// 5.4 made `lookup_normal*` take `&KeyChord` directly; the
    /// test helper keeps its old `(KeyCode, KeyModifiers)` shape
    /// so test bodies don't churn -- the conversion routes
    /// through the same `chord::from_event` adapter the runtime
    /// uses.
    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        let raw = KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        crate::chord::from_event(&raw).expect("test event converts to a chord")
    }

    /// D.5.b (2026-05-30): shadow the host-crate `lookup_normal`
    /// signature change. Production gained
    /// `active_minor_modes: &[ModeId]` so chord bindings
    /// registered against `MinorMode(ModeId)` layers gate on
    /// per-buffer activation (K.1.c). These tests exercise the
    /// always-on `Builtin`-layer catalog; they pass `&[]`
    /// (no minor modes active) to keep behaviour identical to
    /// the pre-D.5.b legacy `lookup` path.
    fn lookup_normal(handle: &KeymapHandle, chord: &KeyChord) -> Option<Action> {
        lattice_host::keymap_normal::lookup_normal(handle, chord, &[])
    }

    fn lookup_normal_with_prefix(
        handle: &KeymapHandle,
        prefix: &[KeyChord],
        chord: &KeyChord,
    ) -> Action {
        lattice_host::keymap_normal::lookup_normal_with_prefix(handle, prefix, chord, &[])
    }

    fn fixture() -> (CommandRegistry, Builtins, ActionIds) {
        let mut r = CommandRegistry::new();
        let b = lattice_grammar::builtins::populate(&mut r);
        let a = crate::actions::populate(&mut r, &b);
        (r, b, a)
    }

    fn populated_handle() -> (KeymapHandle, Builtins, ActionIds) {
        let (mut r, b, a) = fixture();
        let so = lattice_syntax::register_syntax_text_objects(&mut r);
        let h = KeymapHandle::new();
        register_normal_bindings(&h, &b, &a, &so);
        (h, b, a)
    }

    #[test]
    fn motion_h_invokes_char_left() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('h'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn arrow_left_aliases_char_left() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Left, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn upper_g_invokes_goto_last_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('G'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.goto_last_line.0),
            other => panic!("expected Invoke(goto_last_line), got {other:?}"),
        }
    }

    #[test]
    fn pseudo_operator_x_carries_char_right_target() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('x'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(inv.target, Some(Target::Motion(m, _)) if m == b.char_right));
            }
            other => panic!("expected Invoke(delete, char_right), got {other:?}"),
        }
    }

    #[test]
    fn pseudo_operator_d_carries_line_end_target() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('D'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(inv.target, Some(Target::Motion(m, _)) if m == b.line_end));
            }
            other => panic!("expected Invoke(delete, line_end), got {other:?}"),
        }
    }

    #[test]
    fn pseudo_operator_y_capital_uses_current_line_range() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('Y'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.yank.0);
                assert!(matches!(
                    inv.range,
                    Some(lattice_grammar::Range::CurrentLine)
                ));
            }
            other => panic!("expected Invoke(yank, CurrentLine), got {other:?}"),
        }
    }

    #[test]
    fn viewport_h_jumps_to_top() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('H'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.jump_viewport_top),
            other => panic!("expected Invoke(jump_viewport_top), got {other:?}"),
        }
    }

    #[test]
    fn mode_entry_v_enters_charwise_visual() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('v'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.enter_visual_charwise),
            other => panic!("expected Invoke(enter_visual_charwise), got {other:?}"),
        }
    }

    #[test]
    fn paste_p_lower_pastes_after() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('p'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.paste_after),
            other => panic!("expected Invoke(paste_after), got {other:?}"),
        }
    }

    #[test]
    fn search_slash_enters_forward_search() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('/'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.enter_search_forward),
            other => panic!("expected Invoke(enter_search_forward), got {other:?}"),
        }
    }

    #[test]
    fn tab_jumps_history_forward() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Tab, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.jump_history_forward),
            other => panic!("expected Invoke(jump_history_forward), got {other:?}"),
        }
    }

    #[test]
    fn page_down_invokes_line_down_with_count_ten() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::PageDown, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.line_down.0);
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(10)));
            }
            other => panic!("expected Invoke(line_down, count=10), got {other:?}"),
        }
    }

    /// Slice 8.g.iii: `d` is now a terminal binding that arms
    /// `Pending::AfterOperator(delete)`. The trie still has
    /// children (`[d, w]`, `[d, d]`, etc.) for the second-key
    /// resolution, but lookup of `[d]` alone returns `Bound`
    /// because the depth-1 node carries a binding.
    #[test]
    fn d_invokes_absorb_operator_delete() {
        // Slice 8.i.4.c: pressing `d` invokes the typed
        // `absorb_operator_delete` action, which emits
        // `AppEffect::AbsorbOperatorPrefix(delete)`. App's
        // handler latches op_count and pushes [d] to
        // partial_chord.
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('d'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, a.absorb_operator_delete);
            }
            other => panic!("expected Invoke(absorb_operator_delete), got {other:?}"),
        }
    }

    #[test]
    fn dw_resolves_to_delete_with_word_forward_target() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('w'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(
                    inv.target,
                    Some(Target::Motion(m, _)) if m == b.word_forward
                ));
            }
            other => panic!("expected Invoke(delete, word_forward), got {other:?}"),
        }
    }

    #[test]
    fn dd_resolves_to_delete_current_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(
                    inv.range,
                    Some(lattice_grammar::Range::CurrentLine)
                ));
            }
            other => panic!("expected Invoke(delete, CurrentLine), got {other:?}"),
        }
    }

    #[test]
    fn yy_resolves_to_yank_current_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('y')],
            &ev(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.yank.0);
                assert!(matches!(
                    inv.range,
                    Some(lattice_grammar::Range::CurrentLine)
                ));
            }
            other => panic!("expected Invoke(yank, CurrentLine), got {other:?}"),
        }
    }

    #[test]
    fn cc_resolves_to_change_current_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('c')],
            &ev(KeyCode::Char('c'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.change.0);
                assert!(matches!(
                    inv.range,
                    Some(lattice_grammar::Range::CurrentLine)
                ));
            }
            other => panic!("expected Invoke(change, CurrentLine), got {other:?}"),
        }
    }

    #[test]
    fn di_absorbs_partial_chord() {
        // Slice 8.i.4.c: with prefix [d], pressing `i` absorbs
        // into partial_chord. The trie returns Partial because
        // `[d, i, w]` etc. are bound; lookup_normal_with_prefix
        // emits `AbsorbPartialChord(i)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('i'), KeyModifiers::NONE),
        );
        assert!(matches!(
            r,
            Action::AbsorbPartialChord(c) if c == KeyChord::char('i')
        ));
    }

    #[test]
    fn diw_resolves_to_delete_inner_word() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d'), KeyChord::char('i')],
            &ev(KeyCode::Char('w'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(
                    inv.target,
                    Some(Target::TextObject(id, _)) if id == b.inner_word
                ));
            }
            other => panic!("expected Invoke(delete, inner_word), got {other:?}"),
        }
    }

    #[test]
    fn dab_resolves_to_delete_around_paren() {
        // Alias check: `b` inside `da` resolves to around_paren.
        let (h, b_, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d'), KeyChord::char('a')],
            &ev(KeyCode::Char('b'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b_.delete.0);
                assert!(matches!(
                    inv.target,
                    Some(Target::TextObject(id, _)) if id == b_.around_paren
                ));
            }
            other => panic!("expected Invoke(delete, around_paren), got {other:?}"),
        }
    }

    #[test]
    fn df_absorbs_partial_chord() {
        // Slice 8.i.4.c: with prefix [d], pressing `f` absorbs
        // into partial_chord. The trie returns Partial because
        // `[d, f, *]` is bound (find-char wildcard).
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        assert!(matches!(
            r,
            Action::AbsorbPartialChord(c) if c == KeyChord::char('f')
        ));
    }

    #[test]
    fn d_unrecognised_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('Q'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }

    /// Doubled-operator under the `g` prefix: `gUU` -> linewise
    /// upper. The prefix walk is `[g, U, U]`.
    #[test]
    fn g_uu_resolves_to_upper_current_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g'), KeyChord::char('U')],
            &ev(KeyCode::Char('U'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.upper.0);
                assert!(matches!(
                    inv.range,
                    Some(lattice_grammar::Range::CurrentLine)
                ));
            }
            other => panic!("expected Invoke(upper, CurrentLine), got {other:?}"),
        }
    }

    /// `gUw` -- upper applied to the word_forward motion target.
    #[test]
    fn g_uw_resolves_to_upper_with_word_forward() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g'), KeyChord::char('U')],
            &ev(KeyCode::Char('w'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.upper.0);
                assert!(matches!(
                    inv.target,
                    Some(Target::Motion(m, _)) if m == b.word_forward
                ));
            }
            other => panic!("expected Invoke(upper, word_forward), got {other:?}"),
        }
    }

    /// Slice 8.g.ii: `g` is a partial trie node (children only,
    /// no terminal binding). `lookup_normal` converts the
    /// `LookupResult::Partial` into `SetPending(AfterG)` so the
    /// dispatcher arms the second-key resolver.
    #[test]
    fn g_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(g)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::char('g')
        ));
    }

    #[test]
    fn z_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(z)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::char('z')
        ));
    }

    #[test]
    fn gg_resolves_to_goto_first_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.goto_first_line.0),
            other => panic!("expected Invoke(goto_first_line), got {other:?}"),
        }
    }

    #[test]
    fn gd_without_lsp_mode_is_unresolved_at_builtin_layer() {
        // MO.1: gd migrated to LspMode::keymap() (MinorMode layer).
        // Builtin trie no longer has gd → Action::None without lsp-mode.
        let (h, _, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None), "expected Action::None, got {r:?}");
    }

    #[test]
    fn gu_invokes_absorb_operator_lower() {
        // Slice 8.i.4.c: `gu` (with prefix [g]) resolves to
        // Invoke(absorb_operator_lower).
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_lower),
            other => panic!("expected Invoke(absorb_operator_lower), got {other:?}"),
        }
    }

    #[test]
    fn g_capital_j_resolves_to_join_lines_without_space() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('J'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.join_lines_bare),
            other => panic!("expected Invoke(join_lines_bare), got {other:?}"),
        }
    }

    #[test]
    fn zz_centers_cursor() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_center),
            other => panic!("expected Invoke(scroll_cursor_to_center), got {other:?}"),
        }
    }

    #[test]
    fn z_dot_aliases_zz() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('.'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_center),
            other => panic!("expected Invoke(scroll_cursor_to_center), got {other:?}"),
        }
    }

    #[test]
    fn z_enter_aliases_zt() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Enter, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_top),
            other => panic!("expected Invoke(scroll_cursor_to_top), got {other:?}"),
        }
    }

    #[test]
    fn z_dash_aliases_zb() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('-'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_bottom),
            other => panic!("expected Invoke(scroll_cursor_to_bottom), got {other:?}"),
        }
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.toggle_fold_at_cursor),
            other => panic!("expected Invoke(toggle_fold_at_cursor), got {other:?}"),
        }
    }

    #[test]
    fn z_unrecognized_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }

    #[test]
    fn z_esc_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }

    /// Slice 8.g.v: `q` outside macro recording arms
    /// `Pending::AfterMacroStart`. The recording-state-dependent
    /// branch (`StopMacroRecord` when already recording) lives
    /// in `compute_normal_action` as a short-circuit before the
    /// trie lookup -- the registry doesn't see App state.
    #[test]
    fn q_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(q)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::char('q')
        ));
    }

    // ---- Slice 8.g.v: wildcard chord paths ----

    #[test]
    fn ma_resolves_to_set_mark_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('m')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.set_mark);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(set_mark, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn m_invalid_passes_char_to_actionspec() {
        // Slice 8.i.3: validation moved from the dispatcher to
        // the bound `ActionSpec`. The dispatcher returns
        // `Invoke(set_mark)` with the captured `'!'` regardless
        // of validity; the spec returns `Effect::None` for
        // non-alphanumeric chars and `App::apply` clears the
        // pending state on every Invoke.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('m')],
            &ev(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.set_mark);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('!')));
            }
            other => panic!("expected Invoke(set_mark, Char('!')), got {other:?}"),
        }
    }

    #[test]
    fn apostrophe_a_resolves_to_jump_mark_line_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('\'')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.jump_to_mark_line);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(jump_to_mark_line, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn backtick_a_resolves_to_jump_mark_exact_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('`')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.jump_to_mark_exact);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(jump_to_mark_exact, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn quote_a_selects_named_register_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(select_register, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn quote_plus_selects_system_register() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('+'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('+')));
            }
            other => panic!("expected Invoke(select_register, Char('+')), got {other:?}"),
        }
    }

    #[test]
    fn quote_zero_selects_numbered_register_zero() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('0'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('0')));
            }
            other => panic!("expected Invoke(select_register, Char('0')), got {other:?}"),
        }
    }

    #[test]
    fn quote_invalid_passes_char_to_actionspec() {
        // Slice 8.i.3: validation lives in the bound `ActionSpec`,
        // not the dispatcher. The dispatched `Invoke` carries the
        // captured `'!'`; the spec returns `Effect::None` for chars
        // that don't name a register.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('!')));
            }
            other => panic!("expected Invoke(select_register, Char('!')), got {other:?}"),
        }
    }

    #[test]
    fn qa_starts_macro_record_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('q')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.start_macro_record);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(start_macro_record, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn at_at_plays_last_macro() {
        // The dispatcher returns `Invoke(play_macro, Char('@'))`;
        // the bound `ActionSpec` reads `@` and produces
        // `AppEffect::PlayLastMacro` rather than `PlayMacro('@')`.
        // Slice 8.i.3 moved this branching from the dispatcher's
        // legacy substituter into the spec.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('@')],
            &ev(KeyCode::Char('@'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.play_macro);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('@')));
            }
            other => panic!("expected Invoke(play_macro, Char('@')), got {other:?}"),
        }
    }

    #[test]
    fn at_a_plays_macro_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('@')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.play_macro);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(play_macro, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn f_x_resolves_to_find_char_forward_with_args() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('f')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.find_char_forward.0);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('X')));
            }
            other => panic!("expected Invoke(find_char_forward, Char('X')), got {other:?}"),
        }
    }

    #[test]
    fn dfx_resolves_to_delete_with_find_char_target() {
        // `df<X>` -- delete forward up to and including 'X'.
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d'), KeyChord::char('f')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(m, args)) => {
                        assert_eq!(m, b.find_char_forward);
                        assert!(matches!(args, lattice_grammar::args::Args::Char('X')));
                    }
                    other => panic!("expected Motion(find_char_forward, Char('X')), got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Wildcard rejects modifier-bearing chords (per
    /// `keymap_trie`'s wildcard rule). `f<C-x>` is unbound and
    /// the dispatcher drops the pending state -- a documented
    /// drift from legacy `resolve_after_find_char`, which
    /// accepted any `KeyCode::Char(c)` regardless of modifiers.
    /// Terminals don't typically emit `f<C-x>` and the
    /// alternative chord representation is the trie's invariant.
    #[test]
    fn f_ctrl_x_falls_through_to_drop_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('f')],
            &ev(KeyCode::Char('x'), KeyModifiers::CONTROL),
        );
        assert!(matches!(r, Action::None));
    }

    /// `<S-h>`'s SHIFT is stripped by `KeyChord::from_event`
    /// for bare letters (case carries the bit), so the trie
    /// only needs `(Char('h'), NONE)`. Pin that here.
    #[test]
    fn shift_h_resolves_via_lowercase_chord() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('h'), KeyModifiers::SHIFT));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    /// `<M-h>` falls through to char_left -- legacy
    /// `translate_normal` matched on `event.code` alone after
    /// the CTRL guard, so non-CONTROL modifiers are
    /// transparent. Same modifier-transparency as Replace /
    /// Visual.
    #[test]
    fn alt_h_resolves_to_char_left() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('h'), KeyModifiers::ALT));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    // ---- Slice 8.g.iv: attach_count.

    fn invoke_no_count() -> Action {
        Action::Invoke(CommandInvocation::of(
            lattice_protocol::ids::CommandId::new(42),
        ))
    }

    fn invoke_with_default_count(n: u32) -> Action {
        Action::Invoke(
            CommandInvocation::of(lattice_protocol::ids::CommandId::new(42))
                .with_count(lattice_grammar::command::Count(n)),
        )
    }

    #[test]
    fn attach_count_pending_count_only() {
        let r = attach_count(invoke_no_count(), 5, 0);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(5)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_count_op_times_motion() {
        let r = attach_count(invoke_no_count(), 3, 2);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(6)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_count_op_only_uses_default_motion_count_one() {
        // pending_count == 0 and inv has no default => motion_count = 1.
        // op_count = 4 => final = 4.
        let r = attach_count(invoke_no_count(), 0, 4);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(4)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_count_default_count_baked_in_used_when_pending_zero() {
        // PageDown shape: the binding registered with Count(10).
        // pending_count == 0 => motion_count falls back to
        // inv.count = 10. op_count == 0 => final = 10.
        let r = attach_count(invoke_with_default_count(10), 0, 0);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(10)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_count_pending_overrides_default_count() {
        // `5<PageDown>`: pending_count=5 wins over the binding's
        // baked-in Count(10).
        let r = attach_count(invoke_with_default_count(10), 5, 0);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(5)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_count_no_attachment_when_final_is_one() {
        // `j` with no count: motion_count=1, op_count=0, final=1.
        // Don't write `Count(1)` -- keep the invocation's count
        // field `None` (legacy semantics).
        let r = attach_count(invoke_no_count(), 0, 0);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.count, None),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_count_passes_through_non_invoke_actions() {
        let r = attach_count(Action::ExitVisual, 5, 0);
        assert!(matches!(r, Action::ExitVisual));
        let r = attach_count(Action::None, 0, 0);
        assert!(matches!(r, Action::None));
        // Slice 8.i.4.d: AbsorbPartialChord is the new "non-Invoke
        // pass-through" attach_count case (was SetPending(_)).
        let r = attach_count(Action::AbsorbPartialChord(KeyChord::char('g')), 5, 0);
        assert!(matches!(r, Action::AbsorbPartialChord(_)));
    }

    #[test]
    fn attach_count_idempotent_when_re_applied() {
        // App's existing count math runs *after* translate's
        // attach_count for the legacy interactive flow. Pin
        // idempotence: re-applying with the same pending /
        // op_count yields the same Count.
        let once = attach_count(invoke_no_count(), 3, 2);
        let once_clone = match &once {
            Action::Invoke(inv) => Action::Invoke(inv.clone()),
            _ => panic!(),
        };
        let twice = attach_count(once_clone, 3, 2);
        match (once, twice) {
            (Action::Invoke(a), Action::Invoke(b)) => {
                assert_eq!(a.count, b.count);
                assert_eq!(a.count, Some(lattice_grammar::command::Count(6)));
            }
            other => panic!("got {other:?}"),
        }
    }

    // ---- Slice 8.g.vi: CTRL chords + <C-w> sub-tree ----

    #[test]
    fn ctrl_d_resolves_to_line_down_count_ten() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('d'), KeyModifiers::CONTROL));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.line_down.0);
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(10)));
            }
            other => panic!("expected Invoke(line_down, count=10), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_o_resolves_to_jump_history_back() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('o'), KeyModifiers::CONTROL));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.jump_history_back),
            other => panic!("expected Invoke(jump_history_back), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_v_enters_blockwise_visual() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('v'), KeyModifiers::CONTROL));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.enter_visual_blockwise),
            other => panic!("expected Invoke(enter_visual_blockwise), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(<C-w>)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::ctrl('w')
        ));
    }

    #[test]
    fn ctrl_w_then_l_navigates_pane_right() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_right),
            other => panic!("expected Invoke(navigate_pane_right), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_ctrl_l_also_navigates_pane_right() {
        // Vim accepts ctrl-modified second keys after `<C-w>`
        // (sticky-prefix muscle memory).
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_right),
            other => panic!("expected Invoke(navigate_pane_right), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_arrow_left_navigates_pane_left() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Left, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_left),
            other => panic!("expected Invoke(navigate_pane_left), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_backspace_navigates_pane_left() {
        // Many terminals collapse `<C-h>` to Backspace; the
        // bare-Backspace path covers that.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Backspace, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_left),
            other => panic!("expected Invoke(navigate_pane_left), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_tab_cycles_to_next_pane() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Tab, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.next_pane),
            other => panic!("expected Invoke(next_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_back_tab_cycles_to_prev_pane() {
        // BackTab normalises to chord `(Tab, SHIFT)` via
        // `KeyChord::from_event`; the trie has the explicit
        // `<S-Tab>` registration under `[<C-w>]`.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::BackTab, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.prev_pane),
            other => panic!("expected Invoke(prev_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_v_splits_vertical() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.split_pane_vertical),
            other => panic!("expected Invoke(split_pane_vertical), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_capital_s_splits_horizontal() {
        // `<C-w>S` is a legacy alias for `<C-w>s`.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('S'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.split_pane_horizontal),
            other => panic!("expected Invoke(split_pane_horizontal), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_q_closes_pane() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('q'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.close_pane),
            other => panic!("expected Invoke(close_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_esc_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }
}
