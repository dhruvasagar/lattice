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

use crate::app::{Action, FindKind, Pending, ScrollPos, ViewportPos};
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
    // App's `Pending::AfterG` arm in `input::translate_normal`
    // calls `lookup_normal_with_prefix(handle, &[g_chord], event)`
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

    // ---- Slice 8.g.iii: operator-pending resolution.
    //
    // Each operator gets the same target / doubled / text-object /
    // find-char paths registered under its primary chord(s). The
    // five "single-chord" operators register under their own chord
    // (`[d]`, `[c]`, `[y]`, `[>]`, `[<]`) plus a terminal
    // `SetPending(AfterOperator(op))` at depth 1. The case
    // operators (`upper` / `lower` / `toggle_case`) ride under the
    // `g` prefix at depth 2 (`[g, U]`, `[g, u]`, `[g, ~]`); 8.g.ii
    // already wired their depth-2 terminal `SetPending` bindings,
    // so this slice just extends the path with depth-3 (motion /
    // doubled / text-object pending / find-char pending) and
    // depth-4 (text-object resolution) entries.
    register_operator_pending(
        handle,
        &[lit_char('d')],
        builtins.delete,
        ChordPattern::Literal(KeyChord::char('d')),
        builtins,
    );
    register_operator_pending(
        handle,
        &[lit_char('c')],
        builtins.change,
        ChordPattern::Literal(KeyChord::char('c')),
        builtins,
    );
    register_operator_pending(
        handle,
        &[lit_char('y')],
        builtins.yank,
        ChordPattern::Literal(KeyChord::char('y')),
        builtins,
    );
    register_operator_pending(
        handle,
        &[lit_char('>')],
        builtins.indent_right,
        ChordPattern::Literal(KeyChord::char('>')),
        builtins,
    );
    register_operator_pending(
        handle,
        &[lit_char('<')],
        builtins.indent_left,
        ChordPattern::Literal(KeyChord::char('<')),
        builtins,
    );
    // Case operators -- prefix is the two-key sequence registered
    // at slice 8.g.ii. Their doubled forms (`gUU` / `guu` / `g~~`)
    // operate on the current line.
    register_operator_pending(
        handle,
        &[lit_char('g'), lit_char('U')],
        builtins.upper,
        ChordPattern::Literal(KeyChord::char('U')),
        builtins,
    );
    register_operator_pending(
        handle,
        &[lit_char('g'), lit_char('u')],
        builtins.lower,
        ChordPattern::Literal(KeyChord::char('u')),
        builtins,
    );
    register_operator_pending(
        handle,
        &[lit_char('g'), lit_char('~')],
        builtins.toggle_case,
        ChordPattern::Literal(KeyChord::char('~')),
        builtins,
    );

    // ---- d/c/y/>/< as single-chord terminals that arm the
    // operator-pending state. `[g, U]` / `[g, u]` / `[g, ~]` were
    // already registered at slice 8.g.ii; their depth-2 binding
    // sets the same `SetPending(AfterOperator(...))` action.
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('d')],
        Action::SetPending(Pending::AfterOperator(builtins.delete)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('c')],
        Action::SetPending(Pending::AfterOperator(builtins.change)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('y')],
        Action::SetPending(Pending::AfterOperator(builtins.yank)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('>')],
        Action::SetPending(Pending::AfterOperator(builtins.indent_right)),
        source(),
    );
    handle.bind_legacy(
        layer,
        mode,
        &[lit_char('<')],
        Action::SetPending(Pending::AfterOperator(builtins.indent_left)),
        source(),
    );
}

/// Register the slice 8.g.iii operator-pending paths for one
/// operator under `op_prefix`. Same shape across every operator:
/// motion targets, the doubled-operator current-line shorthand,
/// `i_` / `a_` text-object pendings + their resolutions, and
/// the `f` / `F` / `t` / `T` find-char pendings (resolution stays
/// in legacy `resolve_after_find_char` until 8.g.v).
///
/// `doubled_self` is the chord that triggers the linewise form
/// (e.g. `'d'` for `dd`, `'U'` for `gUU`). It's the trailing key
/// of the doubled form, not the prefix.
fn register_operator_pending(
    handle: &KeymapHandle,
    op_prefix: &[ChordPattern],
    op: lattice_grammar::registry::OperatorId,
    doubled_self: ChordPattern,
    builtins: &Builtins,
) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Normal;

    // ---- Motion targets. Each operator's `[op_prefix..., motion_chord]`
    // ---- resolves to `Invoke(op, Target::Motion(motion))`.
    let motion_table: &[(ChordPattern, lattice_grammar::registry::MotionId)] = &[
        (lit_char('h'), builtins.char_left),
        (lit_special(SpecialKey::Left), builtins.char_left),
        (lit_char('l'), builtins.char_right),
        (lit_special(SpecialKey::Right), builtins.char_right),
        (lit_char('j'), builtins.line_down),
        (lit_special(SpecialKey::Down), builtins.line_down),
        (lit_char('k'), builtins.line_up),
        (lit_special(SpecialKey::Up), builtins.line_up),
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
    ];
    for (chord, motion) in motion_table {
        let mut path: Vec<ChordPattern> = op_prefix.to_vec();
        path.push(chord.clone());
        handle.bind(
            layer,
            mode,
            &path,
            CommandInvocation::of(op.0)
                .with_target(Target::Motion(*motion, Args::None)),
            source(),
        );
    }

    // ---- Doubled-operator -> `Range::CurrentLine`. `dd`, `cc`,
    // ---- `yy`, `>>`, `<<`, `gUU`, `guu`, `g~~`.
    {
        let mut path: Vec<ChordPattern> = op_prefix.to_vec();
        path.push(doubled_self);
        handle.bind(
            layer,
            mode,
            &path,
            CommandInvocation::of(op.0)
                .with_range(lattice_grammar::Range::CurrentLine),
            source(),
        );
    }

    // ---- Text-object pendings + resolutions. `[op, i]` /
    // ---- `[op, a]` arm `Pending::AfterTextObject`; `[op, i, X]`
    // ---- / `[op, a, X]` resolve to typed `Invoke(op,
    // ---- Target::TextObject(...))`.
    for around in [false, true] {
        let around_chord: ChordPattern = if around {
            lit_char('a')
        } else {
            lit_char('i')
        };
        let mut pending_path: Vec<ChordPattern> = op_prefix.to_vec();
        pending_path.push(around_chord.clone());
        handle.bind_legacy(
            layer,
            mode,
            &pending_path,
            Action::SetPending(Pending::AfterTextObject {
                operator: op,
                around,
            }),
            source(),
        );
        register_text_object_resolutions(
            handle,
            &pending_path,
            op,
            around,
            builtins,
        );
    }

    // ---- Find-char chained: `[op, f]` / `[op, F]` / `[op, t]` /
    // ---- `[op, T]` -> `SetPending(AfterFindChar { kind, operator
    // ---- = Some(op) })`. The third-key resolution stays in
    // ---- legacy `resolve_after_find_char` (slice 8.g.v).
    for (chord, kind) in [
        (lit_char('f'), FindKind::Forward),
        (lit_char('F'), FindKind::Backward),
        (lit_char('t'), FindKind::TillForward),
        (lit_char('T'), FindKind::TillBackward),
    ] {
        let mut path: Vec<ChordPattern> = op_prefix.to_vec();
        path.push(chord);
        handle.bind_legacy(
            layer,
            mode,
            &path,
            Action::SetPending(Pending::AfterFindChar {
                kind,
                operator: Some(op),
            }),
            source(),
        );
    }
}

/// Register every text-object resolution path under
/// `pending_prefix` (which already ends in `i` or `a`). For each
/// text-object chord (with all its aliases), bind to the
/// corresponding inner / around `TextObjectId`.
fn register_text_object_resolutions(
    handle: &KeymapHandle,
    pending_prefix: &[ChordPattern],
    op: lattice_grammar::registry::OperatorId,
    around: bool,
    builtins: &Builtins,
) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Normal;

    let textobj_table: &[(&[ChordPattern], lattice_grammar::registry::TextObjectId, lattice_grammar::registry::TextObjectId)] = &[
        (
            &[lit_char('w')],
            builtins.inner_word,
            builtins.around_word,
        ),
        (
            &[lit_char('W')],
            builtins.inner_big_word,
            builtins.around_big_word,
        ),
        (
            &[lit_char('p')],
            builtins.inner_paragraph,
            builtins.around_paragraph,
        ),
        (
            &[lit_char('s')],
            builtins.inner_sentence,
            builtins.around_sentence,
        ),
        (
            &[lit_char('t')],
            builtins.inner_tag,
            builtins.around_tag,
        ),
        (
            &[lit_char('"')],
            builtins.inner_quote_double,
            builtins.around_quote_double,
        ),
        (
            &[lit_char('\'')],
            builtins.inner_quote_single,
            builtins.around_quote_single,
        ),
        (
            &[lit_char('`')],
            builtins.inner_quote_backtick,
            builtins.around_quote_backtick,
        ),
        // Paren aliases: `(`, `)`, `b`.
        (
            &[lit_char('('), lit_char(')'), lit_char('b')],
            builtins.inner_paren,
            builtins.around_paren,
        ),
        // Bracket aliases: `[`, `]`.
        (
            &[lit_char('['), lit_char(']')],
            builtins.inner_bracket,
            builtins.around_bracket,
        ),
        // Brace aliases: `{`, `}`, `B`.
        (
            &[lit_char('{'), lit_char('}'), lit_char('B')],
            builtins.inner_brace,
            builtins.around_brace,
        ),
        // Angle aliases: `<`, `>`.
        (
            &[lit_char('<'), lit_char('>')],
            builtins.inner_angle,
            builtins.around_angle,
        ),
    ];
    for (chord_aliases, inner_id, around_id) in textobj_table {
        let tobj = if around { *around_id } else { *inner_id };
        for chord in chord_aliases.iter() {
            let mut path: Vec<ChordPattern> = pending_prefix.to_vec();
            path.push(chord.clone());
            handle.bind(
                layer,
                mode,
                &path,
                CommandInvocation::of(op.0)
                    .with_target(Target::TextObject(tobj, Args::None)),
                source(),
            );
        }
    }
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

/// Resolve the next key of a multi-chord Normal-mode sequence
/// via the registry. The caller supplies the prefix chord
/// sequence already absorbed; this helper appends the
/// normalised current chord and looks the resulting path up in
/// the trie. `Bound` -> the bound action; everything else
/// (`Partial` / `Unbound`) -> `Action::SetPending(Pending::None)`
/// to drop the pending state, matching every legacy
/// `resolve_after_*`'s catchall.
///
/// Used by:
/// - `Pending::AfterG` / `Pending::AfterZ` (slice 8.g.ii) --
///   prefix `[g]` / `[z]`.
/// - `Pending::AfterOperator(op)` (slice 8.g.iii) -- prefix
///   `[d]` / `[c]` / `[y]` / `[>]` / `[<]` for the single-chord
///   operators, or `[g, U]` / `[g, u]` / `[g, ~]` for the case
///   operators (mapped via `operator_prefix`).
/// - `Pending::AfterTextObject { op, around }` (slice 8.g.iii) --
///   prefix `[op_prefix..., 'i' or 'a']`.
pub fn lookup_normal_with_prefix(
    handle: &KeymapHandle,
    prefix: &[KeyChord],
    event: &KeyEvent,
) -> Action {
    let Some(raw_chord) = KeyChord::from_event(event) else {
        return Action::SetPending(Pending::None);
    };
    let chord = normalize_for_normal_lookup(raw_chord);
    let mut path: Vec<KeyChord> = prefix.to_vec();
    path.push(chord);
    match handle.lookup(BindingMode::Normal, &path) {
        LookupResult::Bound { command, .. } => action_from_bound(&command),
        LookupResult::Partial | LookupResult::Unbound => {
            Action::SetPending(Pending::None)
        }
    }
}

/// Slice 8.g.iv: attach the input-side count accumulator to a
/// resolved `Action::Invoke`. Pure function; non-`Invoke`
/// actions pass through unchanged.
///
/// Vim semantics: `<op-count><op><motion-count><motion>` yields
/// a final count of `op_count * motion_count`. Either alone
/// replaces the default count of `1`. The motion side falls
/// back to `inv.count` (any default the binding registered with
/// at boot, e.g. `<PageDown>`'s `Count(10)`) when the user
/// hasn't typed a digit prefix.
///
/// Architecture doc §7.1: "Once a non-digit chord arrives,
/// lookup runs with the accumulated count attached to the
/// resulting `CommandInvocation`'s count field. Dispatch
/// unchanged; `execute(invocation_with_count)` works today."
/// Before this slice the multiplication lived in App's
/// dispatcher (`run_document_invocation` /
/// `run_read_only_motion`); now it rides with the action out
/// of `translate_normal`. App still resets `pending_count` /
/// `op_count` at end-of-dispatch.
pub fn attach_count(action: Action, pending_count: u32, op_count: u32) -> Action {
    let Action::Invoke(mut inv) = action else {
        return action;
    };
    let motion_count = if pending_count > 0 {
        pending_count
    } else {
        inv.count.map(|c| c.0).unwrap_or(1)
    };
    let final_count = if op_count > 0 {
        op_count.saturating_mul(motion_count)
    } else {
        motion_count
    };
    if final_count > 1 {
        inv = inv.with_count(lattice_grammar::command::Count(final_count));
    }
    Action::Invoke(inv)
}

/// Map an operator id to its primary chord prefix in the
/// Normal-mode trie. Used by the `Pending::AfterOperator` and
/// `Pending::AfterTextObject` resolvers to compute the lookup
/// path.
///
/// Returns an empty `Vec` for unknown operators -- the caller
/// surfaces that as `SetPending(None)`. Slice 8.g.iii covers
/// every operator the existing keymap exposes; plugin-defined
/// operators (slice 8.h) will register their own prefix at
/// binding time.
pub fn operator_prefix(
    op: lattice_grammar::registry::OperatorId,
    builtins: &Builtins,
) -> Vec<KeyChord> {
    if op == builtins.delete {
        vec![KeyChord::char('d')]
    } else if op == builtins.change {
        vec![KeyChord::char('c')]
    } else if op == builtins.yank {
        vec![KeyChord::char('y')]
    } else if op == builtins.indent_right {
        vec![KeyChord::char('>')]
    } else if op == builtins.indent_left {
        vec![KeyChord::char('<')]
    } else if op == builtins.upper {
        vec![KeyChord::char('g'), KeyChord::char('U')]
    } else if op == builtins.lower {
        vec![KeyChord::char('g'), KeyChord::char('u')]
    } else if op == builtins.toggle_case {
        vec![KeyChord::char('g'), KeyChord::char('~')]
    } else {
        Vec::new()
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

    /// Slice 8.g.iii: `d` is now a terminal binding that arms
    /// `Pending::AfterOperator(delete)`. The trie still has
    /// children (`[d, w]`, `[d, d]`, etc.) for the second-key
    /// resolution, but lookup of `[d]` alone returns `Bound`
    /// because the depth-1 node carries a binding.
    #[test]
    fn d_arms_after_operator_delete_pending() {
        let (h, b) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('d'), KeyModifiers::NONE));
        match r {
            Some(Action::SetPending(Pending::AfterOperator(op))) => {
                assert_eq!(op, b.delete);
            }
            other => panic!(
                "expected SetPending(AfterOperator(delete)), got {other:?}"
            ),
        }
    }

    #[test]
    fn dw_resolves_to_delete_with_word_forward_target() {
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
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
    fn di_arms_after_text_object_pending_inner() {
        let (h, b) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('i'), KeyModifiers::NONE),
        );
        match r {
            Action::SetPending(Pending::AfterTextObject {
                operator,
                around,
            }) => {
                assert_eq!(operator, b.delete);
                assert!(!around);
            }
            other => panic!("expected SetPending(AfterTextObject), got {other:?}"),
        }
    }

    #[test]
    fn diw_resolves_to_delete_inner_word() {
        let (h, b) = populated_handle();
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
        let (h, b_) = populated_handle();
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
    fn df_arms_after_find_char_with_operator() {
        let (h, b) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        match r {
            Action::SetPending(Pending::AfterFindChar {
                kind: FindKind::Forward,
                operator: Some(op),
            }) => {
                assert_eq!(op, b.delete);
            }
            other => panic!(
                "expected SetPending(AfterFindChar Forward delete), got {other:?}"
            ),
        }
    }

    #[test]
    fn d_unrecognised_drops_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('Q'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::SetPending(Pending::None)));
    }

    /// Doubled-operator under the `g` prefix: `gUU` -> linewise
    /// upper. The prefix walk is `[g, U, U]`.
    #[test]
    fn g_uu_resolves_to_upper_current_line() {
        let (h, b) = populated_handle();
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
        let (h, b) = populated_handle();
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
    fn gd_resolves_to_lsp_definition_request() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::LspDefinitionRequest));
    }

    #[test]
    fn gu_arms_after_operator_pending_for_lower() {
        let (h, b) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
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
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('J'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::JoinLines { with_space: false }));
    }

    #[test]
    fn zz_centers_cursor() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Center)));
    }

    #[test]
    fn z_dot_aliases_zz() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('.'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Center)));
    }

    #[test]
    fn z_enter_aliases_zt() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Top)));
    }

    #[test]
    fn z_dash_aliases_zb() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('-'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ScrollCursorTo(ScrollPos::Bottom)));
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::ToggleFoldAtCursor));
    }

    #[test]
    fn z_unrecognized_drops_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::SetPending(Pending::None)));
    }

    #[test]
    fn z_esc_drops_pending() {
        let (h, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
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
        let r = attach_count(Action::SetPending(Pending::AfterG), 5, 0);
        assert!(matches!(r, Action::SetPending(Pending::AfterG)));
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
}
