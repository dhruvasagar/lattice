//! Crossterm-coupled test harness for the renderer-neutral
//! `lattice_host::keymap_insert` catalog. Production code
//! moved to lattice-host in slice 5.4 / slice 4; the tests
//! stay here because their `ev()` helper builds `KeyChord`
//! values via `crate::chord::from_event(&KeyEvent { ... })`.

pub use lattice_host::keymap_insert::*;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, unused_imports)]
    use super::*;
    use crate::actions::ActionIds;
    use crate::app::Action;
    use crate::chord::KeyChord;
    use crate::keymap_registry::{KeymapHandle, PushLayerKind};
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

    /// Process-wide shared `ActionIds`. Built once on first
    /// access. The IDs are stable for the duration of a test
    /// run so cross-test handle-and-id comparisons hold.
    /// Process-wide shared `(registry, actions)`, built once. The
    /// registry is kept (not discarded) so the snippet mode-keymap
    /// layer can be translated against the SAME registry that minted
    /// `shared_actions()` -- `CommandId`s are only stable *within* one
    /// registry instance, so a layer built from a second registry
    /// would resolve `action:snippet-next-placeholder` to a different
    /// id than the test asserts.
    fn shared_init() -> &'static (lattice_grammar::CommandRegistry, ActionIds) {
        use std::sync::OnceLock;
        static INIT: OnceLock<(lattice_grammar::CommandRegistry, ActionIds)> = OnceLock::new();
        INIT.get_or_init(|| {
            let mut r = lattice_grammar::CommandRegistry::new();
            let b = lattice_grammar::builtins::populate(&mut r);
            let _ = lattice_grammar::ex_commands::populate(&mut r);
            let a = crate::actions::populate(&mut r, &b);
            (r, a)
        })
    }

    fn shared_actions() -> &'static ActionIds {
        &shared_init().1
    }

    fn shared_registry() -> &'static lattice_grammar::CommandRegistry {
        &shared_init().0
    }

    fn populated_handle() -> KeymapHandle {
        let h = KeymapHandle::new();
        register_insert_bindings(&h, shared_actions());
        h
    }

    fn populated_handle_with_popup() -> KeymapHandle {
        let h = populated_handle();
        h.push_layer(
            PushLayerKind::MinorMode(completion_popup_mode_id()),
            "completion-popup",
            completion_popup_layer_bindings(shared_actions()),
        );
        h
    }

    /// Push the `active-snippet-mode` layer via the K.2.4 translation path
    /// so the test uses the same mechanism as editor boot.
    fn push_snippet_layer_via_k24(h: &KeymapHandle) {
        // Translate the snippet mode keymap against the SHARED registry
        // (not a fresh one) so its bindings resolve `action:snippet-*`
        // to the same `CommandId`s `shared_actions()` exposes.
        let mut mr = lattice_mode::ModeRegistry::new();
        mr.register(lattice_snippet::modes::SnippetActiveMode)
            .expect("register active-snippet-mode");
        // SN.3c.1: `snippet-mode` owns the `<C-x><C-s>` expand chord
        // (Insert) via `keymap()`. Register it here too so the
        // translated layer carries the chord — mirrors boot, where
        // `<C-x><C-s>` is no longer a Builtin binding.
        mr.register(lattice_snippet::modes::SnippetMode::new())
            .expect("register snippet-mode");
        lattice_host::keymap_mode_contributions::translate_mode_keymaps(h, &mr, shared_registry());
    }

    fn populated_handle_with_snippet() -> KeymapHandle {
        let h = populated_handle();
        push_snippet_layer_via_k24(&h);
        h
    }

    fn populated_handle_with_both() -> KeymapHandle {
        // Order matters under the pre-K.1.c global pre-merge:
        // snippet first, then popup, so popup's later
        // installation wins on overlapping chords. After K.1.c
        // the per-buffer active-mode order drives precedence
        // — this fixture still exercises the lower-level
        // registry merge ordering directly.
        let h = populated_handle();
        push_snippet_layer_via_k24(&h);
        h.push_layer(
            PushLayerKind::MinorMode(completion_popup_mode_id()),
            "completion-popup",
            completion_popup_layer_bindings(shared_actions()),
        );
        h
    }

    // ---- Base Insert ----

    #[test]
    fn esc_in_base_insert_returns_to_normal() {
        let h = populated_handle();
        let a = shared_actions();
        match dispatch_insert(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), &[]) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.enter_mode_normal),
            other => panic!("expected Invoke(enter_mode_normal), got {other:?}"),
        }
    }

    #[test]
    fn backspace_in_base_insert_deletes_char_backward() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Backspace, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.delete_char_backward),
            other => panic!("expected Invoke(delete_char_backward), got {other:?}"),
        }
    }

    #[test]
    fn enter_in_base_insert_inserts_newline() {
        let h = populated_handle();
        let a = shared_actions();
        match dispatch_insert(&h, &ev(KeyCode::Enter, KeyModifiers::NONE), &[]) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.insert_newline),
            other => panic!("expected Invoke(insert_newline), got {other:?}"),
        }
    }

    #[test]
    fn tab_in_base_insert_inserts_tab() {
        let h = populated_handle();
        let a = shared_actions();
        match dispatch_insert(&h, &ev(KeyCode::Tab, KeyModifiers::NONE), &[]) {
            Action::Invoke(inv) => assert_eq!(inv.command, a.insert_tab),
            other => panic!("expected Invoke(insert_tab), got {other:?}"),
        }
    }

    #[test]
    fn printable_char_in_base_insert_falls_through_to_insert() {
        let h = populated_handle();
        for c in ['a', 'A', '1', '$', ' '] {
            match dispatch_insert(&h, &ev(KeyCode::Char(c), KeyModifiers::NONE), &[]) {
                Action::Insert(s) => assert_eq!(s, c.to_string()),
                other => panic!("char {c:?}: expected Insert, got {other:?}"),
            }
        }
    }

    #[test]
    fn ctrl_letter_unbound_in_base_insert_yields_none() {
        let h = populated_handle();
        // <C-y> isn't bound at base; legacy returned None.
        let r = dispatch_insert(&h, &ev(KeyCode::Char('y'), KeyModifiers::CONTROL), &[]);
        assert!(matches!(r, Action::None));
    }

    #[test]
    fn ctrl_space_in_base_insert_triggers_completion() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char(' '), KeyModifiers::CONTROL), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_trigger),
            other => panic!("expected Invoke(completion_trigger), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_x_in_base_insert_absorbs_partial_chord() {
        // Slice 8.i.4.b: pressing `<C-x>` returns
        // `Action::AbsorbPartialChord(<C-x>)`. The trie's `Partial`
        // result drives the App's `partial_chord` stack; the next
        // keystroke runs through `dispatch_insert` with that prefix
        // and resolves `<C-x><C-s>`.
        //
        // SN.3c.1: `<C-x>` is a partial prefix ONLY because
        // `snippet-mode`'s layer provides the `<C-x><C-s>` terminal —
        // it's no longer Builtin. Use the snippet-layer handle so the
        // merged trie sees the prefix (matches boot, where the layer
        // is pushed).
        let h = populated_handle_with_snippet();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('x'), KeyModifiers::CONTROL), &[]);
        match r {
            Action::AbsorbPartialChord(c) => assert_eq!(c, KeyChord::ctrl('x')),
            other => panic!("expected AbsorbPartialChord(<C-x>), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_x_then_ctrl_o_no_longer_binds() {
        // CSM.K1: `<C-x><C-o>` (vim omni-completion alias)
        // retired. The chord falls through to the dispatcher
        // with no Invoke; `<C-Space>` is the sole popup trigger.
        let h = populated_handle();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &[KeyChord::ctrl('x')],
        );
        assert!(
            !matches!(r, Action::Invoke(_)),
            "<C-x><C-o> should no longer resolve to an Invoke; got {r:?}",
        );
    }

    #[test]
    fn ctrl_x_then_ctrl_s_resolves_to_snippet_expand() {
        // SN.3c.1: `<C-x><C-s>` is contributed by `snippet-mode`'s
        // layer (not Builtin), so resolve against the snippet-layer
        // handle — same merged trie the editor sees once the layer is
        // boot-pushed.
        let h = populated_handle_with_snippet();
        let a = shared_actions();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &[KeyChord::ctrl('x')],
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.snippet_expand),
            other => panic!("expected Invoke(snippet_expand), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_x_then_unrecognized_drops_partial_chord() {
        // Slice 8.i.4.b: an unrecognised second key after
        // `<C-x>` returns `Action::None`. `App::apply`'s
        // non-`AbsorbPartialChord(_)` clear-rule resets
        // `partial_chord` automatically -- no explicit
        // `SetPending(None)` round-trip needed.
        let h = populated_handle();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('q'), KeyModifiers::CONTROL),
            &[KeyChord::ctrl('x')],
        );
        assert!(matches!(r, Action::None));
    }

    // ---- Completion popup overlay ----

    #[test]
    fn popup_ctrl_n_navigates_next() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('n'), KeyModifiers::CONTROL), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_next),
            other => panic!("expected Invoke(completion_next), got {other:?}"),
        }
    }

    #[test]
    fn popup_down_arrow_navigates_next() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Down, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_next),
            other => panic!("expected Invoke(completion_next), got {other:?}"),
        }
    }

    #[test]
    fn popup_tab_accepts() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Tab, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn popup_enter_accepts() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Enter, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn popup_esc_cancels_and_exits_insert() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_cancel_and_exit_insert)
            }
            other => panic!("expected Invoke(completion_cancel_and_exit_insert), got {other:?}"),
        }
    }

    #[test]
    fn popup_ctrl_e_cancels_only() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('e'), KeyModifiers::CONTROL), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_cancel),
            other => panic!("expected Invoke(completion_cancel), got {other:?}"),
        }
    }

    #[test]
    fn popup_bare_char_routes_through_accept_then_insert() {
        use lattice_grammar::args::Args;
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('a'), KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_accept_then_insert);
                assert!(matches!(inv.args, Args::Char('a')));
            }
            other => {
                panic!("expected Invoke(completion_accept_then_insert, Char('a')), got {other:?}")
            }
        }
    }

    /// CSM.K2: `<C-b>` inside the popup filters to
    /// `gen:buffer-words` (Args::String payload). The previous
    /// docs-scroll-up binding has been moved to `PageUp`.
    #[test]
    fn popup_ctrl_b_filters_to_buffer_words() {
        use lattice_grammar::args::Args;
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('b'), KeyModifiers::CONTROL), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_filter_to_source);
                match inv.args {
                    Args::String(s) => {
                        assert_eq!(s, lattice_completion::insert::BufferWordsSource::ID)
                    }
                    other => panic!("expected Args::String, got {other:?}"),
                }
            }
            other => panic!(
                "expected Invoke(completion_filter_to_source, \"gen:buffer-words\"), got {other:?}"
            ),
        }
    }

    /// CSM.K2: `<C-o>` filters to LSP.
    #[test]
    fn popup_ctrl_o_filters_to_lsp() {
        use lattice_grammar::args::Args;
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('o'), KeyModifiers::CONTROL), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_filter_to_source);
                match inv.args {
                    Args::String(s) => {
                        assert_eq!(s, lattice_completion::insert::LSP_COMPLETION_SOURCE_ID)
                    }
                    other => panic!("expected Args::String, got {other:?}"),
                }
            }
            other => panic!("expected Invoke(completion_filter_to_source, lsp), got {other:?}"),
        }
    }

    /// CSM.K2: `<C-Space>` inside the popup clears the active
    /// source filter (replaces the legacy
    /// `completion_trigger` binding on this layer).
    #[test]
    fn popup_ctrl_space_clears_filter() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Char(' '), KeyModifiers::CONTROL), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_filter_clear);
            }
            other => panic!("expected Invoke(completion_filter_clear), got {other:?}"),
        }
    }

    /// CSM.K2: docs-scroll moved off `<C-f>`/`<C-b>` (now filter
    /// chords) to `PageDown` / `PageUp`.
    #[test]
    fn popup_page_down_scrolls_docs() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::PageDown, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_docs_scroll_down)
            }
            other => panic!("expected Invoke(completion_docs_scroll_down), got {other:?}"),
        }
    }

    #[test]
    fn popup_ctrl_x_falls_through_to_base_insert_partial_chord() {
        // Slice 8.i.4.b: the popup layer doesn't bind <C-x>; lookup
        // falls through to the `<C-x>` partial node, returning
        // `AbsorbPartialChord(<C-x>)`.
        //
        // SN.3c.1: that partial node is no longer Builtin — it's
        // contributed by `snippet-mode`'s `<C-x><C-s>` layer. In
        // production both layers coexist (snippet-mode is Global on
        // every document buffer, popup-mode is active while the popup
        // is open), so the fixture pushes both. The assertion still
        // protects the same property: the popup layer must not shadow
        // the `<C-x>` partial.
        let h = populated_handle_with_both();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('x'), KeyModifiers::CONTROL), &[]);
        match r {
            Action::AbsorbPartialChord(c) => assert_eq!(c, KeyChord::ctrl('x')),
            other => panic!("expected AbsorbPartialChord(<C-x>), got {other:?}"),
        }
    }

    // ---- Active snippet overlay ----

    #[test]
    fn snippet_tab_jumps_to_next_placeholder() {
        let h = populated_handle_with_snippet();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Tab, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_next_placeholder)
            }
            other => panic!("expected Invoke(snippet_next_placeholder), got {other:?}"),
        }
    }

    #[test]
    fn snippet_back_tab_jumps_to_prev_placeholder() {
        let h = populated_handle_with_snippet();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::BackTab, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_prev_placeholder)
            }
            other => panic!("expected Invoke(snippet_prev_placeholder), got {other:?}"),
        }
    }

    #[test]
    fn snippet_shift_tab_jumps_to_prev_placeholder() {
        let h = populated_handle_with_snippet();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Tab, KeyModifiers::SHIFT), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_prev_placeholder)
            }
            other => panic!("expected Invoke(snippet_prev_placeholder), got {other:?}"),
        }
    }

    #[test]
    fn snippet_esc_leaves_snippet() {
        let h = populated_handle_with_snippet();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.snippet_leave),
            other => panic!("expected Invoke(snippet_leave), got {other:?}"),
        }
    }

    #[test]
    fn snippet_unrelated_key_falls_through_to_base_insert() {
        let h = populated_handle_with_snippet();
        let r = dispatch_insert(&h, &ev(KeyCode::Char('z'), KeyModifiers::NONE), &[]);
        match r {
            Action::Insert(s) => assert_eq!(s, "z"),
            other => panic!("expected Insert(z), got {other:?}"),
        }
    }

    // ---- Combined overlays: popup wins ----

    #[test]
    fn popup_wins_over_snippet_for_tab() {
        let h = populated_handle_with_both();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Tab, KeyModifiers::NONE), &[]);
        // Popup binds <Tab> -> CompletionAccept; snippet binds
        // <Tab> -> SnippetNextPlaceholder. Popup pushed last
        // (higher LayerId) so popup wins.
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_accept),
            other => panic!("expected Invoke(completion_accept), got {other:?}"),
        }
    }

    #[test]
    fn popup_wins_over_snippet_for_esc() {
        let h = populated_handle_with_both();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_cancel_and_exit_insert)
            }
            other => panic!("expected Invoke(completion_cancel_and_exit_insert), got {other:?}"),
        }
    }

    #[test]
    fn shift_tab_with_both_overlays_resolves_via_snippet_layer() {
        // <S-Tab> is unique to the snippet layer; popup doesn't
        // bind it. Falls through to snippet -> SnippetPrevPlaceholder.
        let h = populated_handle_with_both();
        let a = shared_actions();
        let r = dispatch_insert(&h, &ev(KeyCode::BackTab, KeyModifiers::NONE), &[]);
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_prev_placeholder)
            }
            other => panic!("expected Invoke(snippet_prev_placeholder), got {other:?}"),
        }
    }
}
