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

use lattice_grammar::SourceLocation;
use lattice_grammar::Target;
use lattice_grammar::args::Args;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_protocol::ids::CommandId;
use lattice_syntax::SyntaxTextObjectIds;

use crate::action::{Action, FindKind};
use crate::actions::ActionIds;
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
pub fn register_normal_bindings(
    handle: &KeymapHandle,
    builtins: &Builtins,
    actions: &ActionIds,
    syntax_textobjects: &SyntaxTextObjectIds,
) {
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
        &[lit_char('I')],
        CommandInvocation::of(actions.enter_insert_first_non_blank),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[lit_char('A')],
        CommandInvocation::of(actions.enter_append_end_of_line),
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
    // Issue #29 slice 3 (2026-05-22): tab navigation aliases.
    // `<C-PageDown>` = next tab; `<C-PageUp>` = previous tab.
    // Standard GUI/terminal binding many users expect alongside
    // `gt` / `gT`.
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord {
            key: KeyKind::Special(SpecialKey::PageDown),
            mods: KeyMods::CTRL,
        })],
        CommandInvocation::of(actions.next_tab),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[ChordPattern::Literal(KeyChord {
            key: KeyKind::Special(SpecialKey::PageUp),
            mods: KeyMods::CTRL,
        })],
        CommandInvocation::of(actions.prev_tab),
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
    // Issue #29 (2026-05-22): `gt` next tab, `gT` previous tab.
    // For `{N}gt` (absolute tab target), the count is picked
    // up via the existing chord-count prefix mechanism — the
    // handler `do_goto_tab` reads the count off the dispatched
    // action's payload (set by the count-override hook in
    // dispatch.rs when `count.is_some()`).
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('t')],
        CommandInvocation::of(actions.next_tab),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('T')],
        CommandInvocation::of(actions.prev_tab),
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
    // W.6: display-line motions (soft-wrap aware).
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('j')],
        CommandInvocation::of(actions.display_line_down),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_special(SpecialKey::Down)],
        CommandInvocation::of(actions.display_line_down),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('k')],
        CommandInvocation::of(actions.display_line_up),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_special(SpecialKey::Up)],
        CommandInvocation::of(actions.display_line_up),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('0')],
        CommandInvocation::of(actions.display_line_start),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[g.clone(), lit_char('$')],
        CommandInvocation::of(actions.display_line_end),
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
        syntax_textobjects,
    );
    register_operator_pending(
        handle,
        &[lit_char('c')],
        builtins.change,
        ChordPattern::Literal(KeyChord::char('c')),
        builtins,
        syntax_textobjects,
    );
    register_operator_pending(
        handle,
        &[lit_char('y')],
        builtins.yank,
        ChordPattern::Literal(KeyChord::char('y')),
        builtins,
        syntax_textobjects,
    );
    register_operator_pending(
        handle,
        &[lit_char('>')],
        builtins.indent_right,
        ChordPattern::Literal(KeyChord::char('>')),
        builtins,
        syntax_textobjects,
    );
    register_operator_pending(
        handle,
        &[lit_char('<')],
        builtins.indent_left,
        ChordPattern::Literal(KeyChord::char('<')),
        builtins,
        syntax_textobjects,
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
        syntax_textobjects,
    );
    register_operator_pending(
        handle,
        &[lit_char('g'), lit_char('u')],
        builtins.lower,
        ChordPattern::Literal(KeyChord::char('u')),
        builtins,
        syntax_textobjects,
    );
    register_operator_pending(
        handle,
        &[lit_char('g'), lit_char('~')],
        builtins.toggle_case,
        ChordPattern::Literal(KeyChord::char('~')),
        builtins,
        syntax_textobjects,
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
        // Issue #28 (2026-05-22): split-ratio adjustment.
        (&[lit_char('=')], actions.equalize_panes),
        (&[lit_char('+')], actions.grow_pane_height),
        (&[lit_char('-')], actions.shrink_pane_height),
        (&[lit_char('>')], actions.grow_pane_width),
        (&[lit_char('<')], actions.shrink_pane_width),
        // T4 (2026-05-25): `<C-w>T` — move active pane to new tab.
        (&[lit_char('T')], actions.move_pane_to_new_tab),
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
///
/// N.1.3 (2026-06-10): `pub` so boot can wire a *provider-contributed*
/// operator's chord (the narrow `zn`) into this universal
/// operator-pending layer. The operator SPEC + `apply` are owned by
/// the provider crate; only this chord-wiring lives here, because
/// operator-pending composition needs the host-resolved `Builtins`.
pub fn register_operator_pending(
    handle: &KeymapHandle,
    op_prefix: &[ChordPattern],
    op: lattice_grammar::registry::OperatorId,
    doubled_self: ChordPattern,
    builtins: &Builtins,
    syntax_textobjects: &SyntaxTextObjectIds,
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
        register_text_object_resolutions(
            handle,
            &pending_path,
            op,
            around,
            builtins,
            syntax_textobjects,
        );
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
    syntax_textobjects: &SyntaxTextObjectIds,
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
    // N.1.4c: the structural (tree-sitter) objects resolve through the
    // SAME operator-pending path -- `af`/`if` (function), `ac`/`ic`
    // (class), `aa`/`ia` (parameter), `al`/`il` (loop). Their chord
    // chars (f/c/a/l) are free in the text-object slot: find-char
    // (`df<c>`) lives on a different post-operator path, so `daf` =
    // d -> a(around) -> f(function) never collides. Ownership stays
    // with lattice-syntax (it minted the ids); the host only wires the
    // chord -> id mapping, exactly as it does for the builtin objects.
    let syntax_table: &[(
        &[ChordPattern],
        lattice_grammar::registry::TextObjectId,
        lattice_grammar::registry::TextObjectId,
    )] = &[
        (
            &[lit_char('f')],
            syntax_textobjects.inner_function,
            syntax_textobjects.around_function,
        ),
        (
            &[lit_char('c')],
            syntax_textobjects.inner_class,
            syntax_textobjects.around_class,
        ),
        (
            &[lit_char('a')],
            syntax_textobjects.inner_parameter,
            syntax_textobjects.around_parameter,
        ),
        (
            &[lit_char('l')],
            syntax_textobjects.inner_loop,
            syntax_textobjects.around_loop,
        ),
    ];
    for (chord_aliases, inner_id, around_id) in textobj_table.iter().chain(syntax_table.iter()) {
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
pub fn lookup_normal(
    handle: &KeymapHandle,
    chord: &KeyChord,
    active_minor_modes: &[lattice_mode::ModeId],
) -> Option<Action> {
    let chord = normalize_for_normal_lookup(*chord);
    match handle.lookup_with_context(BindingMode::Normal, &[chord], active_minor_modes) {
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
    chord: &KeyChord,
    active_minor_modes: &[lattice_mode::ModeId],
) -> Action {
    let chord = normalize_for_normal_lookup(*chord);
    let mut path: Vec<KeyChord> = prefix.to_vec();
    path.push(chord);
    match handle.lookup_with_context(BindingMode::Normal, &path, active_minor_modes) {
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
