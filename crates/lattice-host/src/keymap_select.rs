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

use lattice_grammar::SourceLocation;
use lattice_grammar::VisualKind;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_syntax::SyntaxMotionIds;
use lattice_syntax::SyntaxTextObjectIds;

use lattice_mode::mode::ModeId;

use crate::action::Action;
use crate::actions::ActionIds;
use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{ChordPattern, KeymapLayer, LookupResult};

/// Register the Select-mode chord table: the **motion / extension /
/// text-object** subset of Visual, under `BindingMode::Select`.
///
/// **Decision (LOCKED, select-mode.md §4): duplicate the registration**
/// (this parallels [`crate::keymap_visual::register_visual_bindings`])
/// **guarded by a parity test** — `visual_and_select_share_every_motion`
/// below — rather than a shared source list. The test is the drift
/// guard; it fails loudly the moment the two layers diverge, keeps each
/// registration readable on its own, and avoids a speculative shared-list
/// abstraction before a second consumer exists (heuristic #1).
///
/// What Select registers vs. Visual:
/// - **Motions** (`motion_rows`) — extend the selection, **identical** to
///   Visual. The shared `motion_rows` table means a motion added there
///   lights up in both; the parity test pins it.
/// - **Tree-sitter structural motions** (`syntax_motion_rows`, TSM.4) —
///   `]f`/`[f`/… extend the selection, **identical** to Visual, from the
///   same shared table; the parity test walks these too.
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
    syntax_motions: &SyntaxMotionIds,
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

    // Motions: the SHARED `keymap_normal::motion_rows` table — same one
    // Normal / operator-pending / Visual consume. The host's
    // `SelectionChange` arm extends the active selection's head, so a
    // motion extends the Select selection exactly as in Visual.
    for (chord, motion) in crate::keymap_normal::motion_rows(builtins) {
        handle.bind(
            layer,
            mode,
            std::slice::from_ref(&chord),
            CommandInvocation::of(motion.0),
            select_source(),
        );
    }

    // TSM.4: the sixteen tree-sitter structural motions
    // (`]f`/`[f`/`]F`/`[F`, `]c`/`[c`/`]C`/`[C`, `]a`/`[a`/`]A`/`[A`,
    // `]l`/`[l`/`]L`/`[L`). Bound identically to Visual from the SHARED
    // `keymap_normal::syntax_motion_rows` table — Select≡Visual motion
    // parity (select-mode.md §4) requires every Visual motion extend the
    // Select selection too. The parity test below
    // (`visual_and_select_share_every_motion`) walks this table as well.
    for (seq, motion) in crate::keymap_normal::syntax_motion_rows(syntax_motions) {
        handle.bind(
            layer,
            mode,
            &seq,
            CommandInvocation::of(motion.0),
            select_source(),
        );
    }

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
/// printable-overtype fallthrough. Used both as the normal path (when
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
    let looked_up = crate::keymap_insert::normalize_for_insert_lookup(*chord);
    let path: Vec<KeyChord> = if partial_chord.is_empty() {
        vec![looked_up]
    } else {
        let mut p = partial_chord.to_vec();
        p.push(looked_up);
        p
    };
    let LookupResult::Bound { command, captured } =
        handle.lookup_with_context(BindingMode::Select, &path, active_minor_modes)
    else {
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
    // TSM.4: Select binds the sixteen tree-sitter structural motions
    // (`syntax_motion_rows`) identically to Visual — the Select≡Visual
    // motion-parity contract (select-mode.md §4) requires it, and the
    // `visual_and_select_share_every_motion` parity test below now walks
    // `syntax_motion_rows` in addition to `motion_rows` to pin it.

    fn empty_handle() -> KeymapHandle {
        // The dispatch tests below run against an EMPTY Select table, so
        // every lookup is `Unbound` — a fresh printable overtypes and the
        // control chords fire. The parity test uses a fully POPULATED
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
        let syntax_motions: SyntaxMotionIds =
            lattice_syntax::register_syntax_motions(&mut registry);
        let h = KeymapHandle::new();
        crate::keymap_visual::register_visual_bindings(
            &h,
            &builtins,
            &action_ids,
            &syntax_textobjects,
            &syntax_motions,
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
        register_select_bindings(
            &h,
            &builtins,
            &action_ids,
            &syntax_textobjects,
            &syntax_motions,
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

    // ── SN.3d.2: Visual≡Select parity (the drift guard, select-mode.md §4) ──

    /// Every Visual MOTION binds to the same command in Select. This is
    /// the LOCKED drift guard for the duplicated registration: if a
    /// motion is added to `register_visual_bindings` but not
    /// `register_select_bindings` (or they bind different commands), this
    /// fails loudly. Covers BOTH the builtin `motion_rows` (single-key)
    /// AND the TSM.4 tree-sitter `syntax_motion_rows` (two-key `]f`/`[f`
    /// structural motions) — the latter closes the drift vector that let
    /// `]f` overtype the selection in Select while extending it in Visual.
    #[test]
    fn visual_and_select_share_every_motion() {
        use lattice_grammar::CommandRegistry;
        use lattice_grammar::builtins::populate as grammar_builtins_populate;
        // A throwaway registry yields the motion CHORD lists (the chords
        // are literal keys, registry-independent). The command identity
        // is compared WITHIN `populated_handle` (Visual vs Select), so
        // the two registries' differing CommandIds don't matter — the
        // drift guard is "Visual and Select agree", not a cross-registry
        // id match.
        let mut throwaway = CommandRegistry::new();
        let builtins = grammar_builtins_populate(&mut throwaway);
        let syntax_motions = lattice_syntax::register_syntax_motions(&mut throwaway);
        let h = populated_handle();

        // (a) builtin single-key motions.
        let mut checked = 0usize;
        for (chord, _motion) in crate::keymap_normal::motion_rows(&builtins) {
            let path = match chord {
                ChordPattern::Literal(c) => [c],
                _ => continue, // motion_rows is all literals today
            };
            let v = bound_command_id(&h, BindingMode::Visual, &path);
            let s = bound_command_id(&h, BindingMode::Select, &path);
            assert!(
                v.is_some(),
                "Visual must bind motion {:?} (test premise)",
                path[0]
            );
            assert_eq!(v, s, "Visual and Select disagree on motion {:?}", path[0]);
            checked += 1;
        }
        assert!(
            checked >= 20,
            "expected the full motion table, got {checked}"
        );

        // (b) TSM.4 tree-sitter structural motions (`]f`/`[f`/… two-key
        // sequences). Same drift guard: every one must resolve identically
        // in Visual and Select.
        let mut ts_checked = 0usize;
        for (seq, _motion) in crate::keymap_normal::syntax_motion_rows(&syntax_motions) {
            let path: Vec<KeyChord> = seq
                .iter()
                .map(|p| match p {
                    ChordPattern::Literal(c) => *c,
                    _ => panic!("syntax_motion_rows must be all literals"),
                })
                .collect();
            let v = bound_command_id(&h, BindingMode::Visual, &path);
            let s = bound_command_id(&h, BindingMode::Select, &path);
            assert!(
                v.is_some(),
                "Visual must bind syntax motion {path:?} (test premise)"
            );
            assert_eq!(v, s, "Visual and Select disagree on syntax motion {path:?}");
            ts_checked += 1;
        }
        assert_eq!(
            ts_checked, 16,
            "expected all 16 tree-sitter structural motions, got {ts_checked}"
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
