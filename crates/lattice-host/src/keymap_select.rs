//! Select-mode dispatch (SN.3d.1).
//!
//! Select mode (`ModalState::Select(VisualKind)`) is Visual's sibling:
//! the same selection *geometry*, inverted *typing* semantics. A bare
//! printable key **replaces the whole selection with that char and
//! drops into Insert** ([`Action::SelectOvertype`]); motions and
//! text-objects extend the selection exactly as in Visual. See
//! `docs/dev/architecture/select-mode.md`.
//!
//! ## Why this is genuinely new dispatch, not "`dispatch_visual` + a flag"
//!
//! [`crate::keymap_visual::dispatch_visual`] has **no** printable
//! fallthrough — an unbound printable in Visual is a no-op. The defining
//! Select behaviour is exactly that fallthrough: an unbound printable
//! overtypes. The reference for the fallthrough is
//! [`crate::keymap_insert`]'s `literal_text_fallback` (CTRL → `None`,
//! `Char(c)` → an edit), mapped here to the replace-and-insert edit
//! (select-mode.md §3) rather than a plain insert.
//!
//! ## Dispatch order
//!
//! 1. **Mode-control chords** (fire regardless of the binding table):
//!    `<Esc>` → [`Action::ExitSelect`]; `<C-g>` →
//!    [`Action::ToggleVisualSelect`] (toggle back to Visual, selection
//!    preserved); `<C-o>` → one-shot Normal — *recognised but post-MVP*
//!    per select-mode.md §3, swallowed (`Action::None`) so a stray
//!    `<C-o>` never overtypes a literal char.
//! 2. **Any other CONTROL-bearing chord** → `Action::None` (mirrors
//!    `dispatch_visual`'s CONTROL short-circuit).
//! 3. **Mid-sequence** (a text-object prefix `i` / `a` already absorbed
//!    into `partial_chord`) → resolve `[partial..., chord]` against the
//!    `BindingMode::Select` table — the same partial-chord machinery
//!    Normal/Visual use.
//! 4. **Fresh chord** → `BindingMode::Select` lookup. `Bound` →
//!    its action (motion extends / exit); `Partial` → absorb;
//!    `Unbound` → the printable overtype fallthrough.
//!
//! The `BindingMode::Select` chord table itself (motions, text-objects,
//! exits) is registered in SN.3d.2 (`register_select_bindings`, guarded
//! by a Visual≡Select parity test). Until then every lookup here is
//! `Unbound`, so a fresh printable overtypes and the control chords
//! work — which is exactly d.1's testable surface.

use lattice_grammar::VisualKind;

use crate::action::Action;
use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::LookupResult;

/// Dispatch a Select-mode key event. See the module docs for the
/// ordering contract. `partial_chord` is the host's running multi-key
/// prefix (empty on a fresh chord; holds an absorbed `[i]` / `[a]`
/// mid-text-object), identical to the Visual path.
pub fn translate_select(
    handle: &KeymapHandle,
    chord: &KeyChord,
    _kind: VisualKind,
    partial_chord: &[KeyChord],
) -> Action {
    // 1. Mode-control chords. `<Esc>` exits to Normal even mid-
    //    text-object (abandons any absorbed prefix — there are no
    //    Select multi-key chords yet, so this is a no-op in practice).
    if matches!(chord.key, KeyKind::Special(SpecialKey::Esc)) {
        return Action::ExitSelect;
    }
    if chord.mods.ctrl() {
        match chord.key {
            // `<C-g>` is reserved in both Visual and Select for the
            // toggle (select-mode.md §4). One handler flips whichever
            // is active, preserving the selection geometry.
            KeyKind::Char('g') => return Action::ToggleVisualSelect,
            // `<C-o>` one-shot Normal — vim parity, post-MVP
            // (select-mode.md §3). Swallow so it never overtypes.
            KeyKind::Char('o') => return Action::None,
            // 2. Any other CONTROL-bearing chord is a no-op, exactly
            //    as `dispatch_visual` short-circuits CONTROL.
            _ => return Action::None,
        }
    }

    // 3. Mid-sequence text-object resolution against the Select table.
    if !partial_chord.is_empty() {
        let chord = normalize_for_select_lookup(*chord);
        let mut path: Vec<KeyChord> = partial_chord.to_vec();
        path.push(chord);
        return match handle.lookup(BindingMode::Select, &path) {
            LookupResult::Bound { command, captured } => {
                crate::keymap_normal::action_from_bound_with_capture(&command, &captured)
            }
            LookupResult::Partial => Action::AbsorbPartialChord(chord),
            LookupResult::Unbound => Action::None,
        };
    }

    // 4. Fresh chord. A bound motion / exit / text-object prefix wins;
    //    an UNBOUND printable falls through to overtype.
    let looked_up = normalize_for_select_lookup(*chord);
    match handle.lookup(BindingMode::Select, &[looked_up]) {
        LookupResult::Bound { command, captured } => {
            crate::keymap_normal::action_from_bound_with_capture(&command, &captured)
        }
        LookupResult::Partial => Action::AbsorbPartialChord(looked_up),
        LookupResult::Unbound => printable_overtype_fallback(chord),
    }
}

/// The Select fallthrough: a bare printable overtypes the selection.
/// Mirrors [`crate::keymap_insert`]'s `literal_text_fallback`, but maps
/// `Char(c)` to [`Action::SelectOvertype`] (replace-and-insert) instead
/// of a plain insert. CONTROL was already filtered by the caller.
fn printable_overtype_fallback(chord: &KeyChord) -> Action {
    match chord.key {
        KeyKind::Char(c) => Action::SelectOvertype(c),
        _ => Action::None,
    }
}

/// Strip SHIFT / ALT / SUPER for the Select trie lookup — same
/// treatment as the Visual path (`keymap_visual::normalize_for_visual_lookup`):
/// the catalog binds bare chords only; CONTROL is filtered by the
/// caller before this runs.
fn normalize_for_select_lookup(chord: KeyChord) -> KeyChord {
    KeyChord {
        key: chord.key,
        mods: chord
            .mods
            .without(KeyMods::SHIFT)
            .without(KeyMods::ALT)
            .without(KeyMods::SUPER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_handle() -> KeymapHandle {
        // d.1 runs against an EMPTY Select table (register_select_bindings
        // lands in d.2). Every lookup is therefore `Unbound`, so a fresh
        // printable overtypes and the control chords work — d.1's surface.
        KeymapHandle::new()
    }

    // `Action` derives only `Debug, Clone` (no `PartialEq`), so the
    // assertions match on the variant rather than `assert_eq!`.

    #[test]
    fn bare_printable_overtypes() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::char('x'), VisualKind::Charwise, &[]),
            Action::SelectOvertype('x')
        ));
        // A letter that is a Visual *operator* (`d`) still overtypes in
        // Select — operators are NOT registered in the Select table, so
        // it falls through. This is the inverted-semantics core.
        assert!(matches!(
            translate_select(&h, &KeyChord::char('d'), VisualKind::Charwise, &[]),
            Action::SelectOvertype('d')
        ));
    }

    #[test]
    fn esc_exits_select() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(
                &h,
                &KeyChord::special(SpecialKey::Esc),
                VisualKind::Linewise,
                &[]
            ),
            Action::ExitSelect
        ));
    }

    #[test]
    fn ctrl_g_toggles_to_visual() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::ctrl('g'), VisualKind::Charwise, &[]),
            Action::ToggleVisualSelect
        ));
    }

    #[test]
    fn ctrl_o_is_swallowed_post_mvp() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::ctrl('o'), VisualKind::Charwise, &[]),
            Action::None
        ));
    }

    #[test]
    fn other_control_chords_are_noops() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::ctrl('w'), VisualKind::Charwise, &[]),
            Action::None
        ));
    }

    #[test]
    fn special_keys_do_not_overtype() {
        let h = empty_handle();
        // A special (non-Char) key with no binding is a no-op, never a
        // spurious overtype.
        assert!(matches!(
            translate_select(
                &h,
                &KeyChord::special(SpecialKey::Tab),
                VisualKind::Charwise,
                &[]
            ),
            Action::None
        ));
    }
}
