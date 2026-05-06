//! Normal-mode binding registration + lookup helper.
//!
//! Audit slice 8.g (Normal mode) is sub-sliced; this module
//! handles the 8.g.i deliverable -- the **simple single-key
//! bindings** that don't depend on App-side state (counts,
//! pending prefixes, macro recording, operator-pending). The
//! remaining sub-slices migrate the rest:
//!
//! - 8.g.ii -- `g_` / `z_` family.
//! - 8.g.iii -- operator-pending -> motion / text-object trie
//!   expansion.
//! - 8.g.iv -- count accumulator (stays input-side; attaches to
//!   the resolved invocation).
//! - 8.g.v -- mark / register / find-char wildcards.
//! - 8.g.vi -- `<C-w>` window-management sub-tree.
//!
//! ## What 8.g.i registers
//!
//! - **Motions** (with `<Left>` / `<Down>` / `<Up>` /
//!   `<Right>` / `<Home>` / `<End>` aliases): `h`, `j`, `k`,
//!   `l`, `0`, `$`, `^`, `w`, `b`, `e`, `W`, `B`, `E`, `}`,
//!   `{`, `)`, `(`, `G`. Each binds to
//!   `CommandInvocation::of(motion.0)` -- the dispatcher
//!   returns `Action::Invoke(command.clone())`.
//! - **Viewport jumps**: `H` / `M` / `L`.
//! - **Mode entry**: `i`, `a`, `o`, `O`, `:`, `v`, `V`, `R`.
//! - **Paste**: `p`, `P`.
//! - **Pseudo-operators with built-in target**: `D` (= `d$`),
//!   `C` (= `c$`), `S` (= `cc`), `Y` (= `yy`), `x` (= delete
//!   one char right). These bind to the typed
//!   `CommandInvocation` directly (no operator-pending state),
//!   so they stay single-chord even though they conceptually
//!   compose an operator with a target.
//! - **Misc single-chord**: `J` (line-join), `;` / `,`
//!   (find-repeat), `~` (toggle case), `K` (LSP hover), `/` /
//!   `?` (enter search), `n` / `N` / `*` / `#` (search nav),
//!   `%` (match bracket), `u` (undo), `.` (dot-repeat), `-`
//!   (oil parent dir), `<Tab>` (jump-list forward), `<PageUp>`
//!   / `<PageDown>` (count-10 line-down/up).
//!
//! ## What 8.g.i leaves in `input::translate_normal`
//!
//! - Pending-state resolution (`AfterCtrlW`, `AfterG`,
//!   `AfterOperator`, `AfterFindChar`, `AfterTextObject`,
//!   `AfterZ`, `AfterSetMark`, `AfterJumpMark{Line,Exact}`,
//!   `AfterRegister`, `AfterMacroStart`, `AfterMacroPlay`).
//! - `<C-...>` chords (8.g.vi).
//! - Numeric prefix accumulator.
//! - Operator-leading single keys (`d` / `c` / `y` / `>` / `<`)
//!   -- they set `Pending::AfterOperator` (8.g.iii).
//! - Pending-prefix single keys (`g`, `z`, `q`, `@`, `"`, `m`,
//!   `'`, `` ` ``, `f`, `F`, `t`, `T`).
//!
//! `input::translate_normal` now starts with a `lookup_normal`
//! call against the registry; on `Some(action)` it returns
//! immediately, on `None` it falls through to the legacy
//! match arm. As subsequent sub-slices migrate more bindings,
//! the legacy match arm shrinks; 8.g.vi closes it out.
//!
//! ## Modifier transparency
//!
//! `dispatch_normal`'s `lookup_normal` strips `ALT` and `SUPER`
//! before lookup; `CTRL` and `SHIFT` are preserved.
//! `KeyChord::from_event` already strips redundant SHIFT on
//! bare letters, so `(Char('H'), NONE)` is the canonical chord
//! for both `H` and `<S-h>` -- the trie only needs one entry
//! per uppercase letter. CTRL-bearing chords are filtered by
//! the legacy CTRL guard above the lookup call site, so the
//! trie never sees them in slice 8.g.i (they migrate in
//! 8.g.vi together with the `<C-w>` sub-tree).

use std::sync::Arc;

use crossterm::event::{KeyEvent, KeyModifiers};
use lattice_grammar::SourceLocation;
use lattice_grammar::args::Args;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::{ModalState, SearchDirection, Target, VisualKind};

use crate::app::{Action, Pending, ScrollPos, ViewportPos};
use crate::chord::{KeyChord, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_replace::KeymapHandleLegacyExt;
use crate::keymap_trie::{
    BoundCommand, ChordPattern, KeymapLayer, LookupResult,
};

/// Register the slice 8.g.i Normal-mode catalog into the
/// supplied handle's `Builtin` layer. The legacy
/// `input::translate_normal` keeps its match arm for the
/// bindings not yet in this catalog.
pub fn register_normal_bindings(handle: &KeymapHandle, builtins: &Builtins) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Normal;

    // ---- Motions: typed CommandInvocation, no count baked in.
    let motion_table: &[(ChordPattern, lattice_grammar::registry::MotionId)] = &[
        (lit_char('h'), builtins.char_left),
        (lit_special(SpecialKey::Left), builtins.char_left),
        (lit_char('j'), builtins.line_down),
        (lit_special(SpecialKey::Down), builtins.line_down),
        (lit_char('k'), builtins.line_up),
        (lit_special(SpecialKey::Up), builtins.line_up),
        (lit_char('l'), builtins.char_right),
        (lit_special(SpecialKey::Right), builtins.char_right),
        (lit_char('0'), builtins.line_start),
        (lit_special(SpecialKey::Home), builtins.line_start),
        (lit_char('$'), builtins.line_end),
        (lit_special(SpecialKey::End), builtins.line_end),
        (lit_char('^'), builtins.first_non_blank),
        (lit_char('w'), builtins.word_forward),
        (lit_char('b'), builtins.word_backward),
        (lit_char('e'), builtins.word_end),
        (lit_char('W'), builtins.big_word_forward),
        (lit_char('B'), builtins.big_word_backward),
        (lit_char('E'), builtins.big_word_end),
        (lit_char('}'), builtins.paragraph_forward),
        (lit_char('{'), builtins.paragraph_backward),
        (lit_char(')'), builtins.sentence_forward),
        (lit_char('('), builtins.sentence_backward),
        (lit_char('G'), builtins.goto_last_line),
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

    // ---- Pseudo-operators: typed invocations with built-in
    // ---- targets / ranges, no follow-up motion needed.
    // `Y` -> yy (linewise yank).
    handle.bind(
        layer,
        mode,
        &[lit_char('Y')],
        CommandInvocation::of(builtins.yank.0)
            .with_range(lattice_grammar::Range::CurrentLine),
        source(),
    );
    // `x` -> delete one char to the right.
    handle.bind(
        layer,
        mode,
        &[lit_char('x')],
        CommandInvocation::of(builtins.delete.0)
            .with_target(Target::Motion(builtins.char_right, Args::None)),
        source(),
    );
    // `D` = `d$` (delete to end of line).
    handle.bind(
        layer,
        mode,
        &[lit_char('D')],
        CommandInvocation::of(builtins.delete.0)
            .with_target(Target::Motion(builtins.line_end, Args::None)),
        source(),
    );
    // `C` = `c$` (change to end of line).
    handle.bind(
        layer,
        mode,
        &[lit_char('C')],
        CommandInvocation::of(builtins.change.0)
            .with_target(Target::Motion(builtins.line_end, Args::None)),
        source(),
    );
    // `S` = `cc` (substitute line).
    handle.bind(
        layer,
        mode,
        &[lit_char('S')],
        CommandInvocation::of(builtins.change.0)
            .with_range(lattice_grammar::Range::CurrentLine),
        source(),
    );

    // ---- Legacy-action bindings (no `CommandInvocation` peer
    // ---- today; bridge stays until 8.i).

    // Viewport jumps.
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('H')],
        Action::JumpViewport(ViewportPos::Top),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('M')],
        Action::JumpViewport(ViewportPos::Middle),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('L')],
        Action::JumpViewport(ViewportPos::Bottom),
        source(),
    );

    // Paste.
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('p')],
        Action::PasteAfter,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('P')],
        Action::PasteBefore,
        source(),
    );

    // Mode entry.
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('i')],
        Action::EnterMode(ModalState::Insert),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('a')],
        Action::EnterAppend,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('o')],
        Action::OpenLineBelow,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('O')],
        Action::OpenLineAbove,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char(':')],
        Action::EnterCommandLine,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('v')],
        Action::EnterVisual(VisualKind::Charwise),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('V')],
        Action::EnterVisual(VisualKind::Linewise),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('R')],
        Action::EnterMode(ModalState::Replace),
        source(),
    );

    // Misc single-chord.
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('J')],
        Action::JoinLines { with_space: true },
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char(';')],
        Action::FindRepeat { reverse: false },
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char(',')],
        Action::FindRepeat { reverse: true },
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('~')],
        Action::ToggleCaseAtCursor,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('K')],
        Action::LspHoverRequest,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('/')],
        Action::EnterSearch(SearchDirection::Forward),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('?')],
        Action::EnterSearch(SearchDirection::Backward),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('n')],
        Action::SearchNext,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('N')],
        Action::SearchPrevious,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('*')],
        Action::SearchWordUnderCursor(SearchDirection::Forward),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('#')],
        Action::SearchWordUnderCursor(SearchDirection::Backward),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('%')],
        Action::MatchBracket,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('u')],
        Action::Undo,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('.')],
        Action::RepeatLastChange,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('-')],
        Action::OilNavigateUp,
        source(),
    );

    // Specials.
    handle.bind_legacy(
        layer,
        mode,
        &[lit_special(SpecialKey::Tab)],
        Action::JumpHistoryForward,
        source(),
    );
    // PageDown / PageUp -- count-10 line-down / line-up. These
    // bake the count into the invocation directly (no
    // pending-count interaction); legacy parity preserved.
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::PageDown)],
        CommandInvocation::of(builtins.line_down.0)
            .with_count(lattice_grammar::command::Count(10)),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::PageUp)],
        CommandInvocation::of(builtins.line_up.0)
            .with_count(lattice_grammar::command::Count(10)),
        source(),
    );

    // ---- Slice 8.g.ii: `g_` family.
    //
    // `[g]` itself stays a partial trie node -- no terminal
    // binding here -- so lookup of `[g]` returns
    // `LookupResult::Partial`, which `lookup_normal`
    // translates into `SetPending(Pending::AfterG)`. The
    // second keystroke arrives with `pending = AfterG`; the
    // App's `resolve_after_g` calls
    // `lookup_normal_two_key(handle, KeyChord::char('g'), event)`
    // to walk `[g, X]` against the same trie.
    let g = lit_char('g');

    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('g')],
        CommandInvocation::of(builtins.goto_first_line.0),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('U')],
        Action::SetPending(Pending::AfterOperator(builtins.upper)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('u')],
        Action::SetPending(Pending::AfterOperator(builtins.lower)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('~')],
        Action::SetPending(Pending::AfterOperator(builtins.toggle_case)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('v')],
        Action::ReselectLastVisual,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('J')],
        Action::JoinLines { with_space: false },
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char(';')],
        Action::WalkMarkHistoryBack,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char(',')],
        Action::WalkMarkHistoryForward,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('d')],
        Action::LspDefinitionRequest,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('D')],
        Action::LspDeclarationRequest,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('y')],
        Action::LspTypeDefinitionRequest,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('I')],
        Action::LspImplementationRequest,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[g.clone(), lit_char('r')],
        Action::LspReferencesRequest,
        source(),
    );

    // ---- Slice 8.g.ii: `z_` family.
    //
    // Same pattern: `[z]` is a partial trie node;
    // `lookup_normal` converts it to
    // `SetPending(Pending::AfterZ)`.
    let z = lit_char('z');

    // Center-cursor scrolls. `zz` and `z.` both center.
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('z')],
        Action::ScrollCursorTo(ScrollPos::Center),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('.')],
        Action::ScrollCursorTo(ScrollPos::Center),
        source(),
    );
    // Top-of-viewport scrolls. `zt` and `z<CR>` both align top.
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('t')],
        Action::ScrollCursorTo(ScrollPos::Top),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_special(SpecialKey::Enter)],
        Action::ScrollCursorTo(ScrollPos::Top),
        source(),
    );
    // Bottom-of-viewport scrolls. `zb` and `z-` both align bottom.
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('b')],
        Action::ScrollCursorTo(ScrollPos::Bottom),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('-')],
        Action::ScrollCursorTo(ScrollPos::Bottom),
        source(),
    );

    // Folds.
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('f')],
        Action::CreateFoldFromVisual,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('o')],
        Action::OpenFoldAtCursor,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('c')],
        Action::CloseFoldAtCursor,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('a')],
        Action::ToggleFoldAtCursor,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('R')],
        Action::OpenAllFolds,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('M')],
        Action::CloseAllFolds,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('d')],
        Action::DeleteFoldAtCursor,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('j')],
        Action::GotoNextFold,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z.clone(), lit_char('k')],
        Action::GotoPrevFold,
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[z, lit_char('i')],
        Action::ToggleFoldEnable,
        source(),
    );
}

/// Look up a single-key event in the Normal-mode catalog.
/// Returns `Some(action)` when the chord is bound; `None` to
/// signal the caller (`input::translate_normal`) to fall back to
/// its legacy match arm for the not-yet-migrated bindings.
///
/// `event` should arrive *after* the legacy CTRL guard, the
/// numeric prefix accumulator, and the recording-macro `q`
/// special-case in `translate_normal` -- this helper is the
/// last stop before the legacy match. CTRL-bearing chords are
/// passed through untouched (the trie has no CTRL bindings
/// yet -- they migrate in 8.g.vi).
///
/// Slice 8.g.ii: `g` and `z` are partial trie nodes (children
/// only, no terminal binding). `LookupResult::Partial` on those
/// chords surfaces here as `Some(Action::SetPending(AfterG /
/// AfterZ))` so the dispatcher arms the second-key resolver.
/// Other partial paths still return `None` (no caller produces
/// them today; future sub-slices can extend this match arm).
pub fn lookup_normal(handle: &KeymapHandle, event: &KeyEvent) -> Option<Action> {
    let Some(raw_chord) = KeyChord::from_event(event) else {
        return None;
    };
    let chord = normalize_for_normal_lookup(raw_chord);
    match handle.lookup(BindingMode::Normal, &[chord]) {
        LookupResult::Bound { command, .. } => Some(action_from_bound(&command)),
        LookupResult::Partial => {
            if chord == KeyChord::char('g') {
                Some(Action::SetPending(Pending::AfterG))
            } else if chord == KeyChord::char('z') {
                Some(Action::SetPending(Pending::AfterZ))
            } else {
                None
            }
        }
        LookupResult::Unbound => None,
    }
}

/// Resolve the second key of a `g_` / `z_` chord (and any
/// future Normal-mode two-key prefix) via the registry. The
/// caller supplies the prefix chord that armed the pending
/// state; this helper builds `[prefix, normalised(event)]` and
/// looks it up. `Bound` -> the bound action; everything else
/// (`Partial` / `Unbound`) -> `Action::SetPending(Pending::None)`
/// to drop the pending state, matching the legacy `_ =>
/// SetPending(None)` catchall in `resolve_after_g` /
/// `resolve_after_z`.
///
/// Slice 8.g.ii migrates `g_` / `z_` through this helper.
pub fn lookup_normal_two_key(
    handle: &KeymapHandle,
    prefix: KeyChord,
    event: &KeyEvent,
) -> Action {
    let Some(raw_chord) = KeyChord::from_event(event) else {
        return Action::SetPending(Pending::None);
    };
    let chord = normalize_for_normal_lookup(raw_chord);
    match handle.lookup(BindingMode::Normal, &[prefix, chord]) {
        LookupResult::Bound { command, .. } => action_from_bound(&command),
        LookupResult::Partial | LookupResult::Unbound => {
            Action::SetPending(Pending::None)
        }
    }
}

fn normalize_for_normal_lookup(chord: KeyChord) -> KeyChord {
    // Strip ALT and SUPER; preserve CTRL and SHIFT. Same
    // treatment as Insert mode -- legacy `translate_normal`
    // matched on `event.code` after the CTRL guard, so
    // non-CONTROL modifiers are transparent below the guard;
    // SHIFT on bare letters is already encoded in the case
    // by `KeyChord::from_event`.
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

fn action_from_bound(bound: &Arc<BoundCommand>) -> Action {
    match bound.legacy_action.as_ref() {
        Some(action) => action.clone(),
        None => Action::Invoke(bound.command.clone()),
    }
}

fn lit_char(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord::char(c))
}

fn lit_special(s: SpecialKey) -> ChordPattern {
    ChordPattern::Literal(KeyChord::special(s))
}

fn source() -> SourceLocation {
    SourceLocation::builtin_file(file!(), line!())
}

// Keep the `KeyModifiers` import live: callers use it and the
// drift test asserts modifier behaviour. No-op helper is the
// cheapest way to placate "unused import" without `#[allow]`.
#[allow(dead_code)]
fn _assert_modifiers_used(_m: KeyModifiers) {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crossterm::event::{KeyCode, KeyEventKind, KeyEventState};
    use lattice_grammar::CommandRegistry;

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
        let b = lattice_grammar::builtins::populate(&mut r);
        (r, b)
    }

    fn populated_handle() -> (KeymapHandle, Builtins) {
        let (_, b) = fixture();
        let h = KeymapHandle::new();
        register_normal_bindings(&h, &b);
        (h, b)
    }

    #[test]
    fn motion_h_invokes_char_left() {
        let (h, b) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('h'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn arrow_left_aliases_char_left() {
        let (h, b) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Left, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn upper_g_invokes_goto_last_line() {
        let (h, b) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('G'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.goto_last_line.0),
            other => panic!("expected Invoke(goto_last_line), got {other:?}"),
        }
    }

    #[test]
    fn pseudo_operator_x_carries_char_right_target() {
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
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
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('H'), KeyModifiers::NONE));
        assert!(matches!(r, Some(Action::JumpViewport(ViewportPos::Top))));
    }

    #[test]
    fn mode_entry_v_enters_charwise_visual() {
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::EnterVisual(VisualKind::Charwise))
        ));
    }

    #[test]
    fn paste_p_lower_pastes_after() {
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(matches!(r, Some(Action::PasteAfter)));
    }

    #[test]
    fn search_slash_enters_forward_search() {
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::EnterSearch(SearchDirection::Forward))
        ));
    }

    #[test]
    fn tab_jumps_history_forward() {
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Tab, KeyModifiers::NONE));
        assert!(matches!(r, Some(Action::JumpHistoryForward)));
    }

    #[test]
    fn page_down_invokes_line_down_with_count_ten() {
        let (h, b) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::PageDown, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.line_down.0);
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(10)));
            }
            other => panic!("expected Invoke(line_down, count=10), got {other:?}"),
        }
    }

    #[test]
    fn unmigrated_d_returns_none_for_legacy_fallthrough() {
        // `d` is operator-leading -- not in 8.g.i's catalog;
        // 8.g.iii migrates it. Lookup must return None so the
        // caller falls through to the legacy match arm.
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(r.is_none());
    }

    /// Slice 8.g.ii: `g` is a partial trie node (children only,
    /// no terminal binding). `lookup_normal` converts the
    /// `LookupResult::Partial` into `SetPending(AfterG)` so the
    /// dispatcher arms the second-key resolver.
    #[test]
    fn g_arms_after_g_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(matches!(r, Some(Action::SetPending(Pending::AfterG))));
    }

    #[test]
    fn z_arms_after_z_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(matches!(r, Some(Action::SetPending(Pending::AfterZ))));
    }

    #[test]
    fn gg_resolves_to_goto_first_line() {
        let (h, b) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('g'),
            &ev(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, b.goto_first_line.0),
            other => panic!("expected Invoke(goto_first_line), got {other:?}"),
        }
    }

    #[test]
    fn gd_resolves_to_lsp_definition_request() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('g'),
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::LspDefinitionRequest));
    }

    #[test]
    fn gu_arms_after_operator_pending_for_lower() {
        let (h, b) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('g'),
            &ev(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        match r {
            Action::SetPending(Pending::AfterOperator(op)) => {
                assert_eq!(op, b.lower);
            }
            other => panic!("expected SetPending(AfterOperator(lower)), got {other:?}"),
        }
    }

    #[test]
    fn g_capital_j_resolves_to_join_lines_without_space() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('g'),
            &ev(KeyCode::Char('J'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::JoinLines { with_space: false }));
    }

    #[test]
    fn zz_centers_cursor() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Center)));
    }

    #[test]
    fn z_dot_aliases_zz() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Char('.'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Center)));
    }

    #[test]
    fn z_enter_aliases_zt() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Top)));
    }

    #[test]
    fn z_dash_aliases_zb() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Char('-'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Bottom)));
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ToggleFoldAtCursor));
    }

    #[test]
    fn z_unrecognized_drops_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::SetPending(Pending::None)));
    }

    #[test]
    fn z_esc_drops_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal_two_key(
            &h,
            KeyChord::char('z'),
            &ev(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::SetPending(Pending::None)));
    }

    #[test]
    fn unmigrated_q_returns_none_for_legacy_fallthrough() {
        // `q` is macro-recording control (state-dependent).
        let (h, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(r.is_none());
    }

    /// `<S-h>`'s SHIFT is stripped by `KeyChord::from_event`
    /// for bare letters (case carries the bit), so the trie
    /// only needs `(Char('h'), NONE)`. Pin that here.
    #[test]
    fn shift_h_resolves_via_lowercase_chord() {
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('h'), KeyModifiers::ALT));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }
}
