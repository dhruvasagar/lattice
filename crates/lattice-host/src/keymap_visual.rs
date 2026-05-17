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
//!
//! Slice 8.e's net win: every motion / operator binding moves
//! off the `legacy_action` bridge and onto a real
//! `CommandInvocation`. Only the three `ExitVisual` shapes plus
//! the two block-only `Enter*` paths still carry a `legacy_action`
//! -- they don't have a `CommandInvocation` peer today.

use std::sync::Arc;

use lattice_grammar::SourceLocation;
use lattice_grammar::VisualKind;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;

use crate::action::Action;
use crate::actions::ActionIds;
use crate::chord::{KeyChord, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, LookupResult};

/// Register every chord the legacy `input::translate_visual`
/// recognised into the supplied handle's `Builtin` layer under
/// `BindingMode::Visual`. Called at App startup; the registration
/// captures `builtins`'s motion / operator ids by value, so the
/// resulting `BoundCommand`s never re-resolve at lookup time.
///
/// Sources are tagged at this file + line so `:describe-key`
/// shows e.g.
/// `h -> motion:char-left  (builtin, keymap_visual.rs:NN)`.
pub fn register_visual_bindings(handle: &KeymapHandle, builtins: &Builtins, actions: &ActionIds) {
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

    // Motions: each chord binds to a typed CommandInvocation.
    // The dispatcher returns `Action::Invoke(command.clone())` --
    // identical to the legacy `invoke(builtins.char_left)`.
    let motion_table: &[(ChordPattern, lattice_grammar::registry::MotionId)] = &[
        (literal(KeyChord::char('h')), builtins.char_left),
        (
            literal(KeyChord::special(SpecialKey::Left)),
            builtins.char_left,
        ),
        (literal(KeyChord::char('j')), builtins.line_down),
        (
            literal(KeyChord::special(SpecialKey::Down)),
            builtins.line_down,
        ),
        (literal(KeyChord::char('k')), builtins.line_up),
        (literal(KeyChord::special(SpecialKey::Up)), builtins.line_up),
        (literal(KeyChord::char('l')), builtins.char_right),
        (
            literal(KeyChord::special(SpecialKey::Right)),
            builtins.char_right,
        ),
        (literal(KeyChord::char('0')), builtins.line_start),
        (
            literal(KeyChord::special(SpecialKey::Home)),
            builtins.line_start,
        ),
        (literal(KeyChord::char('$')), builtins.line_end),
        (
            literal(KeyChord::special(SpecialKey::End)),
            builtins.line_end,
        ),
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
/// 4. `Bound` -> `Action::Invoke(command.clone())`. The
///    dispatcher's `CommandKind::Action` branch routes the
///    invocation to the bound `ActionSpec`, which produces the
///    matching `Effect::AppAction(...)`.
/// 5. `Unbound` / `Partial` -> `Action::None`. Visual mode has
///    no multi-key chords today; `Partial` is reserved for a
///    user-config / plugin layer that registers one.
pub fn dispatch_visual(handle: &KeymapHandle, chord: &KeyChord, kind: VisualKind) -> Action {
    if chord.mods.ctrl() {
        return Action::None;
    }
    if matches!(kind, VisualKind::Blockwise) {
        match chord.key {
            crate::chord::KeyKind::Char('I') => return Action::EnterBlockVisualInsert,
            crate::chord::KeyKind::Char('A') => return Action::EnterBlockVisualAppend,
            _ => {}
        }
    }
    // Strip SHIFT/ALT/SUPER for the trie lookup -- Visual's catalog
    // binds bare chords only. CTRL was already filtered out above.
    let chord = KeyChord {
        key: chord.key,
        mods: chord
            .mods
            .without(crate::chord::KeyMods::SHIFT)
            .without(crate::chord::KeyMods::ALT)
            .without(crate::chord::KeyMods::SUPER),
    };
    match handle.lookup(BindingMode::Visual, &[chord]) {
        LookupResult::Bound { command, .. } => action_from_bound(&command),
        LookupResult::Partial | LookupResult::Unbound => Action::None,
    }
}

fn action_from_bound(bound: &Arc<BoundCommand>) -> Action {
    Action::Invoke(bound.command.clone())
}
