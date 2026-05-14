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
use lattice_grammar::Target;
use lattice_grammar::args::Args;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_protocol::ids::CommandId;

use crate::actions::ActionIds;
use crate::app::{Action, FindKind};
use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, LookupResult};

/// Register the slice 8.g.i Normal-mode catalog into the
/// supplied handle's `Builtin` layer. The legacy
/// `input::translate_normal` keeps its match arm for the
/// bindings not yet in this catalog.
///
/// `actions` is the App-side action ID table (slice 8.i; see
/// `docs/dev/notes/8i-approach.md`). Bindings that historically fired an
/// `Action::Foo` directly via `bind_legacy` get migrated to
/// `bind(... CommandInvocation::of(actions.foo) ...)` as the
/// per-batch slices land; the rest stay on the bridge until
/// their batch's turn.
pub fn register_normal_bindings(handle: &KeymapHandle, builtins: &Builtins, actions: &ActionIds) {
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
        CommandInvocation::of(builtins.yank.0).with_range(lattice_grammar::Range::CurrentLine),
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
        CommandInvocation::of(builtins.change.0).with_range(lattice_grammar::Range::CurrentLine),
        source(),
    );

    // ---- Legacy-action bindings (no `CommandInvocation` peer
    // ---- today; bridge stays until 8.i).

    // Viewport jumps.
    handle.bind(
        layer,
        mode,
        &[lit_char('H')],
        CommandInvocation::of(actions.jump_viewport_top),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('M')],
        CommandInvocation::of(actions.jump_viewport_middle),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('L')],
        CommandInvocation::of(actions.jump_viewport_bottom),
        source(),
    );

    // Paste.
    handle.bind(
        layer,
        mode,
        &[lit_char('p')],
        CommandInvocation::of(actions.paste_after),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('P')],
        CommandInvocation::of(actions.paste_before),
        source(),
    );

    // Mode entry.
    handle.bind(
        layer,
        mode,
        &[lit_char('i')],
        CommandInvocation::of(actions.enter_mode_insert),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('a')],
        CommandInvocation::of(actions.enter_append),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('o')],
        CommandInvocation::of(actions.open_line_below),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('O')],
        CommandInvocation::of(actions.open_line_above),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char(':')],
        CommandInvocation::of(actions.enter_command_line),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('v')],
        CommandInvocation::of(actions.enter_visual_charwise),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('V')],
        CommandInvocation::of(actions.enter_visual_linewise),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('R')],
        CommandInvocation::of(actions.enter_mode_replace),
        source(),
    );

    // Misc single-chord.
    handle.bind(
        layer,
        mode,
        &[lit_char('J')],
        CommandInvocation::of(actions.join_lines_with_space),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char(';')],
        CommandInvocation::of(actions.find_repeat_forward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char(',')],
        CommandInvocation::of(actions.find_repeat_reverse),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('~')],
        CommandInvocation::of(actions.toggle_case_at_cursor),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('K')],
        CommandInvocation::of(actions.lsp_hover_request),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('/')],
        CommandInvocation::of(actions.enter_search_forward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('?')],
        CommandInvocation::of(actions.enter_search_backward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('n')],
        CommandInvocation::of(actions.search_next),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('N')],
        CommandInvocation::of(actions.search_previous),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('*')],
        CommandInvocation::of(actions.search_word_under_cursor_forward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('#')],
        CommandInvocation::of(actions.search_word_under_cursor_backward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('%')],
        CommandInvocation::of(actions.match_bracket),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('u')],
        CommandInvocation::of(actions.undo),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('.')],
        CommandInvocation::of(actions.repeat_last_change),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('-')],
        CommandInvocation::of(actions.oil_navigate_up),
        source(),
    );

    // Specials.
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::Tab)],
        CommandInvocation::of(actions.jump_history_forward),
        source(),
    );
    // PageDown / PageUp -- count-10 line-down / line-up. These
    // bake the count into the invocation directly (no
    // pending-count interaction); legacy parity preserved.
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::PageDown)],
        CommandInvocation::of(builtins.line_down.0).with_count(lattice_grammar::command::Count(10)),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_special(SpecialKey::PageUp)],
        CommandInvocation::of(builtins.line_up.0).with_count(lattice_grammar::command::Count(10)),
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
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('U')],
        CommandInvocation::of(actions.absorb_operator_upper),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('u')],
        CommandInvocation::of(actions.absorb_operator_lower),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('~')],
        CommandInvocation::of(actions.absorb_operator_toggle_case),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('v')],
        CommandInvocation::of(actions.reselect_last_visual),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('J')],
        CommandInvocation::of(actions.join_lines_bare),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char(';')],
        CommandInvocation::of(actions.walk_mark_history_back),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char(',')],
        CommandInvocation::of(actions.walk_mark_history_forward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('d')],
        CommandInvocation::of(actions.lsp_definition_request),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('D')],
        CommandInvocation::of(actions.lsp_declaration_request),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('y')],
        CommandInvocation::of(actions.lsp_type_definition_request),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('I')],
        CommandInvocation::of(actions.lsp_implementation_request),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('r')],
        CommandInvocation::of(actions.lsp_references_request),
        source(),
    );
    // 4.5.c: `gx` -> follow LSP documentLink at cursor.
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('x')],
        CommandInvocation::of(actions.lsp_follow_link_at_cursor),
        source(),
    );

    // ---- Slice 8.g.ii: `z_` family.
    //
    // Same pattern: `[z]` is a partial trie node;
    // `lookup_normal` converts it to
    // `SetPending(Pending::AfterZ)`.
    let z = lit_char('z');

    // Center-cursor scrolls. `zz` and `z.` both center.
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('z')],
        CommandInvocation::of(actions.scroll_cursor_to_center),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('.')],
        CommandInvocation::of(actions.scroll_cursor_to_center),
        source(),
    );
    // Top-of-viewport scrolls. `zt` and `z<CR>` both align top.
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('t')],
        CommandInvocation::of(actions.scroll_cursor_to_top),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_special(SpecialKey::Enter)],
        CommandInvocation::of(actions.scroll_cursor_to_top),
        source(),
    );
    // Bottom-of-viewport scrolls. `zb` and `z-` both align bottom.
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('b')],
        CommandInvocation::of(actions.scroll_cursor_to_bottom),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('-')],
        CommandInvocation::of(actions.scroll_cursor_to_bottom),
        source(),
    );

    // Folds.
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('f')],
        CommandInvocation::of(actions.create_fold_from_visual),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('o')],
        CommandInvocation::of(actions.open_fold_at_cursor),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('c')],
        CommandInvocation::of(actions.close_fold_at_cursor),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('a')],
        CommandInvocation::of(actions.toggle_fold_at_cursor),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('R')],
        CommandInvocation::of(actions.open_all_folds),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('M')],
        CommandInvocation::of(actions.close_all_folds),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('d')],
        CommandInvocation::of(actions.delete_fold_at_cursor),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('j')],
        CommandInvocation::of(actions.goto_next_fold),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z.clone(), lit_char('k')],
        CommandInvocation::of(actions.goto_prev_fold),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[z, lit_char('i')],
        CommandInvocation::of(actions.toggle_fold_enable),
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

    // ---- Slice 8.g.v: mark / register / find-char / macro
    // ---- wildcards. Each prefix chord is a partial trie node
    // ---- whose terminal sub-binding is a `CharLiteral`
    // ---- (matches any bare-printable char). The depth-1
    // ---- binding here arms the legacy `Pending::After*` state
    // ---- so the App's existing two-keystroke flow stays
    // ---- intact; the depth-2 wildcard binding carries a
    // ---- placeholder action that the dispatcher's
    // ---- `substitute_normal_capture` rewrites with the
    // ---- captured char.

    // `m<X>` -- set mark X. The `[m]` standalone prefix is
    // unbound (slice 8.i.4.a): the trie returns Partial because
    // `[m, *]` exists, and `lookup_normal` synthesises
    // `Action::AbsorbPartialChord(m)` so `App::partial_chord` =
    // `[m]`. Same shape for `'`, `` ` ``, `"`, `q`, `@`, `<C-w>`
    // below, plus `g` and `z` which never had a standalone bind.
    handle.bind(
        layer,
        mode,
        &[lit_char('m'), ChordPattern::CharLiteral],
        CommandInvocation::of(actions.set_mark),
        source(),
    );

    // `'<X>` -- jump to mark X (line).
    handle.bind(
        layer,
        mode,
        &[lit_char('\''), ChordPattern::CharLiteral],
        CommandInvocation::of(actions.jump_to_mark_line),
        source(),
    );

    // `` `<X> `` -- jump to mark X (exact).
    handle.bind(
        layer,
        mode,
        &[lit_char('`'), ChordPattern::CharLiteral],
        CommandInvocation::of(actions.jump_to_mark_exact),
        source(),
    );

    // `"<X>` -- select register X for the next operator / paste.
    handle.bind(
        layer,
        mode,
        &[lit_char('"'), ChordPattern::CharLiteral],
        CommandInvocation::of(actions.select_register),
        source(),
    );

    // `q<X>` -- start macro recording into register X.
    // `q` while recording stops; that case is handled before
    // the trie lookup in `compute_normal_action` because it
    // depends on the App-side `recording_macro` state.
    handle.bind(
        layer,
        mode,
        &[lit_char('q'), ChordPattern::CharLiteral],
        CommandInvocation::of(actions.start_macro_record),
        source(),
    );

    // `@<X>` -- play macro from register X (`@@` repeats last).
    handle.bind(
        layer,
        mode,
        &[lit_char('@'), ChordPattern::CharLiteral],
        CommandInvocation::of(actions.play_macro),
        source(),
    );

    // `f<X>` / `F<X>` / `t<X>` / `T<X>` -- find-char on the
    // current line (no operator). `[f]` etc. arm the pending
    // state; `[f, CharLiteral]` resolves to a typed
    // `Invoke(find_char_*, Args::Char(captured))`.
    register_find_char_paths(handle, &[], None, builtins);

    // ---- d/c/y/>/< as single-chord terminals that arm the
    // operator-pending state. `[g, U]` / `[g, u]` / `[g, ~]` were
    // already registered at slice 8.g.ii; their depth-2 binding
    // sets the same `SetPending(AfterOperator(...))` action.
    handle.bind(
        layer,
        mode,
        &[lit_char('d')],
        CommandInvocation::of(actions.absorb_operator_delete),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('c')],
        CommandInvocation::of(actions.absorb_operator_change),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('y')],
        CommandInvocation::of(actions.absorb_operator_yank),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('>')],
        CommandInvocation::of(actions.absorb_operator_indent_right),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('<')],
        CommandInvocation::of(actions.absorb_operator_indent_left),
        source(),
    );

    // ---- Slice 8.g.vi: CTRL chord bindings.
    //
    // Direct depth-1 entries, modifier preserved. Modifier
    // normalisation in `lookup_normal` keeps CTRL+SHIFT, so
    // `<C-d>` resolves as `(Char('d'), CTRL)` -- the chord the
    // trie stores. The legacy CTRL guard at the top of
    // `compute_normal_action` retires once these registrations
    // land; every CTRL-bearing chord now flows through the
    // registry like every other binding.
    //
    // `<C-c>` (the universal quit hatch) is intercepted by
    // `input::translate` *before* mode dispatch and so never
    // reaches this trie -- skipping the registration is
    // intentional.

    // `<C-d>` / `<C-u>` -- half-page scroll. Bake `Count(10)`
    // into the invocation so 8.g.iv's `attach_count` honours it
    // when no user-typed prefix is in flight.
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('d'))],
        CommandInvocation::of(builtins.line_down.0).with_count(lattice_grammar::command::Count(10)),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('u'))],
        CommandInvocation::of(builtins.line_up.0).with_count(lattice_grammar::command::Count(10)),
        source(),
    );

    // Viewport / scroll / undo-tree / jump history / tag stack /
    // redraw / blockwise visual.
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('f'))],
        CommandInvocation::of(actions.page_down),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('b'))],
        CommandInvocation::of(actions.page_up),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('e'))],
        CommandInvocation::of(actions.scroll_line_down),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('y'))],
        CommandInvocation::of(actions.scroll_line_up),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('r'))],
        CommandInvocation::of(actions.redo),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('o'))],
        CommandInvocation::of(actions.jump_history_back),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('i'))],
        CommandInvocation::of(actions.jump_history_forward),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('t'))],
        CommandInvocation::of(actions.tag_stack_pop),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('l'))],
        CommandInvocation::of(actions.redraw_screen),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('v'))],
        CommandInvocation::of(actions.enter_visual_blockwise),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit(KeyChord::ctrl('q'))],
        CommandInvocation::of(actions.enter_visual_blockwise),
        source(),
    );

    // ---- `<C-w>` window-management sub-tree.
    //
    // Slice 8.i.4.a: the standalone `[<C-w>]` bind is gone --
    // the trie returns `Partial` and `lookup_normal` emits
    // `AbsorbPartialChord` (same shape as `g` / `z` / `m` / `'`
    // / etc. above). Children of `[<C-w>, *]` register their
    // resolutions below.
    register_ctrl_w_sub_tree(handle, actions);
}

/// Register every `[<C-w>, X]` path covered by the legacy
/// `resolve_after_ctrl_w`. Vim is lenient about the second key
/// after `<C-w>`: both ctrl-modified (`<C-w><C-l>`) and bare
/// (`<C-w>l`) variants navigate identically. Many terminals
/// also collapse `<C-h>` to Backspace and `<C-i>` to Tab; we
/// honour those mappings via the bare-key paths.
fn register_ctrl_w_sub_tree(handle: &KeymapHandle, actions: &ActionIds) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Normal;
    let cw = lit(KeyChord::ctrl('w'));

    // Bare-key second chord (NextPane / PrevPane / split / close /
    // navigate). Includes the Tab / BackTab / arrow / Backspace
    // aliases the legacy supported.
    // Slice 8.i.4.d: pane chords routed through typed
    // `CommandKind::Action` invocations. Each chord binds the
    // resolved action's `CommandId` directly; App's
    // `apply_app_effect` maps `AppEffect::*Pane*` to the
    // legacy `Action::*Pane*` arms (the latter retire when
    // those arms are inlined in a future cleanup slice).
    let bare_table: &[(&[ChordPattern], CommandId)] = &[
        (
            &[lit_char('s'), lit_char('S')],
            actions.split_pane_horizontal,
        ),
        (&[lit_char('v')], actions.split_pane_vertical),
        (&[lit_char('c'), lit_char('q')], actions.close_pane),
        (
            &[
                lit_char('h'),
                lit_special(SpecialKey::Left),
                lit_special(SpecialKey::Backspace),
            ],
            actions.navigate_pane_left,
        ),
        (
            &[lit_char('j'), lit_special(SpecialKey::Down)],
            actions.navigate_pane_down,
        ),
        (
            &[lit_char('k'), lit_special(SpecialKey::Up)],
            actions.navigate_pane_up,
        ),
        (
            &[lit_char('l'), lit_special(SpecialKey::Right)],
            actions.navigate_pane_right,
        ),
        (
            &[lit_char('w'), lit_special(SpecialKey::Tab)],
            actions.next_pane,
        ),
        (
            &[
                lit_char('W'),
                ChordPattern::Literal(KeyChord {
                    key: KeyKind::Special(SpecialKey::Tab),
                    mods: KeyMods::SHIFT,
                }),
            ],
            actions.prev_pane,
        ),
    ];
    for (chords, action_id) in bare_table {
        for chord in chords.iter() {
            handle.bind(
                layer,
                mode,
                &[cw.clone(), chord.clone()],
                CommandInvocation::of(*action_id),
                source(),
            );
        }
    }

    // Ctrl-modified second chord: `<C-w><C-X>` mirrors `<C-w>X`
    // for the navigation / split / close / NextPane bindings.
    // `<C-c>` and `<C-q>` both map to ClosePane in the legacy
    // even though `<C-c>` is intercepted as Quit before the
    // pending arm runs (so the `<C-c>` registration is
    // unreachable in practice -- kept for parity with the
    // legacy table).
    let ctrl_table: &[(char, CommandId)] = &[
        ('w', actions.next_pane),
        ('h', actions.navigate_pane_left),
        ('j', actions.navigate_pane_down),
        ('k', actions.navigate_pane_up),
        ('l', actions.navigate_pane_right),
        ('s', actions.split_pane_horizontal),
        ('v', actions.split_pane_vertical),
        ('c', actions.close_pane),
        ('q', actions.close_pane),
    ];
    for (c, action_id) in ctrl_table {
        handle.bind(
            layer,
            mode,
            &[cw.clone(), lit(KeyChord::ctrl(*c))],
            CommandInvocation::of(*action_id),
            source(),
        );
    }
}

fn lit(chord: KeyChord) -> ChordPattern {
    ChordPattern::Literal(chord)
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
            CommandInvocation::of(op.0).with_target(Target::Motion(*motion, Args::None)),
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
            CommandInvocation::of(op.0).with_range(lattice_grammar::Range::CurrentLine),
            source(),
        );
    }

    // ---- Text-object resolutions. Slice 8.i.4.c: the depth-2
    // ---- `[op, i]` / `[op, a]` standalone arms are gone -- the
    // ---- trie's natural `Partial` result for those paths
    // ---- (because `[op, i, X]` / `[op, a, X]` are bound) drives
    // ---- `App::partial_chord` via `AbsorbPartialChord`. The
    // ---- depth-3 resolutions stay; they fire when partial_chord
    // ---- has accumulated `[op, i / a]` and the user types the
    // ---- text-object char.
    for around in [false, true] {
        let around_chord: ChordPattern = if around { lit_char('a') } else { lit_char('i') };
        let mut pending_path: Vec<ChordPattern> = op_prefix.to_vec();
        pending_path.push(around_chord.clone());
        register_text_object_resolutions(handle, &pending_path, op, around, builtins);
    }

    // ---- Slice 8.g.v: find-char chained -- the depth-2
    // ---- `[op, f/F/t/T]` arms the pending state, and the
    // ---- depth-3 `[op, f/F/t/T, CharLiteral]` wildcard
    // ---- resolves to a typed `Invoke(op,
    // ---- Target::Motion(find_char_*, Args::Char(captured)))`.
    register_find_char_paths(handle, op_prefix, Some(op), builtins);
}

/// Register the four find-char chord paths under `prefix`. When
/// `operator` is `None`, registers the standalone `f` / `F` /
/// `t` / `T` (Normal-mode cursor motion). When `Some(op)`, the
/// paths sit under the operator's prefix (e.g. `[d, f, X]`) and
/// the resolved invocation is `Invoke(op,
/// Target::Motion(find_char_*, Args::Char(captured)))`.
///
/// The depth-1 entry (just `[prefix..., f]`) arms
/// `Pending::AfterFindChar` so the App's existing
/// two-keystroke flow can stay; the depth-2 entry is the
/// `CharLiteral` wildcard that captures the char and triggers
/// the substituter in `substitute_normal_capture`.
fn register_find_char_paths(
    handle: &KeymapHandle,
    prefix: &[ChordPattern],
    operator: Option<lattice_grammar::registry::OperatorId>,
    builtins: &Builtins,
) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Normal;

    let table: &[(ChordPattern, FindKind, lattice_grammar::registry::MotionId)] = &[
        (lit_char('f'), FindKind::Forward, builtins.find_char_forward),
        (
            lit_char('F'),
            FindKind::Backward,
            builtins.find_char_backward,
        ),
        (
            lit_char('t'),
            FindKind::TillForward,
            builtins.till_char_forward,
        ),
        (
            lit_char('T'),
            FindKind::TillBackward,
            builtins.till_char_backward,
        ),
    ];

    for (chord, _kind, motion_id) in table {
        // Slice 8.i.4.c: the depth-1 (or depth-2 under operator)
        // standalone `[..., f / F / t / T]` arms are gone -- the
        // trie's natural `Partial` result for those paths drives
        // `App::partial_chord` via `AbsorbPartialChord`. Only
        // the depth-2 (or depth-3) `CharLiteral` wildcard
        // resolution stays; it fires when partial_chord has
        // accumulated `[..., f / F / t / T]` and the user types
        // the target char.
        let mut wild_path: Vec<ChordPattern> = prefix.to_vec();
        wild_path.push(chord.clone());
        wild_path.push(ChordPattern::CharLiteral);
        let invocation = match operator {
            None => CommandInvocation::of(motion_id.0),
            Some(op) => {
                CommandInvocation::of(op.0).with_target(Target::Motion(*motion_id, Args::None))
            }
        };
        handle.bind(layer, mode, &wild_path, invocation, source());
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

    let textobj_table: &[(
        &[ChordPattern],
        lattice_grammar::registry::TextObjectId,
        lattice_grammar::registry::TextObjectId,
    )] = &[
        (&[lit_char('w')], builtins.inner_word, builtins.around_word),
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
        (&[lit_char('t')], builtins.inner_tag, builtins.around_tag),
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
                CommandInvocation::of(op.0).with_target(Target::TextObject(tobj, Args::None)),
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
    let Some(raw_chord) = crate::chord::from_event(event) else {
        return None;
    };
    let chord = normalize_for_normal_lookup(raw_chord);
    match handle.lookup(BindingMode::Normal, &[chord]) {
        LookupResult::Bound { command, captured } => {
            Some(action_from_bound_with_capture(&command, &captured))
        }
        LookupResult::Partial => {
            // Slice 8.i.4.a: every `Partial` result absorbs into
            // `App::partial_chord` via `AbsorbPartialChord`. The
            // App's next keystroke runs through
            // `dispatch_normal` with this stack as prefix, hitting
            // the trie's resolved binding for the full path.
            // Replaces the prior `g`/`z` -> `SetPending(After*)`
            // synthesis. The 9 simple prefix-only Pending variants
            // (`AfterG`, `AfterZ`, `AfterCtrlW`, `AfterSetMark`,
            // `AfterJumpMarkLine`, `AfterJumpMarkExact`,
            // `AfterRegister`, `AfterMacroStart`,
            // `AfterMacroPlay`) all funnel through here now.
            // Parameterised pendings (`AfterOperator(_)`,
            // `AfterTextObject{_}`, `AfterFindChar{_}`) keep their
            // own `SetPending` flow until 8.i.4.b.
            Some(Action::AbsorbPartialChord(chord))
        }
        LookupResult::Unbound => None,
    }
}

/// Resolve the next key of a multi-chord Normal-mode sequence
/// via the registry. The caller supplies the prefix chord
/// sequence already absorbed; this helper appends the
/// normalised current chord and looks the resulting path up in
/// the trie. `Bound` -> the bound action; everything else
/// (`Partial` / `Unbound`) -> `Action::None`
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
    let Some(raw_chord) = crate::chord::from_event(event) else {
        return Action::None;
    };
    let chord = normalize_for_normal_lookup(raw_chord);
    let mut path: Vec<KeyChord> = prefix.to_vec();
    path.push(chord);
    match handle.lookup(BindingMode::Normal, &path) {
        LookupResult::Bound { command, captured } => {
            action_from_bound_with_capture(&command, &captured)
        }
        LookupResult::Partial => {
            // Slice 8.i.4.c: nested partial chord. Same shape
            // as `lookup_normal`'s `Partial` arm -- absorb the
            // current chord into `App::partial_chord` so the
            // next keystroke routes through this helper again
            // with the extended prefix. Required for chains
            // like `di` (d already in partial_chord, `i` is
            // partial because `[d, i, w]` etc. are bound) and
            // `df` (find-char prefix).
            Action::AbsorbPartialChord(chord)
        }
        LookupResult::Unbound => Action::None,
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

/// Pull the action out of a bound trie node, folding any
/// `CharLiteral` captures into the result. Slice 8.g.v: lookups
/// through wildcard children (`'a`, `"a`, `fX`, `mX`, `qX`,
/// `@X`, plus the operator-prefixed `dfX` etc.) capture the
/// matched char; this helper applies it to the placeholder
/// stashed in the bound action.
fn action_from_bound_with_capture(bound: &Arc<BoundCommand>, captured: &[char]) -> Action {
    // Fold any captured wildcard char into `Args::Char(c)` so the
    // bound `ActionSpec`'s apply closure can see it. Validation
    // lives in the spec (e.g. `m<X>` requires `[a-zA-Z0-9]`);
    // invalid chars dispatch to `Effect::None`, which is a benign
    // no-op because `App::apply` clears the partial-chord stack on
    // every non-`AbsorbPartialChord(_)` action.
    let mut inv = bound.command.clone();
    if let Some(&c) = captured.first() {
        inv = substitute_invocation_char_arg(inv, c);
    }
    Action::Invoke(inv)
}

fn substitute_invocation_char_arg(
    mut inv: lattice_grammar::CommandInvocation,
    c: char,
) -> lattice_grammar::CommandInvocation {
    use lattice_grammar::args::Args;
    // Operator-targeted form: `df<X>` registers as
    // `Invoke(op, Target::Motion(find_char_*, Args::None))`.
    // Substitute the target's args.
    if let Some(Target::Motion(motion_id, Args::None)) = inv.target {
        inv = inv.with_target(Target::Motion(motion_id, Args::Char(c)));
        return inv;
    }
    // Standalone form: `f<X>` registers as
    // `Invoke(find_char_*, Args::None)`. Substitute the
    // invocation's args.
    if matches!(inv.args, Args::None) {
        inv = inv.with_args(Args::Char(c));
    }
    inv
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

    fn fixture() -> (CommandRegistry, Builtins, ActionIds) {
        let mut r = CommandRegistry::new();
        let b = lattice_grammar::builtins::populate(&mut r);
        let a = crate::actions::populate(&mut r, &b);
        (r, b, a)
    }

    fn populated_handle() -> (KeymapHandle, Builtins, ActionIds) {
        let (_, b, a) = fixture();
        let h = KeymapHandle::new();
        register_normal_bindings(&h, &b, &a);
        (h, b, a)
    }

    #[test]
    fn motion_h_invokes_char_left() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('h'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn arrow_left_aliases_char_left() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Left, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.char_left.0),
            other => panic!("expected Invoke(char_left), got {other:?}"),
        }
    }

    #[test]
    fn upper_g_invokes_goto_last_line() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('G'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, b.goto_last_line.0),
            other => panic!("expected Invoke(goto_last_line), got {other:?}"),
        }
    }

    #[test]
    fn pseudo_operator_x_carries_char_right_target() {
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('H'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.jump_viewport_top),
            other => panic!("expected Invoke(jump_viewport_top), got {other:?}"),
        }
    }

    #[test]
    fn mode_entry_v_enters_charwise_visual() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('v'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.enter_visual_charwise),
            other => panic!("expected Invoke(enter_visual_charwise), got {other:?}"),
        }
    }

    #[test]
    fn paste_p_lower_pastes_after() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('p'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.paste_after),
            other => panic!("expected Invoke(paste_after), got {other:?}"),
        }
    }

    #[test]
    fn search_slash_enters_forward_search() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('/'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.enter_search_forward),
            other => panic!("expected Invoke(enter_search_forward), got {other:?}"),
        }
    }

    #[test]
    fn tab_jumps_history_forward() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Tab, KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.jump_history_forward),
            other => panic!("expected Invoke(jump_history_forward), got {other:?}"),
        }
    }

    #[test]
    fn page_down_invokes_line_down_with_count_ten() {
        let (h, b, _) = populated_handle();
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
    fn d_invokes_absorb_operator_delete() {
        // Slice 8.i.4.c: pressing `d` invokes the typed
        // `absorb_operator_delete` action, which emits
        // `AppEffect::AbsorbOperatorPrefix(delete)`. App's
        // handler latches op_count and pushes [d] to
        // partial_chord.
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('d'), KeyModifiers::NONE));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, a.absorb_operator_delete);
            }
            other => panic!("expected Invoke(absorb_operator_delete), got {other:?}"),
        }
    }

    #[test]
    fn dw_resolves_to_delete_with_word_forward_target() {
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
    fn di_absorbs_partial_chord() {
        // Slice 8.i.4.c: with prefix [d], pressing `i` absorbs
        // into partial_chord. The trie returns Partial because
        // `[d, i, w]` etc. are bound; lookup_normal_with_prefix
        // emits `AbsorbPartialChord(i)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('i'), KeyModifiers::NONE),
        );
        assert!(matches!(
            r,
            Action::AbsorbPartialChord(c) if c == KeyChord::char('i')
        ));
    }

    #[test]
    fn diw_resolves_to_delete_inner_word() {
        let (h, b, _) = populated_handle();
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
        let (h, b_, _) = populated_handle();
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
    fn df_absorbs_partial_chord() {
        // Slice 8.i.4.c: with prefix [d], pressing `f` absorbs
        // into partial_chord. The trie returns Partial because
        // `[d, f, *]` is bound (find-char wildcard).
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        assert!(matches!(
            r,
            Action::AbsorbPartialChord(c) if c == KeyChord::char('f')
        ));
    }

    #[test]
    fn d_unrecognised_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d')],
            &ev(KeyCode::Char('Q'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }

    /// Doubled-operator under the `g` prefix: `gUU` -> linewise
    /// upper. The prefix walk is `[g, U, U]`.
    #[test]
    fn g_uu_resolves_to_upper_current_line() {
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
    fn g_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(g)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::char('g')
        ));
    }

    #[test]
    fn z_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(z)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::char('z')
        ));
    }

    #[test]
    fn gg_resolves_to_goto_first_line() {
        let (h, b, _) = populated_handle();
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
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.lsp_definition_request),
            other => panic!("expected Invoke(lsp_definition_request), got {other:?}"),
        }
    }

    #[test]
    fn gu_invokes_absorb_operator_lower() {
        // Slice 8.i.4.c: `gu` (with prefix [g]) resolves to
        // Invoke(absorb_operator_lower).
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.absorb_operator_lower),
            other => panic!("expected Invoke(absorb_operator_lower), got {other:?}"),
        }
    }

    #[test]
    fn g_capital_j_resolves_to_join_lines_without_space() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('g')],
            &ev(KeyCode::Char('J'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.join_lines_bare),
            other => panic!("expected Invoke(join_lines_bare), got {other:?}"),
        }
    }

    #[test]
    fn zz_centers_cursor() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_center),
            other => panic!("expected Invoke(scroll_cursor_to_center), got {other:?}"),
        }
    }

    #[test]
    fn z_dot_aliases_zz() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('.'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_center),
            other => panic!("expected Invoke(scroll_cursor_to_center), got {other:?}"),
        }
    }

    #[test]
    fn z_enter_aliases_zt() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Enter, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_top),
            other => panic!("expected Invoke(scroll_cursor_to_top), got {other:?}"),
        }
    }

    #[test]
    fn z_dash_aliases_zb() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('-'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.scroll_cursor_to_bottom),
            other => panic!("expected Invoke(scroll_cursor_to_bottom), got {other:?}"),
        }
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.toggle_fold_at_cursor),
            other => panic!("expected Invoke(toggle_fold_at_cursor), got {other:?}"),
        }
    }

    #[test]
    fn z_unrecognized_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }

    #[test]
    fn z_esc_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('z')],
            &ev(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }

    /// Slice 8.g.v: `q` outside macro recording arms
    /// `Pending::AfterMacroStart`. The recording-state-dependent
    /// branch (`StopMacroRecord` when already recording) lives
    /// in `compute_normal_action` as a short-circuit before the
    /// trie lookup -- the registry doesn't see App state.
    #[test]
    fn q_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(q)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::char('q')
        ));
    }

    // ---- Slice 8.g.v: wildcard chord paths ----

    #[test]
    fn ma_resolves_to_set_mark_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('m')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.set_mark);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(set_mark, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn m_invalid_passes_char_to_actionspec() {
        // Slice 8.i.3: validation moved from the dispatcher to
        // the bound `ActionSpec`. The dispatcher returns
        // `Invoke(set_mark)` with the captured `'!'` regardless
        // of validity; the spec returns `Effect::None` for
        // non-alphanumeric chars and `App::apply` clears the
        // pending state on every Invoke.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('m')],
            &ev(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.set_mark);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('!')));
            }
            other => panic!("expected Invoke(set_mark, Char('!')), got {other:?}"),
        }
    }

    #[test]
    fn apostrophe_a_resolves_to_jump_mark_line_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('\'')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.jump_to_mark_line);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(jump_to_mark_line, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn backtick_a_resolves_to_jump_mark_exact_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('`')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.jump_to_mark_exact);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(jump_to_mark_exact, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn quote_a_selects_named_register_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(select_register, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn quote_plus_selects_system_register() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('+'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('+')));
            }
            other => panic!("expected Invoke(select_register, Char('+')), got {other:?}"),
        }
    }

    #[test]
    fn quote_zero_selects_numbered_register_zero() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('0'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('0')));
            }
            other => panic!("expected Invoke(select_register, Char('0')), got {other:?}"),
        }
    }

    #[test]
    fn quote_invalid_passes_char_to_actionspec() {
        // Slice 8.i.3: validation lives in the bound `ActionSpec`,
        // not the dispatcher. The dispatched `Invoke` carries the
        // captured `'!'`; the spec returns `Effect::None` for chars
        // that don't name a register.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('"')],
            &ev(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.select_register);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('!')));
            }
            other => panic!("expected Invoke(select_register, Char('!')), got {other:?}"),
        }
    }

    #[test]
    fn qa_starts_macro_record_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('q')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.start_macro_record);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(start_macro_record, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn at_at_plays_last_macro() {
        // The dispatcher returns `Invoke(play_macro, Char('@'))`;
        // the bound `ActionSpec` reads `@` and produces
        // `AppEffect::PlayLastMacro` rather than `PlayMacro('@')`.
        // Slice 8.i.3 moved this branching from the dispatcher's
        // legacy substituter into the spec.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('@')],
            &ev(KeyCode::Char('@'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.play_macro);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('@')));
            }
            other => panic!("expected Invoke(play_macro, Char('@')), got {other:?}"),
        }
    }

    #[test]
    fn at_a_plays_macro_a() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('@')],
            &ev(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, a.play_macro);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('a')));
            }
            other => panic!("expected Invoke(play_macro, Char('a')), got {other:?}"),
        }
    }

    #[test]
    fn f_x_resolves_to_find_char_forward_with_args() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('f')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.find_char_forward.0);
                assert!(matches!(inv.args, lattice_grammar::args::Args::Char('X')));
            }
            other => panic!("expected Invoke(find_char_forward, Char('X')), got {other:?}"),
        }
    }

    #[test]
    fn dfx_resolves_to_delete_with_find_char_target() {
        // `df<X>` -- delete forward up to and including 'X'.
        let (h, b, _) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('d'), KeyChord::char('f')],
            &ev(KeyCode::Char('X'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => {
                assert_eq!(inv.command, b.delete.0);
                match inv.target {
                    Some(Target::Motion(m, args)) => {
                        assert_eq!(m, b.find_char_forward);
                        assert!(matches!(args, lattice_grammar::args::Args::Char('X')));
                    }
                    other => panic!("expected Motion(find_char_forward, Char('X')), got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Wildcard rejects modifier-bearing chords (per
    /// `keymap_trie`'s wildcard rule). `f<C-x>` is unbound and
    /// the dispatcher drops the pending state -- a documented
    /// drift from legacy `resolve_after_find_char`, which
    /// accepted any `KeyCode::Char(c)` regardless of modifiers.
    /// Terminals don't typically emit `f<C-x>` and the
    /// alternative chord representation is the trie's invariant.
    #[test]
    fn f_ctrl_x_falls_through_to_drop_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::char('f')],
            &ev(KeyCode::Char('x'), KeyModifiers::CONTROL),
        );
        assert!(matches!(r, Action::None));
    }

    /// `<S-h>`'s SHIFT is stripped by `KeyChord::from_event`
    /// for bare letters (case carries the bit), so the trie
    /// only needs `(Char('h'), NONE)`. Pin that here.
    #[test]
    fn shift_h_resolves_via_lowercase_chord() {
        let (h, b, _) = populated_handle();
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
        let (h, b, _) = populated_handle();
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
        // Slice 8.i.4.d: AbsorbPartialChord is the new "non-Invoke
        // pass-through" attach_count case (was SetPending(_)).
        let r = attach_count(Action::AbsorbPartialChord(KeyChord::char('g')), 5, 0);
        assert!(matches!(r, Action::AbsorbPartialChord(_)));
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

    // ---- Slice 8.g.vi: CTRL chords + <C-w> sub-tree ----

    #[test]
    fn ctrl_d_resolves_to_line_down_count_ten() {
        let (h, b, _) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('d'), KeyModifiers::CONTROL));
        match r {
            Some(Action::Invoke(inv)) => {
                assert_eq!(inv.command, b.line_down.0);
                assert_eq!(inv.count, Some(lattice_grammar::command::Count(10)));
            }
            other => panic!("expected Invoke(line_down, count=10), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_o_resolves_to_jump_history_back() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('o'), KeyModifiers::CONTROL));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.jump_history_back),
            other => panic!("expected Invoke(jump_history_back), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_v_enters_blockwise_visual() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('v'), KeyModifiers::CONTROL));
        match r {
            Some(Action::Invoke(inv)) => assert_eq!(inv.command, a.enter_visual_blockwise),
            other => panic!("expected Invoke(enter_visual_blockwise), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_absorbs_partial_chord() {
        // Slice 8.i.4.a: trie's `Partial` -> `AbsorbPartialChord(<C-w>)`.
        let (h, _, _a) = populated_handle();
        let r = lookup_normal(&h, &ev(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(matches!(
            r,
            Some(Action::AbsorbPartialChord(c)) if c == KeyChord::ctrl('w')
        ));
    }

    #[test]
    fn ctrl_w_then_l_navigates_pane_right() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_right),
            other => panic!("expected Invoke(navigate_pane_right), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_ctrl_l_also_navigates_pane_right() {
        // Vim accepts ctrl-modified second keys after `<C-w>`
        // (sticky-prefix muscle memory).
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_right),
            other => panic!("expected Invoke(navigate_pane_right), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_arrow_left_navigates_pane_left() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Left, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_left),
            other => panic!("expected Invoke(navigate_pane_left), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_backspace_navigates_pane_left() {
        // Many terminals collapse `<C-h>` to Backspace; the
        // bare-Backspace path covers that.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Backspace, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.navigate_pane_left),
            other => panic!("expected Invoke(navigate_pane_left), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_tab_cycles_to_next_pane() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Tab, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.next_pane),
            other => panic!("expected Invoke(next_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_back_tab_cycles_to_prev_pane() {
        // BackTab normalises to chord `(Tab, SHIFT)` via
        // `KeyChord::from_event`; the trie has the explicit
        // `<S-Tab>` registration under `[<C-w>]`.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::BackTab, KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.prev_pane),
            other => panic!("expected Invoke(prev_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_v_splits_vertical() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.split_pane_vertical),
            other => panic!("expected Invoke(split_pane_vertical), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_capital_s_splits_horizontal() {
        // `<C-w>S` is a legacy alias for `<C-w>s`.
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('S'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.split_pane_horizontal),
            other => panic!("expected Invoke(split_pane_horizontal), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_q_closes_pane() {
        let (h, _, a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Char('q'), KeyModifiers::NONE),
        );
        match r {
            Action::Invoke(inv) => assert_eq!(inv.command, a.close_pane),
            other => panic!("expected Invoke(close_pane), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_w_then_esc_drops_pending() {
        let (h, _, _a) = populated_handle();
        let r = lookup_normal_with_prefix(
            &h,
            &[KeyChord::ctrl('w')],
            &ev(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(r, Action::None));
    }
}
