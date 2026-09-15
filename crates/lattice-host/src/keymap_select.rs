//! Select-mode dispatch (SN.3d.1).
//!
//! Select mode (`ModalState::Select(VisualKind)`) is Visual's sibling:
//! the same selection *geometry*, inverted *typing* semantics. A bare
//! printable key **replaces the whole selection with that char and
//! drops into Insert** ([`Action::SelectOvertype`]); motions that can't be
//! typed extend the selection exactly as in Visual. See
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
//! 2. **Mid-sequence** (a text-object prefix `i` / `a` already absorbed
//!    into `partial_chord`) → resolve `[partial..., chord]` against the
//!    `BindingMode::Select` table — the same partial-chord machinery
//!    Normal/Visual use.
//! 3. **Fresh chord** → `BindingMode::Select` lookup. `Bound` →
//!    its action (motion extends / exit); `Partial` → absorb;
//!    `Unbound` → the overtype fallthrough.
//!
//! A bound key wins over the fallthrough, so what the Select table binds
//! decides what can be typed. The table holds only keys that can't be:
//! the keymap's motion mirror (VM.4) admits a motion only when its first
//! chord wouldn't overtype, using [`lattice_keymap::overtypes_in_select`],
//! the same predicate the fallthrough calls. `register_select_bindings`
//! still binds `o` and the `i` / `a` text-object prefixes, which take
//! those three letters until VM.5 removes them.

use lattice_grammar::SourceLocation;
use lattice_grammar::VisualKind;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_syntax::SyntaxTextObjectIds;

use lattice_mode::mode::ModeId;

use crate::action::Action;
use crate::actions::ActionIds;
use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{ChordPattern, KeymapLayer, LookupResult};

/// Register the Select-mode chord table's explicit rows, under
/// `BindingMode::Select`.
///
/// What Select holds vs. Visual:
/// - **Motions** — not listed here. The keymap mirrors every Normal motion
///   whose first chord can't be typed (arrows, Home/End, PageUp/PageDown,
///   `<C-d>` / `<C-u>`) into Select at the write (VM.4,
///   keymap-architecture.md §15). A printable motion (`w`, `f{char}`, `]f`)
///   is Visual-only, because in Select that key overtypes.
/// - **`o`** — swap selection ends (same as Visual).
/// - **Text objects** (`text_object_rows`) — set the selection span,
///   identical to Visual.
/// - **NO operators** (`d` / `x` / `c` / `s` / `y` / `>` / `<`). In
///   Select a printable overtypes (`translate_select`'s fallthrough), so
///   binding operators would shadow the defining behaviour. The parity
///   test asserts these resolve in Visual but stay UNBOUND in Select.
/// - **NO exits.** `<Esc>` / `<C-g>` are hardcoded mode-control chords in
///   [`translate_select`], not table entries (`v` / `V` are printables
///   that overtype in Select, so they cannot be exit bindings).
pub fn register_select_bindings(
    handle: &KeymapHandle,
    builtins: &Builtins,
    actions: &ActionIds,
    syntax_textobjects: &SyntaxTextObjectIds,
) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Select;

    // `o` — swap to the other end of the selection (vim Visual `o`).
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char('o'))],
        CommandInvocation::of(actions.swap_visual_ends),
        select_source(),
    );

    // Motions are not listed here. Since VM.4 the keymap mirrors them into
    // Select at every write, and only the ones whose first chord can't be
    // typed (`lattice_keymap::overtypes_in_select`), so Select's table and
    // Select's typing are decided by one predicate instead of two lists.

    // Text objects: `i<obj>` / `a<obj>` set the selection to the object's
    // span — same SHARED `text_object_rows` table Visual + the Normal
    // operator-pending resolver consume, so `viw` / `gh`-then-`iw` can
    // never drift. ZERO per-object code.
    for (chord_aliases, inner_id, around_id) in
        crate::keymap_normal::text_object_rows(builtins, syntax_textobjects)
    {
        for (prefix_char, tobj) in [('i', inner_id), ('a', around_id)] {
            for chord in &chord_aliases {
                handle.bind(
                    layer,
                    mode,
                    &[
                        ChordPattern::Literal(KeyChord::char(prefix_char)),
                        chord.clone(),
                    ],
                    CommandInvocation::of(tobj.0),
                    select_source(),
                );
            }
        }
    }
}

fn select_source() -> SourceLocation {
    SourceLocation::builtin_file(file!(), line!())
}

/// Dispatch a Select-mode key event. See the module docs for the
/// ordering contract. `partial_chord` is the host's running multi-key
/// prefix (empty on a fresh chord; holds an absorbed `[i]` / `[a]`
/// mid-text-object), identical to the Visual path.
pub fn translate_select(
    handle: &KeymapHandle,
    chord: &KeyChord,
    _kind: VisualKind,
    partial_chord: &[KeyChord],
    active_minor_modes: &[ModeId],
) -> Action {
    // 0. SN.3d.4: active minor-mode bindings own the chord first —
    //    the same `KeymapLayer::MinorMode` consultation Insert mode
    //    does (`dispatch_insert`), now wired for Select. A snippet
    //    placeholder focused in Select keeps `<Tab>` / `<S-Tab>`
    //    (navigate, keeping the default) and `<Esc>` (leave the
    //    snippet — a `fall_through` binding that then runs the native
    //    `<Esc>` = `ExitSelect`) live. Without this, those bindings
    //    were dead the moment a default-bearing placeholder selected,
    //    because Select dispatch never consulted minor layers and its
    //    `<Esc>` was hardcoded below. We intercept ONLY a winner that
    //    lives on a minor layer; a base-table `Bound` is a motion /
    //    text-object that `native_select_action` resolves.
    if let Some(action) = minor_select_action(handle, chord, partial_chord, active_minor_modes) {
        return action;
    }
    native_select_action(handle, chord, partial_chord)
}

/// SN.3d.4: the native (minor-free) Select dispatch — the original
/// `translate_select` body. Resolves the hardcoded mode-control chords,
/// the base `BindingMode::Select` motion / text-object table, and the
/// overtype fallthrough. Used both as the normal path (when
/// no active minor binding claims the chord) AND as the `fall_through`
/// continuation for a minor `<Esc>` (mode action THEN `ExitSelect`).
/// Being minor-free, it cannot re-enter `minor_select_action`, so the
/// fall-through never loops or fires the mode action twice.
fn native_select_action(
    handle: &KeymapHandle,
    chord: &KeyChord,
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
            // CG.1 (2026-08-07): the `_ => return Action::None` catch-all
            // that used to close this match is GONE, mirroring the same
            // removal in `dispatch_visual` — see the comment there for
            // why (it made every CTRL binding in this mode, including
            // any a plugin registers over WIT, structurally unreachable).
            //
            // The two arms above stay hardcoded because both are mode
            // *control*, not command lookup. Everything else falls
            // through to the trie. A bare CTRL chord with no binding
            // still ends at `Action::None` — via the lookup below, which
            // is the difference that matters.
            _ => {}
        }
    }

    // 2. Mid-sequence text-object resolution against the Select table.
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

    // 3. Fresh chord. A bound motion / exit / text-object prefix wins;
    //    an UNBOUND key that overtypes falls through to overtype.
    let looked_up = normalize_for_select_lookup(*chord);
    match handle.lookup(BindingMode::Select, &[looked_up]) {
        LookupResult::Bound { command, captured } => {
            crate::keymap_normal::action_from_bound_with_capture(&command, &captured)
        }
        LookupResult::Partial => Action::AbsorbPartialChord(looked_up),
        LookupResult::Unbound => printable_overtype_fallback(chord),
    }
}

/// SN.3d.4: resolve an active minor-mode binding for the chord in
/// Select mode, or `None` to defer to `native_select_action`.
///
/// Mirrors `dispatch_insert`'s minor-layer consultation: look the chord
/// up WITH the active minor set, but act only when the winner lives on
/// a `KeymapLayer::MinorMode` layer — a `Bound` on the base Select
/// table is a motion / text-object the native path owns. A
/// `fall_through` minor binding (the snippet `<Esc>`) runs its mode
/// action and then chains the native Select action for the same chord.
fn minor_select_action(
    handle: &KeymapHandle,
    chord: &KeyChord,
    partial_chord: &[KeyChord],
    active_minor_modes: &[ModeId],
) -> Option<Action> {
    if active_minor_modes.is_empty() {
        return None;
    }
    // Minor bindings are keyed like Insert's (keep CTRL + SHIFT) so
    // `<S-Tab>` stays distinct from `<Tab>`; the base-Select normalize
    // strips SHIFT and would collapse the two.
    //
    // OS.0b: raw-then-fallback, same as `dispatch_insert` — try the
    // chord AS PRESSED first so a mode that deliberately binds an
    // ALT/SUPER-bearing chord in Select is reachable, falling back to
    // the normalized form only when the raw lookup finds nothing.
    let lookup = crate::keymap_insert::lookup_insert_chord(
        handle,
        BindingMode::Select,
        partial_chord,
        *chord,
        active_minor_modes,
    );
    let LookupResult::Bound { command, captured } = lookup.result else {
        return None;
    };
    // Only a minor-layer winner is mode-owned; a base-table `Bound`
    // defers to `native_select_action`.
    if !matches!(command.layer, KeymapLayer::MinorMode(_)) {
        return None;
    }
    let action = crate::keymap_normal::action_from_bound_with_capture(&command, &captured);
    if !command.fall_through {
        return Some(action);
    }
    // `fall_through`: mode action, then the NATIVE continuation for the
    // chord (`<Esc>` → `ExitSelect`). Native is minor-free, so no loop.
    Some(crate::keymap_insert::chain_actions(
        action,
        native_select_action(handle, chord, partial_chord),
    ))
}

/// The Select fallthrough: a key that overtypes replaces the selection.
/// Mirrors [`crate::keymap_insert`]'s `literal_text_fallback`, but maps
/// the key to [`Action::SelectOvertype`] (replace-and-insert) instead of a
/// plain insert.
///
/// Which keys overtype is [`lattice_keymap::overtypes_in_select`], the ONE
/// definition the keymap's motion mirror also calls (VM.4), so the Select
/// table and Select typing can't disagree about it. Vim's rule: "Printable
/// characters, <NL> and <CR> cause the selection to be deleted, and Vim
/// enters Insert mode." Both `<CR>` and `<NL>` (Ctrl-J) type a newline.
fn printable_overtype_fallback(chord: &KeyChord) -> Action {
    // CG.1: the modifier check lives in the predicate, not in the caller.
    // `<C-w>` is a chord, not typing, and must never replace the user's
    // selection with a `w`. That used to be a blanket `return Action::None`
    // for every CTRL chord at the top of `native_select_action`, which also
    // made the Select trie unreachable for CTRL bindings (see the comment
    // there). Checking here keeps the guarantee and lets a real binding win
    // first — same shape as Replace mode, whose wildcard only matches bare
    // printable chars.
    if !lattice_keymap::overtypes_in_select(chord) {
        return Action::None;
    }
    match chord.key {
        KeyKind::Special(SpecialKey::Enter) => Action::SelectOvertype('\n'),
        // `<NL>`: the predicate admits `j` with Ctrl only as Ctrl-J.
        KeyKind::Char('j') if chord.mods.ctrl() => Action::SelectOvertype('\n'),
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
        // The dispatch tests below run against an EMPTY Select table, so
        // every lookup is `Unbound` — a fresh printable overtypes and the
        // control chords fire. The parity tests use a fully POPULATED
        // handle (`populated_handle`).
        KeymapHandle::new()
    }

    /// Build a handle with BOTH the Visual and Select tables registered
    /// from a real, populated command registry — the same path boot
    /// takes (`editor_boot.rs`).
    fn populated_handle() -> KeymapHandle {
        use lattice_grammar::CommandRegistry;
        use lattice_grammar::builtins::populate as grammar_builtins_populate;
        let mut registry = CommandRegistry::new();
        let builtins = grammar_builtins_populate(&mut registry);
        let action_ids = crate::actions::populate(&mut registry, &builtins);
        let syntax_textobjects = lattice_syntax::register_syntax_text_objects(&mut registry);
        let syntax_motions = lattice_syntax::register_syntax_motions(&mut registry);
        let registry = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(registry));
        let h = KeymapHandle::new();
        // VM.4: as boot does. With a registry the keymap mirrors motions into
        // Visual and Select at every write. Without one, Select would hold no
        // motions and the sweep below would pass vacuously.
        h.set_command_registry(registry.clone());
        crate::keymap_visual::register_visual_bindings(
            &h,
            &builtins,
            &action_ids,
            &syntax_textobjects,
        );
        // Operators bind into Visual via `register_operator_bindings` (called by
        // `register_normal_bindings`), not `register_visual_bindings` --
        // an operator acts on the selection by design. The parity test
        // below (`operators_bind_in_visual_but_never_in_select`) reads
        // those Visual operator binds, so the full Normal catalog must
        // be registered here too.
        crate::keymap_normal::register_normal_bindings(
            &h,
            &builtins,
            &action_ids,
            &syntax_textobjects,
            &syntax_motions,
        );
        register_select_bindings(&h, &builtins, &action_ids, &syntax_textobjects);
        // The operator-pending rows, as boot adds them.
        crate::keymap_normal::expand_grammar_rows(
            &h,
            &registry.load(),
            &builtins,
            KeymapLayer::Builtin,
        );
        h
    }

    fn bound_command_id(
        h: &KeymapHandle,
        mode: BindingMode,
        chords: &[KeyChord],
    ) -> Option<lattice_protocol::ids::CommandId> {
        match h.lookup(mode, chords) {
            LookupResult::Bound { command, .. } => Some(command.command.command),
            _ => None,
        }
    }

    // `Action` derives only `Debug, Clone` (no `PartialEq`), so the
    // assertions match on the variant rather than `assert_eq!`.

    #[test]
    fn bare_printable_overtypes() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::char('x'), VisualKind::Charwise, &[], &[]),
            Action::SelectOvertype('x')
        ));
        // A letter that is a Visual *operator* (`d`) still overtypes in
        // Select — operators are NOT registered in the Select table, so
        // it falls through. This is the inverted-semantics core.
        assert!(matches!(
            translate_select(&h, &KeyChord::char('d'), VisualKind::Charwise, &[], &[]),
            Action::SelectOvertype('d')
        ));
    }

    /// Vim's Select rule names `<NL>` and `<CR>` alongside printables. Both
    /// type a newline over the selection. A modified `<CR>` is a chord.
    #[test]
    fn enter_and_ctrl_j_overtype_with_a_newline() {
        let h = empty_handle();
        for (label, chord) in [
            ("<CR>", KeyChord::special(SpecialKey::Enter)),
            ("<C-j>", KeyChord::ctrl('j')),
        ] {
            assert!(
                matches!(
                    translate_select(&h, &chord, VisualKind::Charwise, &[], &[]),
                    Action::SelectOvertype('\n')
                ),
                "{label} must overtype with a newline"
            );
        }
        let ctrl_enter = KeyChord::new(KeyKind::Special(SpecialKey::Enter), KeyMods::CTRL);
        assert!(matches!(
            translate_select(&h, &ctrl_enter, VisualKind::Charwise, &[], &[]),
            Action::None
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
                &[],
                &[]
            ),
            Action::ExitSelect
        ));
    }

    #[test]
    fn ctrl_g_toggles_to_visual() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::ctrl('g'), VisualKind::Charwise, &[], &[]),
            Action::ToggleVisualSelect
        ));
    }

    #[test]
    fn ctrl_o_is_swallowed_post_mvp() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::ctrl('o'), VisualKind::Charwise, &[], &[]),
            Action::None
        ));
    }

    #[test]
    fn other_control_chords_are_noops() {
        let h = empty_handle();
        assert!(matches!(
            translate_select(&h, &KeyChord::ctrl('w'), VisualKind::Charwise, &[], &[]),
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
                &[],
                &[]
            ),
            Action::None
        ));
    }

    // ── Visual / Select parity, as select-mode.md §4 now states it ──

    /// VM.4: Visual takes every motion; Select takes only the ones that can't
    /// be typed.
    ///
    /// This replaces `visual_and_select_share_every_motion`, which asserted
    /// the opposite for printable motions and so held a Select bug in place:
    /// a bound printable takes the keystroke before the overtype fallback
    /// runs. `motion_rows` mixes printables (`w`, `0`, `$`) with
    /// non-printables (arrows, Home, End), so one walk exercises both halves
    /// of the rule. The tree-sitter structural motions (`]f`, …) start with a
    /// printable and are Visual-only; the all-layers drift test in
    /// `tests/a_motion_is_live_in_visual.rs` covers them.
    #[test]
    fn select_takes_only_motions_that_cannot_be_typed() {
        use lattice_grammar::CommandRegistry;
        use lattice_grammar::builtins::populate as grammar_builtins_populate;
        // A throwaway registry yields the motion CHORD lists; the chords are
        // literal keys, independent of any registry's ids.
        let mut throwaway = CommandRegistry::new();
        let builtins = grammar_builtins_populate(&mut throwaway);
        let h = populated_handle();
        let (mut printable, mut non_printable) = (0usize, 0usize);
        for (chord, _motion) in crate::keymap_normal::motion_rows(&builtins) {
            let ChordPattern::Literal(c) = chord else {
                continue;
            };
            let path = [c];
            assert!(
                bound_command_id(&h, BindingMode::Visual, &path).is_some(),
                "Visual must bind motion {c:?}"
            );
            let in_select = bound_command_id(&h, BindingMode::Select, &path).is_some();
            if lattice_keymap::overtypes_in_select(&c) {
                printable += 1;
                assert!(
                    !in_select,
                    "printable motion {c:?} is bound in Select, so it would take typed text"
                );
            } else {
                non_printable += 1;
                assert!(
                    in_select,
                    "non-printable motion {c:?} must extend in Select"
                );
            }
        }
        assert!(
            printable >= 10 && non_printable >= 4,
            "test premise: both halves exercised ({printable} printable / {non_printable} not)"
        );
    }

    /// Every printable character overtypes a Select selection when run
    /// against the POPULATED table, as boot builds it.
    ///
    /// `bare_printable_overtypes` uses an EMPTY table, so it could never
    /// notice a printable being bound. `visual_and_select_share_every_motion`
    /// went further and ASSERTED the printable motions were bound in Select,
    /// which locked the bug in. This sweep is the replacement.
    ///
    /// `NOT_YET` is the explicit `o` / `i` / `a` bindings that
    /// `register_select_bindings` still makes. VM.5 removes those and deletes
    /// this list; until then it names exactly what is left.
    #[test]
    fn every_printable_overtypes_against_the_populated_table() {
        const NOT_YET: &[char] = &['a', 'i', 'o'];
        let h = populated_handle();
        assert!(
            bound_command_id(
                &h,
                BindingMode::Select,
                &[KeyChord::special(SpecialKey::PageDown)]
            )
            .is_some(),
            "test premise: the mirror populated Select, so an empty table isn't passing this"
        );

        let mut stolen = Vec::new();
        for c in (' '..='~').filter(|c| !NOT_YET.contains(c)) {
            match translate_select(&h, &KeyChord::char(c), VisualKind::Charwise, &[], &[]) {
                Action::SelectOvertype(got) if got == c => {}
                other => stolen.push(format!("{c:?} -> {other:?}")),
            }
        }
        for (label, chord) in [
            ("<CR>", KeyChord::special(SpecialKey::Enter)),
            ("<C-j>", KeyChord::ctrl('j')),
        ] {
            match translate_select(&h, &chord, VisualKind::Charwise, &[], &[]) {
                Action::SelectOvertype('\n') => {}
                other => stolen.push(format!("{label} -> {other:?}")),
            }
        }
        assert!(
            stolen.is_empty(),
            "keys that don't overtype in Select:\n{}",
            stolen.join("\n")
        );
    }

    /// `o` (swap ends) is present in both modes.
    #[test]
    fn visual_and_select_share_swap_ends() {
        let h = populated_handle();
        let o = [KeyChord::char('o')];
        assert!(bound_command_id(&h, BindingMode::Visual, &o).is_some());
        assert_eq!(
            bound_command_id(&h, BindingMode::Select, &o),
            bound_command_id(&h, BindingMode::Visual, &o),
            "`o` must swap ends identically in Visual and Select"
        );
    }

    /// Text objects parity: `iw` resolves to the same command in both
    /// (representative of the shared `text_object_rows` table).
    #[test]
    fn visual_and_select_share_text_objects() {
        let h = populated_handle();
        let iw = [KeyChord::char('i'), KeyChord::char('w')];
        let v = bound_command_id(&h, BindingMode::Visual, &iw);
        let s = bound_command_id(&h, BindingMode::Select, &iw);
        assert!(v.is_some(), "Visual must bind `iw`");
        assert_eq!(v, s, "Select `iw` must match Visual `iw`");
    }

    /// **Operators are Visual-ONLY.** In Select a printable overtypes, so
    /// `d` / `x` / `c` / `s` / `y` / `>` / `<` must stay UNBOUND in the
    /// Select table — the dispatcher's fallthrough turns them into
    /// overtypes. This pins the inverted-semantics contract.
    #[test]
    fn operators_bind_in_visual_but_never_in_select() {
        let h = populated_handle();
        for op in ['d', 'x', 'c', 's', 'y', '>', '<'] {
            let path = [KeyChord::char(op)];
            assert!(
                bound_command_id(&h, BindingMode::Visual, &path).is_some(),
                "Visual must bind operator `{op}`"
            );
            assert_eq!(
                bound_command_id(&h, BindingMode::Select, &path),
                None,
                "Select must NOT bind operator `{op}` — it overtypes instead"
            );
        }
    }
}
