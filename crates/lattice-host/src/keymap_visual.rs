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
//! exit (see `docs/dev/architecture/keymap-architecture.md` §5.3); slice 8.e keeps
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
//! - **Text objects** (set the selection to the object's span):
//!   `i<obj>` / `a<obj>` for every object in the SHARED
//!   [`crate::keymap_normal::text_object_rows`] table -- `viw`,
//!   `vaw`, `vi{`, `vap`, `vaf` (function), `vac` (class),
//!   `vaC` (comment), ... These are TWO-key chords resolved by
//!   the same partial-chord machinery Normal mode uses (`i` / `a`
//!   absorbs into the host's `partial_chord`, the object char
//!   resolves the pair); see [`dispatch_visual`]. Each binds to a
//!   bare `CommandInvocation::of(tobj.0)`, which the grammar's
//!   `execute_text_object` turns into an `Effect::SelectionChange`
//!   spanning the object. There is ZERO per-object code: the
//!   binder iterates the shared table, so Visual and the Normal
//!   operator-pending resolver can never drift.
//!
//! Slice 8.e's net win: every motion / operator binding moves
//! off the `legacy_action` bridge and onto a real
//! `CommandInvocation`. Only the three `ExitVisual` shapes plus
//! the two block-only `Enter*` paths still carry a `legacy_action`
//! -- they don't have a `CommandInvocation` peer today.

use lattice_grammar::SourceLocation;
use lattice_grammar::VisualKind;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_syntax::SyntaxTextObjectIds;

use crate::action::Action;
use crate::actions::ActionIds;
use crate::chord::{KeyChord, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{ChordPattern, KeymapLayer, LookupResult};

/// Register every chord the legacy `input::translate_visual`
/// recognised into the supplied handle's `Builtin` layer under
/// `BindingMode::Visual`. Called at App startup; the registration
/// captures `builtins`'s motion / operator ids by value, so the
/// resulting `BoundCommand`s never re-resolve at lookup time.
///
/// Sources are tagged at this file + line so `:describe-key`
/// shows e.g.
/// `h -> motion:char-left  (builtin, keymap_visual.rs:NN)`.
pub fn register_visual_bindings(
    handle: &KeymapHandle,
    builtins: &Builtins,
    actions: &ActionIds,
    syntax_textobjects: &SyntaxTextObjectIds,
) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Visual;

    // Exits: <Esc>, v, V. Promoted to typed `ExitVisual` action
    // in slice 8.i.1.h.
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::special(SpecialKey::Esc))],
        CommandInvocation::of(actions.exit_visual),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char('v'))],
        CommandInvocation::of(actions.exit_visual),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char('V'))],
        CommandInvocation::of(actions.exit_visual),
        source(),
    );

    // `:` -- enter the command line from Visual (vim's `:'<,'>`). The
    // `EnterCommandLine` handler is Visual-aware: it captures the
    // selection into `last_visual` (so `'<` / `'>` and `Range::Selection`
    // resolve to it) and prefills the cmdline with `'<,'>`. Without this
    // binding `:` was unbound in Visual → `dispatch_visual` returned
    // `Action::None`, so you could not invoke ANY command (`:s`,
    // `:narrow`, `:w`, …) from a selection.
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char(':'))],
        CommandInvocation::of(actions.enter_command_line),
        source(),
    );

    // `o` -- swap the cursor to the other end of the selection so the
    // next motion / text object alters that end (vim's Visual `o`).
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord::char('o'))],
        CommandInvocation::of(actions.swap_visual_ends),
        source(),
    );

    // Motions: each chord binds to a typed CommandInvocation. Sourced
    // from the SHARED `keymap_normal::motion_rows` table -- the same
    // one Normal + operator-pending consume -- so a motion added there
    // works in Visual automatically (the host's `SelectionChange` arm
    // extends the active selection's head). The dispatcher returns
    // `Action::Invoke(command.clone())`.
    for (chord, motion) in crate::keymap_normal::motion_rows(builtins) {
        handle.bind(
            layer,
            mode,
            std::slice::from_ref(&chord),
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

    // Text objects: `i<obj>` (inner) / `a<obj>` (around) set the
    // selection to the object's span -- `viw`, `vaf`, `vaC`, `vi{`,
    // ... Rows come from the SHARED
    // [`crate::keymap_normal::text_object_rows`] table, the exact
    // same table the Normal-mode operator-pending resolver consumes,
    // so `viw` / `vaf` and `diw` / `daf` can never drift and there is
    // ZERO per-object code here. A bare text-object invocation
    // dispatches through the grammar's `execute_text_object`, which
    // returns `Effect::SelectionChange` spanning the object; the host
    // adopts both endpoints (see the `Effect::SelectionChange` arm in
    // `dispatch.rs`). The two-key chord (`[i, w]`) resolves via the
    // same partial-chord machinery Normal uses -- see `dispatch_visual`.
    for (chord_aliases, inner_id, around_id) in
        crate::keymap_normal::text_object_rows(builtins, syntax_textobjects)
    {
        for (prefix_char, tobj) in [('i', inner_id), ('a', around_id)] {
            for chord in &chord_aliases {
                handle.bind(
                    layer,
                    mode,
                    &[literal(KeyChord::char(prefix_char)), chord.clone()],
                    CommandInvocation::of(tobj.0),
                    source(),
                );
            }
        }
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

/// Strip SHIFT / ALT / SUPER for the Visual trie lookup -- the
/// catalog binds bare chords only (SHIFT on bare letters is already
/// folded into the case by `KeyChord::from_event`). CTRL is filtered
/// by the caller before this runs. Same treatment as Normal /
/// Replace mode -- legacy `translate_visual` matched on `event.code`
/// alone after the CONTROL guard, so non-CONTROL modifiers are
/// transparent.
fn normalize_for_visual_lookup(chord: KeyChord) -> KeyChord {
    KeyChord {
        key: chord.key,
        mods: chord
            .mods
            .without(KeyMods::SHIFT)
            .without(KeyMods::ALT)
            .without(KeyMods::SUPER),
    }
}

/// Dispatch a Visual-mode key event through the keymap registry.
///
/// 1. CONTROL-bearing key -> `Action::None`. (Legacy short-
///    circuited `CONTROL` and only `CONTROL`.)
/// 2. Mid-sequence (a text-object prefix `i` / `a` already in
///    `partial_chord`): resolve `[partial_chord..., chord]` against
///    the Visual catalog. This is the SAME partial-chord machinery
///    Normal mode uses -- `viw` is `v` then the two-key chord
///    `[i, w]`, `vaf` is `[a, f]`, etc. There is NO per-object code:
///    every text object in [`crate::keymap_normal::text_object_rows`]
///    works automatically, exactly as the user asked ("we shouldn't
///    have to do any custom handling of any text objects").
/// 3. Fresh keystroke. Blockwise overlay: `Char('I')` / `Char('A')`
///    go to `EnterBlockVisualInsert` / `EnterBlockVisualAppend`
///    before lookup. Charwise / linewise fall through.
/// 4. Single-chord lookup. `Bound` -> `Invoke` (folding any wildcard
///    capture); `Partial` -> `AbsorbPartialChord` (a bare `i` / `a`
///    starts a text object -- absorb it so the next key resolves the
///    pair); `Unbound` -> `Action::None`.
///
/// `partial_chord` is the host's running multi-key prefix (the same
/// `Editor::partial_chord` the Normal path threads); it is empty on a
/// fresh chord and holds the absorbed `[i]` / `[a]` mid-text-object.
pub fn dispatch_visual(
    handle: &KeymapHandle,
    chord: &KeyChord,
    kind: VisualKind,
    partial_chord: &[KeyChord],
) -> Action {
    // SN.3d.2: `<C-g>` toggles Visual → Select (reserved in both modes
    // for the toggle; select-mode.md §4). Must precede the CONTROL
    // short-circuit below — symmetric with `translate_select`, which
    // handles `<C-g>` as a hardcoded mode-control chord on the Select
    // side. `<C-g>` is otherwise unbound in Visual today.
    if chord.mods.ctrl() && matches!(chord.key, crate::chord::KeyKind::Char('g')) {
        return Action::ToggleVisualSelect;
    }
    if chord.mods.ctrl() {
        return Action::None;
    }

    // Mid-sequence: a text-object prefix was absorbed last keystroke.
    // Resolve the full path; the blockwise `I` / `A` overlay does not
    // apply here (it is a fresh-chord-only shortcut).
    if !partial_chord.is_empty() {
        let chord = normalize_for_visual_lookup(*chord);
        let mut path: Vec<KeyChord> = partial_chord.to_vec();
        path.push(chord);
        return match handle.lookup(BindingMode::Visual, &path) {
            LookupResult::Bound { command, captured } => {
                crate::keymap_normal::action_from_bound_with_capture(&command, &captured)
            }
            LookupResult::Partial => Action::AbsorbPartialChord(chord),
            LookupResult::Unbound => Action::None,
        };
    }

    if matches!(kind, VisualKind::Blockwise) {
        match chord.key {
            crate::chord::KeyKind::Char('I') => return Action::EnterBlockVisualInsert,
            crate::chord::KeyKind::Char('A') => return Action::EnterBlockVisualAppend,
            _ => {}
        }
    }
    let chord = normalize_for_visual_lookup(*chord);
    match handle.lookup(BindingMode::Visual, &[chord]) {
        LookupResult::Bound { command, captured } => {
            crate::keymap_normal::action_from_bound_with_capture(&command, &captured)
        }
        // A bare `i` / `a` is a text-object prefix: absorb it so the
        // next key resolves `[i / a, obj]`. (Before text objects
        // landed, Visual had no multi-key chords and this returned
        // `Action::None`.)
        LookupResult::Partial => Action::AbsorbPartialChord(chord),
        LookupResult::Unbound => Action::None,
    }
}
