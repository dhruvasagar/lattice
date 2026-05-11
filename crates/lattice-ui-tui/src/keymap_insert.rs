//! Insert-mode binding registration + drift-test helpers.
//!
//! Audit slice 8.f. Third mode migrated off `input::translate`'s
//! hand-rolled match table. Insert is bigger than Replace / Visual
//! because two minor-mode overlays ride on top of base Insert
//! (architecture doc §5.3):
//!
//! - **Completion popup** (`App.insert_completion = Some(...)`):
//!   the popup claims a fixed set of CTRL-bearing chords plus
//!   `<Tab>` / `<CR>` / `<Esc>` plus a bare-char wildcard
//!   ("commit-then-insert"); other chords fall through to base
//!   Insert.
//! - **Active snippet** (`App.active_snippet = Some(...)`): the
//!   snippet claims `<Tab>` / `<S-Tab>` / `<Esc>` for
//!   placeholder navigation; other chords fall through to base
//!   Insert. Popup wins when both overlays are active (legacy
//!   `&& !ctx.insert_completion_open` gate).
//!
//! ## Layer model
//!
//! Each overlay is registered as a [`KeymapLayer::MinorMode`]
//! layer pushed onto the registry when the overlay activates and
//! popped when it deactivates. Push order is enforced by
//! `App::sync_keymap_overlays`: snippet first, popup second, so
//! popup's `LayerId` is higher and popup wins on overlapping
//! chords (preserving the legacy "popup precedes snippet"
//! gating).
//!
//! ## Base Insert bindings
//!
//! Registered directly into [`KeymapLayer::Builtin`] +
//! `BindingMode::Insert` by [`register_insert_bindings`]:
//!
//! - `<Esc>` -> [`Action::EnterMode(Normal)`]
//! - `<BS>` -> [`Action::DeleteCharBackward`]
//! - `<CR>` -> [`Action::Insert("\n")`]
//! - `<Tab>` -> [`Action::Insert("\t")`]
//! - `<C-Space>` -> [`Action::CompletionTrigger`]
//! - `[<C-x>, <C-o>]` -> [`Action::CompletionTrigger`] (omni-completion)
//! - `[<C-x>, <C-s>]` -> [`Action::SnippetExpand`]
//!
//! `<C-x>` itself is a *partial* trie node (no terminal binding;
//! children only). Lookup at `[<C-x>]` returns
//! [`LookupResult::Partial`]; [`dispatch_insert`] translates that
//! into [`Action::SetPending(Pending::AfterCtrlX)`]. The next
//! keystroke arrives with `pending = AfterCtrlX` and the
//! dispatcher reconstructs the two-chord sequence
//! `[<C-x>, current_chord]` for the lookup.
//!
//! ## Literal-text fall-through
//!
//! Per the architecture doc §9 / slice 8.f bullet, "type any
//! printable char that has no binding" stays a dispatcher default
//! rather than a registered char wildcard. Lookup at an
//! unmodified `Char(c)` returns [`LookupResult::Unbound`] in base
//! Insert; the dispatcher's [`literal_text_fallback`] returns
//! `Action::Insert(c.to_string())` (suppressing `CONTROL`-bearing
//! chars to match legacy semantics). When the popup layer is
//! pushed, its char-wildcard wins, so literal typing routes
//! through `CompletionAcceptThenInsert(c)` instead -- the popup
//! handler in App decides whether to accept the focused candidate
//! or fall back to plain insertion.
//!
//! ## Modifier transparency (drift caveats)
//!
//! Legacy `translate_insert` matched on `event.code` alone for
//! `<Esc>` / `<BS>` / `<CR>` / `<Tab>` (modifiers ignored), and
//! short-circuited only `CONTROL` on the `Char(c)` arm. The trie
//! is precise: `(Esc, NONE)` and `(Esc, CONTROL)` are distinct
//! chords. To bridge, [`dispatch_insert`] runs a
//! mode-specific normalisation pass before lookup:
//!
//! | chord shape                | normalisation                |
//! |----------------------------|------------------------------|
//! | `Special(_)` + ALT/SUPER   | strip ALT, SUPER             |
//! | `Char(_)` without CTRL     | strip ALT, SUPER             |
//! | `Char(_)` with CTRL        | strip ALT, SUPER             |
//!
//! SHIFT is preserved on specials so the snippet layer can
//! distinguish `<S-Tab>` from `<Tab>`. SHIFT is preserved on
//! CTRL+letter so `<C-S-c>` stays distinct from `<C-c>`. SHIFT
//! is preserved on bare letters too (the chord normalisation in
//! [`KeyChord::from_event`] already strips redundant SHIFT for
//! bare ASCII letters where case carries the bit).
//!
//! Three documented drift cases vs. legacy (acceptable per the
//! drift test's allow-list -- terminals don't emit these in
//! practice):
//!
//! - `<S-Esc>` (SHIFT + Esc): legacy returned `EnterMode(Normal)`;
//!   new returns `None` (chord `(Esc, SHIFT)` has no entry; SHIFT
//!   is preserved on specials).
//! - `<C-Esc>` (CONTROL + Esc): legacy returned
//!   `EnterMode(Normal)`; new returns `None`.
//! - `<S-Tab>` as `KeyCode::Tab + SHIFT` (rare; usually arrives
//!   as `KeyCode::BackTab` instead): legacy returned `Insert("\t")`;
//!   new returns `SnippetPrevPlaceholder` if the snippet layer
//!   is pushed, else `None`. `KeyCode::BackTab` (the common path)
//!   is unaffected -- `KeyChord::from_event` normalises BackTab
//!   to `(Tab, SHIFT)`, identical handling.

use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lattice_grammar::CommandInvocation;
use lattice_grammar::SourceLocation;
use lattice_protocol::ids::CommandId;

use crate::actions::ActionIds;
use crate::app::Action;
use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{
    BoundCommand, ChordPattern, KeymapLayer, KeymapTrie, LookupResult,
};

/// Register every chord the legacy `input::translate_insert`
/// recognised into the supplied handle's `Builtin` layer under
/// `BindingMode::Insert`. Called at App startup.
///
/// `<C-x>` is registered implicitly: inserting
/// `[<C-x>, <C-o>]` at depth 2 makes the depth-1 lookup of
/// `[<C-x>]` return [`LookupResult::Partial`]. Same for
/// `[<C-x>, <C-s>]`.
pub fn register_insert_bindings(handle: &KeymapHandle, actions: &ActionIds) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Insert;

    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::Esc)],
        CommandInvocation::of(actions.enter_mode_normal),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::Backspace)],
        CommandInvocation::of(actions.delete_char_backward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::Enter)],
        CommandInvocation::of(actions.insert_newline),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::Tab)],
        CommandInvocation::of(actions.insert_tab),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl(' '))],
        CommandInvocation::of(actions.completion_trigger),
        source(),
    );
    // CSM.K1: `<C-x><C-o>` (vim omni-completion) retired.
    // `<C-Space>` is the sole popup-open trigger; per-source
    // filter chords live inside `completion-popup-mode` (CSM.K2).
    // `<C-x><C-s>` (snippet-expand-at-cursor) is independent of
    // the popup family and stays.
    // <C-x><C-s> -- direct snippet expansion.
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('x')), lit(KeyChord::ctrl('s'))],
        CommandInvocation::of(actions.snippet_expand),
        source(),
    );
}

/// Build the completion-popup minor-mode layer's binding set.
/// Wrapped into the registry by `App::push_completion_popup_layer`
/// when the popup opens; popped when the popup closes.
///
/// Returns one trie keyed under `BindingMode::Insert` -- the only
/// mode the popup is active in. The registry's merge picks up
/// every entry under that mode whenever the layer is pushed.
pub fn completion_popup_layer_bindings(
    actions: &ActionIds,
) -> HashMap<BindingMode, KeymapTrie> {
    let mut trie = KeymapTrie::new();
    let layer = KeymapLayer::MinorMode(0); // tag overridden by registry on push

    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('n'))],
        actions.completion_next,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Down)],
        actions.completion_next,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('p'))],
        actions.completion_prev,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Up)],
        actions.completion_prev,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('y'))],
        actions.completion_accept,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Tab)],
        actions.completion_accept,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Enter)],
        actions.completion_accept,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('e'))],
        actions.completion_cancel,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Esc)],
        actions.completion_cancel_and_exit_insert,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl(' '))],
        actions.completion_trigger,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('d'))],
        actions.completion_toggle_docs,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('f'))],
        actions.completion_docs_scroll_down,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('b'))],
        actions.completion_docs_scroll_up,
    );
    // Char wildcard: any bare printable -> commit-or-insert. The
    // dispatcher folds the captured char into the typed
    // invocation's `Args::Char(c)`; the bound `ActionSpec`
    // returns `AppEffect::CompletionAcceptThenInsert(c)`.
    bind_invocation(
        &mut trie,
        layer,
        &[ChordPattern::CharLiteral],
        actions.completion_accept_then_insert,
    );

    let mut modes = HashMap::new();
    modes.insert(BindingMode::Insert, trie);
    modes
}

/// Build the active-snippet minor-mode layer's binding set.
/// Pushed by `App::push_snippet_layer` when an `ActiveSnippet`
/// activates; popped on snippet exit.
pub fn active_snippet_layer_bindings(
    actions: &ActionIds,
) -> HashMap<BindingMode, KeymapTrie> {
    let mut trie = KeymapTrie::new();
    let layer = KeymapLayer::MinorMode(0); // tag overridden by registry

    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Tab)],
        actions.snippet_next_placeholder,
    );
    // <S-Tab> -- chord (Tab, SHIFT). KeyChord::from_event
    // canonicalises both `KeyCode::BackTab` and
    // `KeyCode::Tab + SHIFT` to the same chord.
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord {
            key: KeyKind::Special(SpecialKey::Tab),
            mods: KeyMods::SHIFT,
        })],
        actions.snippet_prev_placeholder,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::Esc)],
        actions.snippet_leave,
    );

    let mut modes = HashMap::new();
    modes.insert(BindingMode::Insert, trie);
    modes
}

/// Dispatch a key event in Insert mode through the layered
/// keymap registry. Replaces the legacy
/// `input::translate_insert` plus the
/// `translate_insert_completion_popup` and
/// `translate_active_snippet` overlay branches at the top of
/// `input::translate`.
///
/// 1. `pending == AfterCtrlX`: reconstruct
///    `[<C-x>, normalised(event)]`, look up. Bound -> the bound
///    action; anything else -> `SetPending(None)` to drop the
///    pending state and let the user retry (matches legacy).
/// 2. Otherwise: normalise the chord per the modifier table in
///    this module's docstring; look up `[chord]`.
///    - `Bound` -> the bound action. Wildcard captures fill the
///      char placeholder in `CompletionAcceptThenInsert`.
///    - `Partial` -> the only multi-key prefix in Insert today
///      is `<C-x>`; emit `SetPending(AfterCtrlX)` for that
///      specific chord. Any other partial path is defensive
///      `Action::None` (no caller can produce one with the
///      current catalog).
///    - `Unbound` -> [`literal_text_fallback`] for printable
///      chars without CONTROL; otherwise `Action::None`.
pub fn dispatch_insert(
    handle: &KeymapHandle,
    event: &KeyEvent,
    partial_chord: &[KeyChord],
) -> Action {
    // Slice 8.i.4: partial-chord dispatch wins when a previous
    // keystroke absorbed a prefix into `App::partial_chord`.
    // This drives the `<C-x>` family (`<C-x><C-o>` /
    // `<C-x><C-s>`) and any future Insert-mode multi-key chord.
    if !partial_chord.is_empty() {
        let Some(chord) = KeyChord::from_event(event) else {
            return Action::None;
        };
        let chord = normalize_for_insert_lookup(chord);
        let mut path: Vec<KeyChord> = partial_chord.to_vec();
        path.push(chord);
        return match handle.lookup(BindingMode::Insert, &path) {
            LookupResult::Bound { command, captured } => {
                action_from_bound(&command, &captured)
            }
            _ => Action::None,
        };
    }

    let Some(raw_chord) = KeyChord::from_event(event) else {
        return literal_text_fallback(event);
    };
    let chord = normalize_for_insert_lookup(raw_chord);
    match handle.lookup(BindingMode::Insert, &[chord]) {
        LookupResult::Bound { command, captured } => {
            action_from_bound(&command, &captured)
        }
        LookupResult::Partial => {
            // Slice 8.i.4.b: every trie `Partial` in Insert mode
            // (currently only `<C-x>`) absorbs into
            // `App::partial_chord` via `AbsorbPartialChord`. The
            // next keystroke runs with this stack as prefix and
            // hits the trie's resolved `[<C-x>, <C-o>]` /
            // `[<C-x>, <C-s>]` binding.
            Action::AbsorbPartialChord(chord)
        }
        LookupResult::Unbound => literal_text_fallback(event),
    }
}

/// Mode-specific modifier strip. See module docstring's table.
fn normalize_for_insert_lookup(chord: KeyChord) -> KeyChord {
    // Strip ALT and SUPER on every chord -- no Insert binding
    // (base or overlay) uses them. Keep CTRL and SHIFT to
    // distinguish `<C-y>` from `y` and `<S-Tab>` from `<Tab>`.
    let mut mods = KeyMods::NONE;
    if chord.mods.ctrl() {
        mods = mods | KeyMods::CTRL;
    }
    if chord.mods.shift() {
        mods = mods | KeyMods::SHIFT;
    }
    KeyChord {
        key: chord.key,
        mods,
    }
}

/// Pull the typed `CommandInvocation` out of a bound trie node,
/// folding any captured wildcard char into the invocation's
/// `Args::Char(c)` (slice 8.i.4.e: replaces the prior
/// `legacy_action`-aware substitution with the same shape used
/// in keymap_normal / keymap_replace -- the bound `ActionSpec`
/// validates and emits the typed `AppEffect`).
fn action_from_bound(bound: &Arc<BoundCommand>, captured: &[char]) -> Action {
    let mut inv = bound.command.clone();
    if let Some(&c) = captured.first() {
        inv = inv.with_args(lattice_grammar::args::Args::Char(c));
    }
    Action::Invoke(inv)
}

/// Dispatcher fallback for unbound chords in base Insert. Mirrors
/// the legacy `translate_insert`'s tail:
/// - CONTROL-bearing -> `Action::None`.
/// - `KeyCode::Char(c)` (any non-CONTROL modifier) -> `Insert(c.to_string())`.
/// - Anything else -> `Action::None`.
fn literal_text_fallback(event: &KeyEvent) -> Action {
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::None;
    }
    match event.code {
        KeyCode::Char(c) => Action::Insert(c.to_string()),
        _ => Action::None,
    }
}

fn lit(chord: KeyChord) -> ChordPattern {
    ChordPattern::Literal(chord)
}

fn lit_special(s: SpecialKey) -> ChordPattern {
    ChordPattern::Literal(KeyChord::special(s))
}

fn source() -> SourceLocation {
    SourceLocation::builtin_file(file!(), line!())
}

/// Helper for the per-overlay trie builders -- stages a typed
/// `CommandInvocation` (slice 8.i.4.e: replaces the legacy
/// `bind_action` that wrapped `Action::Foo` payloads via
/// `BoundCommand::from_legacy_action`). `KeymapLayer` is set on
/// the `BoundCommand` for `:describe-key` provenance; the
/// registry overrides the layer tag with the freshly-issued
/// `MinorMode(id)` when the layer is pushed.
fn bind_invocation(
    trie: &mut KeymapTrie,
    layer: KeymapLayer,
    path: &[ChordPattern],
    command: CommandId,
) {
    let bound = Arc::new(BoundCommand::from_invocation(
        CommandInvocation::of(command),
        source(),
        layer,
    ));
    trie.insert(path, bound);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::keymap_registry::{KeymapHandle, PushLayerKind};
    use crossterm::event::{KeyCode, KeyEventKind, KeyEventState};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Process-wide shared `ActionIds`. Built once on first
    /// access. The IDs are stable for the duration of a test
    /// run so cross-test handle-and-id comparisons hold.
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
        register_insert_bindings(&h, shared_actions());
        h
    }

    fn populated_handle_with_popup() -> KeymapHandle {
        let h = populated_handle();
        h.push_layer(
            PushLayerKind::MinorMode,
            "completion-popup",
            completion_popup_layer_bindings(shared_actions()),
        );
        h
    }

    fn populated_handle_with_snippet() -> KeymapHandle {
        let h = populated_handle();
        h.push_layer(
            PushLayerKind::MinorMode,
            "active-snippet",
            active_snippet_layer_bindings(shared_actions()),
        );
        h
    }

    fn populated_handle_with_both() -> KeymapHandle {
        // Order matters: snippet first, then popup. Popup's
        // higher LayerId means popup wins on overlapping chords
        // (legacy "popup precedes snippet" gating).
        let h = populated_handle();
        h.push_layer(
            PushLayerKind::MinorMode,
            "active-snippet",
            active_snippet_layer_bindings(shared_actions()),
        );
        h.push_layer(
            PushLayerKind::MinorMode,
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Backspace, KeyModifiers::NONE),
            &[],
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.delete_char_backward),
            other => panic!("expected Invoke(delete_char_backward), got {other:?}"),
        }
    }

    #[test]
    fn enter_in_base_insert_inserts_newline() {
        let h = populated_handle();
        let a = shared_actions();
        match dispatch_insert(
            &h,
            &ev(KeyCode::Enter, KeyModifiers::NONE),
            &[],
        ) {
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
            match dispatch_insert(
                &h,
                &ev(KeyCode::Char(c), KeyModifiers::NONE),
                &[],
            ) {
                Action::Insert(s) => assert_eq!(s, c.to_string()),
                other => panic!("char {c:?}: expected Insert, got {other:?}"),
            }
        }
    }

    #[test]
    fn ctrl_letter_unbound_in_base_insert_yields_none() {
        let h = populated_handle();
        // <C-y> isn't bound at base; legacy returned None.
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &[],
        );
        assert!(matches!(r, Action::None));
    }

    #[test]
    fn ctrl_space_in_base_insert_triggers_completion() {
        let h = populated_handle();
        let a = shared_actions();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &[],
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_trigger),
            other => panic!("expected Invoke(completion_trigger), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_x_in_base_insert_absorbs_partial_chord() {
        // Slice 8.i.4.b: pressing `<C-x>` returns
        // `Action::AbsorbPartialChord(<C-x>)` instead of
        // `Action::SetPending(Pending::AfterCtrlX)`. The trie's
        // `Partial` result drives the App's `partial_chord`
        // stack; the next keystroke runs through `dispatch_insert`
        // with that prefix and resolves `<C-x><C-o>` /
        // `<C-x><C-s>`.
        let h = populated_handle();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &[],
        );
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
        let h = populated_handle();
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &[],
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.completion_next),
            other => panic!("expected Invoke(completion_next), got {other:?}"),
        }
    }

    #[test]
    fn popup_down_arrow_navigates_next() {
        let h = populated_handle_with_popup();
        let a = shared_actions();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Down, KeyModifiers::NONE),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Enter, KeyModifiers::NONE),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('e'), KeyModifiers::CONTROL),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
            &[],
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.completion_accept_then_insert);
                assert!(matches!(inv.args, Args::Char('a')));
            }
            other => panic!("expected Invoke(completion_accept_then_insert, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn popup_ctrl_x_falls_through_to_base_insert_partial_chord() {
        // Slice 8.i.4.b: the popup layer doesn't bind <C-x>;
        // lookup falls through to base Insert which has it as a
        // partial node, returning `AbsorbPartialChord(<C-x>)`.
        let h = populated_handle_with_popup();
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::BackTab, KeyModifiers::NONE),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Tab, KeyModifiers::SHIFT),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::Char('z'), KeyModifiers::NONE),
            &[],
        );
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
        let r = dispatch_insert(
            &h,
            &ev(KeyCode::BackTab, KeyModifiers::NONE),
            &[],
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.snippet_prev_placeholder)
            }
            other => panic!("expected Invoke(snippet_prev_placeholder), got {other:?}"),
        }
    }
}
