//! Visual-mode binding registration + drift-test helpers.
//!
//! Audit slice 8.e. The second mode migrated off `input::translate`'s
//! hand-rolled match table; follows the [slice 8.d
//! template][crate::keymap_replace] (`register_<mode>_bindings` +
//! `dispatch_<mode>` + drift test against a frozen reference body
//! of the legacy translator).
//!
//! ## Surface
//!
//! Visual mode is the same chord table for charwise / linewise /
//! blockwise -- the kind only changes how `Range::Selection`
//! resolves at operator-dispatch time (see
//! `lattice-grammar::dispatcher` §5.2.3). Two block-only
//! exceptions land before the trie lookup:
//!
//! - `I` -> [`Action::EnterBlockVisualInsert`] (blockwise only)
//! - `A` -> [`Action::EnterBlockVisualAppend`] (blockwise only)
//!
//! These are pre-dispatch overrides rather than a separate
//! `BindingMode::VisualBlock` -- the architecture's eventual model
//! is a minor-mode layer pushed at blockwise entry / popped at
//! exit (see `docs/keymap-architecture.md` §5.3); slice 8.e keeps
//! the surgical pre-check until that layer machinery lands. The
//! drift test below pins the kind branch so a future graduation
//! to `push_layer` is mechanical.
//!
//! Common-to-all-kinds bindings registered by
//! [`register_visual_bindings`]:
//!
//! - **Exits**: `<Esc>` / `v` / `V` -> `ExitVisual`.
//! - **Motions** (extend the selection): `h` / `<Left>` /
//!   `j` / `<Down>` / `k` / `<Up>` / `l` / `<Right>` /
//!   `0` / `<Home>` / `$` / `<End>` / `^` / `w` / `b` / `e` /
//!   `W` / `B` / `E` / `}` / `{` / `)` / `(` / `G`. Each binds
//!   to `CommandInvocation::of(motion.0)` -- the
//!   non-`legacy_action` path; the dispatcher returns
//!   `Action::Invoke(command.command.clone())`.
//! - **Operators on selection**: `d` / `x` (delete), `c` / `s`
//!   (change), `y` (yank), `>` (indent right), `<` (indent
//!   left). Each binds to
//!   `CommandInvocation::of(op.0).with_range(Range::Selection)`
//!   -- the operator dispatcher's range walker resolves
//!   `Range::Selection` against the active visual selection.
//!
//! Slice 8.e's net win: every motion / operator binding moves
//! off the `legacy_action` bridge and onto a real
//! `CommandInvocation`. Only the three `ExitVisual` shapes plus
//! the two block-only `Enter*` paths still carry a `legacy_action`
//! -- they don't have a `CommandInvocation` peer today.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lattice_grammar::SourceLocation;
use lattice_grammar::VisualKind;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;

use crate::app::Action;
use crate::chord::{KeyChord, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_replace::KeymapHandleLegacyExt;
use crate::keymap_trie::{
    BoundCommand, ChordPattern, KeymapLayer, LookupResult,
};

/// Register every chord the legacy `input::translate_visual`
/// recognised into the supplied handle's `Builtin` layer under
/// `BindingMode::Visual`. Called at App startup; the registration
/// captures `builtins`'s motion / operator ids by value, so the
/// resulting `BoundCommand`s never re-resolve at lookup time.
///
/// Sources are tagged at this file + line so `:describe-key`
/// shows e.g.
/// `h -> motion:char-left  (builtin, keymap_visual.rs:NN)`.
pub fn register_visual_bindings(handle: &KeymapHandle, builtins: &Builtins) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Visual;

    // Exits: <Esc>, v, V. Three legacy actions; no
    // `CommandInvocation` peer today (slice 8.i's bridge).
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Esc))],
        Action::ExitVisual,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char('v'))],
        Action::ExitVisual,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char('V'))],
        Action::ExitVisual,
        source(),
    );

    // Motions: each chord binds to a typed CommandInvocation.
    // The dispatcher returns `Action::Invoke(command.clone())` --
    // identical to the legacy `invoke(builtins.char_left)`.
    let motion_table: &[(ChordPattern, lattice_grammar::registry::MotionId)] = &[
        (literal(KeyChord::char('h')), builtins.char_left),
        (literal(KeyChord::special(SpecialKey::Left)), builtins.char_left),
        (literal(KeyChord::char('j')), builtins.line_down),
        (literal(KeyChord::special(SpecialKey::Down)), builtins.line_down),
        (literal(KeyChord::char('k')), builtins.line_up),
        (literal(KeyChord::special(SpecialKey::Up)), builtins.line_up),
        (literal(KeyChord::char('l')), builtins.char_right),
        (literal(KeyChord::special(SpecialKey::Right)), builtins.char_right),
        (literal(KeyChord::char('0')), builtins.line_start),
        (literal(KeyChord::special(SpecialKey::Home)), builtins.line_start),
        (literal(KeyChord::char('$')), builtins.line_end),
        (literal(KeyChord::special(SpecialKey::End)), builtins.line_end),
        (literal(KeyChord::char('^')), builtins.first_non_blank),
        (literal(KeyChord::char('w')), builtins.word_forward),
        (literal(KeyChord::char('b')), builtins.word_backward),
        (literal(KeyChord::char('e')), builtins.word_end),
        (literal(KeyChord::char('W')), builtins.big_word_forward),
        (literal(KeyChord::char('B')), builtins.big_word_backward),
        (literal(KeyChord::char('E')), builtins.big_word_end),
        (literal(KeyChord::char('}')), builtins.paragraph_forward),
        (literal(KeyChord::char('{')), builtins.paragraph_backward),
        (literal(KeyChord::char(')')), builtins.sentence_forward),
        (literal(KeyChord::char('(')), builtins.sentence_backward),
        (literal(KeyChord::char('G')), builtins.goto_last_line),
    ];
    for (chord, motion) in motion_table {
        handle.bind(
            layer,
            mode,
            std::slice::from_ref(chord),
            CommandInvocation::of(motion.0),
            source(),
        );
    }

    // Operators on the selection. `Range::Selection` resolves at
    // dispatch time to the active visual region.
    let operator_table: &[(ChordPattern, lattice_grammar::registry::OperatorId)] = &[
        (literal(KeyChord::char('d')), builtins.delete),
        (literal(KeyChord::char('x')), builtins.delete),
        (literal(KeyChord::char('c')), builtins.change),
        (literal(KeyChord::char('s')), builtins.change),
        (literal(KeyChord::char('y')), builtins.yank),
        (literal(KeyChord::char('>')), builtins.indent_right),
        (literal(KeyChord::char('<')), builtins.indent_left),
    ];
    for (chord, op) in operator_table {
        handle.bind(
            layer,
            mode,
            std::slice::from_ref(chord),
            CommandInvocation::of(op.0).with_range(lattice_grammar::Range::Selection),
            source(),
        );
    }
}

fn literal(chord: KeyChord) -> ChordPattern {
    ChordPattern::Literal(chord)
}

fn source() -> SourceLocation {
    // Per-row file + caller line would require a macro; the
    // line-of-this-helper is fine for slice 8.e -- the motion /
    // operator id in the bound command already disambiguates the
    // entry to `:describe-key`. A row-precise capture lands when
    // the catalog enumeration replaces these inline calls (slice
    // 8.i).
    SourceLocation::builtin_file(file!(), line!())
}

/// Dispatch a Visual-mode key event through the keymap registry.
///
/// Matches today's `input::translate_visual` semantics:
///
/// 1. CONTROL-bearing key -> `Action::None`. (Legacy short-
///    circuited `CONTROL` and only `CONTROL`.)
/// 2. Blockwise overlay: `KeyCode::Char('I')` /
///    `KeyCode::Char('A')` go to the
///    `EnterBlockVisualInsert` / `EnterBlockVisualAppend`
///    actions before lookup. Charwise / linewise fall through.
/// 3. Strip the remaining modifiers (ALT / SHIFT / SUPER) and
///    look up in `BindingMode::Visual`. The Replace dispatcher
///    documents the rationale (slice 8.d): legacy matched on
///    `event.code` alone after the CONTROL guard, so
///    non-CONTROL modifiers must be transparent.
/// 4. `Bound` -> the bound action. For `from_invocation`
///    bindings (motions, operators), return
///    `Action::Invoke(command.clone())`. For `legacy_action`
///    bindings (the three `ExitVisual` exits), return the
///    cloned legacy action.
/// 5. `Unbound` / `Partial` -> `Action::None`. Visual mode has
///    no multi-key chords today; `Partial` is reserved for a
///    user-config / plugin layer that registers one.
pub fn dispatch_visual(
    handle: &KeymapHandle,
    event: &KeyEvent,
    kind: VisualKind,
) -> Action {
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::None;
    }
    if matches!(kind, VisualKind::Blockwise) {
        match event.code {
            KeyCode::Char('I') => return Action::EnterBlockVisualInsert,
            KeyCode::Char('A') => return Action::EnterBlockVisualAppend,
            _ => {}
        }
    }
    let mut event = *event;
    event.modifiers.remove(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::SUPER);
    let Some(chord) = KeyChord::from_event(&event) else {
        return Action::None;
    };
    match handle.lookup(BindingMode::Visual, &[chord]) {
        LookupResult::Bound { command, .. } => action_from_bound(&command),
        LookupResult::Partial | LookupResult::Unbound => Action::None,
    }
}

fn action_from_bound(bound: &Arc<BoundCommand>) -> Action {
    match bound.legacy_action.as_ref() {
        Some(action) => action.clone(),
        None => Action::Invoke(bound.command.clone()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use lattice_grammar::{CommandRegistry, builtins::populate};
    use lattice_protocol::ids::CommandId;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn fixture() -> (CommandRegistry, Builtins) {
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        (r, b)
    }

    fn populated_handle(b: &Builtins) -> KeymapHandle {
        let h = KeymapHandle::new();
        register_visual_bindings(&h, b);
        h
    }

    /// Reference implementation -- the exact match arms today's
    /// `input::translate_visual` runs. Kept private to the drift
    /// test; once `translate_visual` switches to call
    /// `dispatch_visual`, this stays as the per-binding regression
    /// net for slice 8.e.
    fn legacy_translate_visual(
        event: KeyEvent,
        kind: VisualKind,
        builtins: &Builtins,
    ) -> Action {
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::None;
        }
        if matches!(kind, VisualKind::Blockwise) {
            match event.code {
                KeyCode::Char('I') => return Action::EnterBlockVisualInsert,
                KeyCode::Char('A') => return Action::EnterBlockVisualAppend,
                _ => {}
            }
        }
        match event.code {
            KeyCode::Esc => Action::ExitVisual,
            KeyCode::Char('v') => Action::ExitVisual,
            KeyCode::Char('V') => Action::ExitVisual,
            KeyCode::Char('h') | KeyCode::Left => invoke(builtins.char_left.0),
            KeyCode::Char('j') | KeyCode::Down => invoke(builtins.line_down.0),
            KeyCode::Char('k') | KeyCode::Up => invoke(builtins.line_up.0),
            KeyCode::Char('l') | KeyCode::Right => invoke(builtins.char_right.0),
            KeyCode::Char('0') | KeyCode::Home => invoke(builtins.line_start.0),
            KeyCode::Char('$') | KeyCode::End => invoke(builtins.line_end.0),
            KeyCode::Char('^') => invoke(builtins.first_non_blank.0),
            KeyCode::Char('w') => invoke(builtins.word_forward.0),
            KeyCode::Char('b') => invoke(builtins.word_backward.0),
            KeyCode::Char('e') => invoke(builtins.word_end.0),
            KeyCode::Char('W') => invoke(builtins.big_word_forward.0),
            KeyCode::Char('B') => invoke(builtins.big_word_backward.0),
            KeyCode::Char('E') => invoke(builtins.big_word_end.0),
            KeyCode::Char('}') => invoke(builtins.paragraph_forward.0),
            KeyCode::Char('{') => invoke(builtins.paragraph_backward.0),
            KeyCode::Char(')') => invoke(builtins.sentence_forward.0),
            KeyCode::Char('(') => invoke(builtins.sentence_backward.0),
            KeyCode::Char('G') => invoke(builtins.goto_last_line.0),
            KeyCode::Char('d') | KeyCode::Char('x') => Action::Invoke(
                CommandInvocation::of(builtins.delete.0)
                    .with_range(lattice_grammar::Range::Selection),
            ),
            KeyCode::Char('c') | KeyCode::Char('s') => Action::Invoke(
                CommandInvocation::of(builtins.change.0)
                    .with_range(lattice_grammar::Range::Selection),
            ),
            KeyCode::Char('y') => Action::Invoke(
                CommandInvocation::of(builtins.yank.0)
                    .with_range(lattice_grammar::Range::Selection),
            ),
            KeyCode::Char('>') => Action::Invoke(
                CommandInvocation::of(builtins.indent_right.0)
                    .with_range(lattice_grammar::Range::Selection),
            ),
            KeyCode::Char('<') => Action::Invoke(
                CommandInvocation::of(builtins.indent_left.0)
                    .with_range(lattice_grammar::Range::Selection),
            ),
            _ => Action::None,
        }
    }

    fn invoke(id: CommandId) -> Action {
        Action::Invoke(CommandInvocation::of(id))
    }

    #[test]
    fn esc_exits_visual_in_all_kinds() {
        let (_, b) = fixture();
        let h = populated_handle(&b);
        for kind in [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ] {
            let r = dispatch_visual(&h, &ev(KeyCode::Esc, KeyModifiers::NONE), kind);
            assert!(matches!(r, Action::ExitVisual), "kind={kind:?}: {r:?}");
        }
    }

    #[test]
    fn lowercase_v_toggles_out_of_visual() {
        let (_, b) = fixture();
        let h = populated_handle(&b);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('v'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        assert!(matches!(r, Action::ExitVisual));
    }

    #[test]
    fn uppercase_v_toggles_out_of_visual() {
        let (_, b) = fixture();
        let h = populated_handle(&b);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('V'), KeyModifiers::NONE),
            VisualKind::Linewise,
        );
        assert!(matches!(r, Action::ExitVisual));
    }

    #[test]
    fn motion_h_invokes_char_left() {
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        let (_, b) = fixture();
        let h = populated_handle(&b);
        let r = dispatch_visual(
            &h,
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
            VisualKind::Charwise,
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                assert!(matches!(
                    inv.range,
                    Some(lattice_grammar::Range::Selection)
                ));
            }
            other => panic!("expected Invoke(delete, Selection), got {other:?}"),
        }
    }

    #[test]
    fn x_in_visual_aliases_to_delete() {
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        assert!(matches!(r, Action::EnterBlockVisualInsert), "block I: {r:?}");
    }

    #[test]
    fn capital_a_only_in_blockwise() {
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        let (_, b) = fixture();
        let h = populated_handle(&b);
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
        let (_, b) = fixture();
        let h = populated_handle(&b);
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

    /// Exhaustive drift test: registry-driven dispatch matches
    /// the legacy `translate_visual` for every key event Visual
    /// mode cares about, across the cross-product of {key} ×
    /// {modifier} × {VisualKind}.
    ///
    /// Per the architecture doc §9 / slice 8.e: this test is
    /// the migration's safety net while both paths exist; it
    /// stays after the switchover to detect any future refactor
    /// that drifts `dispatch_visual` from the legacy semantics.
    #[test]
    fn registry_dispatch_matches_legacy_translate() {
        let (_, b) = fixture();
        let h = populated_handle(&b);

        let codes: Vec<KeyCode> = vec![
            KeyCode::Esc,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::F(1),
        ];
        let chars: Vec<char> = "hjklwbeWBEvVIA0$^{}()<>dxcsy GqzN".chars().collect();
        let mod_sets: Vec<KeyModifiers> = vec![
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ];
        let kinds = [
            VisualKind::Charwise,
            VisualKind::Linewise,
            VisualKind::Blockwise,
        ];

        for &kind in &kinds {
            for &code in &codes {
                for &mods in &mod_sets {
                    let event = ev(code, mods);
                    let legacy = legacy_translate_visual(event, kind, &b);
                    let new = dispatch_visual(&h, &event, kind);
                    assert!(
                        actions_equivalent(&legacy, &new),
                        "drift kind={kind:?} {event:?}: legacy={legacy:?} new={new:?}"
                    );
                }
            }
            for &c in &chars {
                for &mods in &mod_sets {
                    let event = ev(KeyCode::Char(c), mods);
                    let legacy = legacy_translate_visual(event, kind, &b);
                    let new = dispatch_visual(&h, &event, kind);
                    assert!(
                        actions_equivalent(&legacy, &new),
                        "drift kind={kind:?} {event:?}: legacy={legacy:?} new={new:?}"
                    );
                }
            }
        }
    }

    /// Same shape comparator as `keymap_replace` -- `Action`
    /// doesn't impl `PartialEq`. Compares the variants Visual
    /// mode actually emits.
    fn actions_equivalent(a: &Action, b: &Action) -> bool {
        use Action::*;
        match (a, b) {
            (None, None) => true,
            (ExitVisual, ExitVisual) => true,
            (EnterBlockVisualInsert, EnterBlockVisualInsert) => true,
            (EnterBlockVisualAppend, EnterBlockVisualAppend) => true,
            (Invoke(a), Invoke(b)) => {
                a.command == b.command && a.range == b.range && a.count == b.count
            }
            _ => false,
        }
    }
}
