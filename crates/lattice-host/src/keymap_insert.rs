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
//! - `<Esc>` -> `Action::EnterMode(Normal)`
//! - `<BS>` -> [`Action::DeleteCharBackward`]
//! - `<CR>` -> `Action::Insert("\n")`
//! - `<Tab>` -> `Action::Insert("\t")`
//! - `<C-Space>` -> [`Action::CompletionTrigger`]
//! - `[<C-x>, <C-o>]` -> [`Action::CompletionTrigger`] (omni-completion)
//!
//! SN.3c.1 (2026-06-14): `[<C-x>, <C-s>]` (snippet-expand) is no
//! longer a Builtin binding — it lives on `snippet-mode`'s `keymap()`
//! (`KeymapLayer::MinorMode("snippet-mode")`). `<C-x>` stays a partial
//! prefix because that mode's layer (boot-pushed) provides the
//! `<C-x><C-s>` terminal.
//!
//! `<C-x>` itself is a *partial* trie node (no terminal binding;
//! children only). Lookup at `[<C-x>]` returns
//! [`LookupResult::Partial`]; [`dispatch_insert`] translates that
//! into `Action::SetPending(Pending::AfterCtrlX)`. The next
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
//! Insert; the dispatcher's private `literal_text_fallback` returns
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
//! chords. To bridge, [`dispatch_insert`] normalizes per the table
//! below -- but see OS.0b just after it before assuming this strip
//! is unconditional:
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
//! **OS.0b (2026-09-06): the strip is a fallback, not a precondition.**
//! No BUILTIN Insert binding (base or overlay) uses ALT or SUPER, so
//! this table's normalized form is exactly what every builtin chord
//! still resolves to. But the `modes` WIT seam makes no such promise to
//! a plugin or host mode registering its OWN Insert-mode binding
//! (`binding-mode: insert`), and nothing in registration rejects an
//! ALT/SUPER-bearing Insert chord -- so a mode or plugin CAN bind one.
//! Stripping the modifiers before every lookup, unconditionally, used
//! to mean such a binding registered correctly and then could never
//! fire: real keypresses were normalized away before reaching it. Every
//! lookup site now tries the chord AS PRESSED first
//! ([`lookup_insert_chord`]) and falls back to this table's normalized
//! form only when the raw lookup finds nothing -- so a deliberately
//! ALT/SUPER-bearing binding is reachable, and a chord that was never
//! going to match either way still costs exactly one lookup.
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

use lattice_grammar::CommandInvocation;
use lattice_grammar::SourceLocation;
use lattice_mode::mode::ModeId;
use lattice_protocol::ids::CommandId;

use crate::action::Action;
use crate::actions::ActionIds;
use crate::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, KeymapTrie, LookupResult};

/// K.1.b (2026-05-30): canonical `ModeId` for the
/// completion-popup minor-mode keymap layer. Used both by
/// `completion_popup_layer_bindings` (the per-binding
/// provenance tag at build time) and by
/// `App::sync_keymap_overlays` (the push site). Centralised
/// here so the two stay in lockstep — drift would surface as
/// `:describe-key` showing the wrong mode name.
pub fn completion_popup_mode_id() -> ModeId {
    ModeId::new("completion-popup-mode")
}

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

    // YR.5: vim's insert-register. `<C-r>` is free in Insert — Normal's
    // `<C-r>` is redo and stays that way — so nothing is displaced.
    //
    // The two paths share a prefix and do not shadow each other, which is
    // worth stating rather than trusting: the trie tries an exact child
    // before the char wildcard, AND a modifier-bearing chord never
    // matches the wildcard at all. So `<C-r><C-r>` takes the literal path
    // and `<C-r>a` the wildcard, with no ordering dependency between the
    // two binds. That is the shadowing class SU.3e spent a slice on.
    //
    // These belong on the BASE Insert layer, not the completion-popup
    // overlay a few functions down — that trie is live only while the
    // popup is open, so binding there would make `<C-r>` work only while
    // completing. It was written there first and the tests caught it.
    handle.bind(
        layer,
        mode,
        &[
            ChordPattern::Literal(KeyChord::ctrl('r')),
            ChordPattern::Literal(KeyChord::ctrl('r')),
        ],
        CommandInvocation::of(actions.open_yank_picker),
        source(),
    );
    handle.bind(
        layer,
        mode,
        &[
            ChordPattern::Literal(KeyChord::ctrl('r')),
            ChordPattern::CharLiteral,
        ],
        CommandInvocation::of(actions.insert_register),
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
        // `Special(Space) + CTRL`, NOT `KeyChord::ctrl(' ')`. The two are
        // different chords, and only this one is what
        // `parse_chord_sequence("<C-Space>")` produces — which is what every
        // plugin binding and user keymap goes through. Binding the other form
        // worked only while the TUI's key decoding was wrong in the matching
        // way; it is a chord nothing can type now.
        &[lit(ctrl_space())],
        CommandInvocation::of(actions.completion_trigger),
        source(),
    );
    // Readline/vim Insert-mode line editing — general across every buffer.
    // <C-a>/<C-e> line ends, <C-b>/<C-f> char nav, <C-w>/<C-u>/<C-k> deletes,
    // <C-t>/<C-d> indent/dedent. (<C-a>/<C-e>/<C-k> deliberately take the
    // readline meaning over vim's rarely-used Insert bindings.)
    for (ch, id) in [
        ('a', actions.insert_cursor_line_start),
        ('e', actions.insert_cursor_line_end),
        ('b', actions.insert_cursor_char_left),
        ('f', actions.insert_cursor_char_right),
        ('w', actions.insert_delete_word_backward),
        ('u', actions.insert_delete_to_line_start),
        ('k', actions.insert_kill_to_line_end),
        ('t', actions.insert_indent_line),
        ('d', actions.insert_dedent_line),
    ] {
        handle.bind(
            layer,
            mode,
            &[lit(KeyChord::ctrl(ch))],
            CommandInvocation::of(id),
            source(),
        );
    }
    // Arrow / Home / End cursor navigation in Insert mode — the same
    // char/line motions as the `<C-b>`/`<C-f>`/`<C-a>`/`<C-e>` readline
    // chords, on the keys most users reach for first. Vim-faithful (arrows
    // move the caret in Insert) and general across every Insert buffer, so
    // the buffer-backed `:` line (MB.1) gets mid-line editing by arrow key
    // for free.
    for (key, id) in [
        (SpecialKey::Left, actions.insert_cursor_char_left),
        (SpecialKey::Right, actions.insert_cursor_char_right),
        (SpecialKey::Home, actions.insert_cursor_line_start),
        (SpecialKey::End, actions.insert_cursor_line_end),
    ] {
        handle.bind(
            layer,
            mode,
            &[lit_special(key)],
            CommandInvocation::of(id),
            source(),
        );
    }
    // CSM.K1: `<C-x><C-o>` (vim omni-completion) retired.
    // `<C-Space>` is the sole popup-open trigger; per-source
    // filter chords live inside `completion-popup-mode` (CSM.K2).
    // SN.3c.1 (2026-06-14): `<C-x><C-s>` (snippet-expand) moved off
    // Builtin onto `snippet-mode`'s `keymap()` at
    // `KeymapLayer::MinorMode("snippet-mode")` — the chord choice now
    // lives with the mode that owns the behavior
    // (`feedback_mode_owns_its_surface`). `<C-x>` is no longer a live
    // Builtin prefix; the merged trie still resolves it as a `Partial`
    // through the (boot-pushed) snippet-mode layer, so the two-key
    // chord still absorbs + dispatches via `dispatch_insert`.
}

/// Build the completion-popup minor-mode layer's binding set.
/// Wrapped into the registry by `App::push_completion_popup_layer`
/// when the popup opens; popped when the popup closes.
///
/// Returns one trie keyed under `BindingMode::Insert` -- the only
/// mode the popup is active in. The registry's merge picks up
/// every entry under that mode whenever the layer is pushed.
pub fn completion_popup_layer_bindings(actions: &ActionIds) -> HashMap<BindingMode, KeymapTrie> {
    let mut trie = KeymapTrie::new();
    // K.1.b: per-binding provenance tag — same ModeId the
    // push site uses, so `:describe-key` shows the binding's
    // layer correctly.
    let layer = KeymapLayer::MinorMode(completion_popup_mode_id());

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
    // CSM.K2: inside the popup, `<C-Space>` clears the active
    // source filter (mirrors vim's "show everything again"
    // intent). The unfiltered insert-mode trigger lives one
    // layer down (base insert keymap) and is shadowed while
    // the popup is open.
    bind_invocation(
        &mut trie,
        layer,
        &[lit(ctrl_space())],
        actions.completion_filter_clear,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('d'))],
        actions.completion_toggle_docs,
    );
    // CSM.K2: docs-scroll moved off `<C-f>`/`<C-b>` (those now
    // act as filter chords -- path / buffer-words). Docs scroll
    // is on PageDown / PageUp, which mirrors the page-wise
    // semantics without colliding with the chord namespace.
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::PageDown)],
        actions.completion_docs_scroll_down,
    );
    bind_invocation(
        &mut trie,
        layer,
        &[lit_special(SpecialKey::PageUp)],
        actions.completion_docs_scroll_up,
    );
    // CSM.K2: single-key filter chords inside the popup. Each
    // chord targets a specific completion source -- the static
    // `Args::String(SourceId)` payload is folded into the bound
    // invocation, so a single action covers every source.
    use lattice_completion::insert::{
        BufferWordsSource, LSP_COMPLETION_SOURCE_ID, PATH_SOURCE_ID, SNIPPET_SOURCE_ID,
        TREE_SITTER_SYMBOL_SOURCE_ID,
    };
    bind_invocation_with_string(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('b'))],
        actions.completion_filter_to_source,
        BufferWordsSource::ID,
    );
    bind_invocation_with_string(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('o'))],
        actions.completion_filter_to_source,
        LSP_COMPLETION_SOURCE_ID,
    );
    bind_invocation_with_string(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('f'))],
        actions.completion_filter_to_source,
        PATH_SOURCE_ID,
    );
    bind_invocation_with_string(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('t'))],
        actions.completion_filter_to_source,
        TREE_SITTER_SYMBOL_SOURCE_ID,
    );
    bind_invocation_with_string(
        &mut trie,
        layer,
        &[lit(KeyChord::ctrl('s'))],
        actions.completion_filter_to_source,
        SNIPPET_SOURCE_ID,
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
///    - `Unbound` -> private `literal_text_fallback` for printable
///      chars without CONTROL; otherwise `Action::None`.
pub fn dispatch_insert(
    handle: &KeymapHandle,
    chord: &KeyChord,
    partial_chord: &[KeyChord],
    active_minor_modes: &[ModeId],
) -> Action {
    // SN.3c.2a (2026-06-14): Insert-mode dispatch is now K.1.c-gated,
    // mirroring `translate_normal`. Previously this used
    // `handle.lookup`, which folds in EVERY registered minor-mode
    // layer unconditionally (`registry.rs`: `lookup` treats all
    // `minor_mode_tries` keys as active) — so an inactive minor mode's
    // Insert bindings (e.g. `active-snippet-mode`'s `<Tab>` / `<Esc>`)
    // shadowed base Insert in every buffer. Routing through
    // `lookup_with_context` with the active buffer's minor set scopes
    // those bindings to buffers where the mode is actually active, the
    // same per-buffer guarantee Normal mode already had.
    //
    // Slice 8.i.4: partial-chord dispatch wins when a previous
    // keystroke absorbed a prefix into `App::partial_chord`.
    // This drives the `<C-x>` family (`<C-x><C-o>` /
    // `<C-x><C-s>`) and any future Insert-mode multi-key chord.
    //
    // OS.0b: every lookup below goes through `lookup_insert_chord`,
    // which tries the chord AS PRESSED first and only falls back to the
    // normalized form when the raw lookup found nothing. See that
    // function's docs for why raw must go first.
    if !partial_chord.is_empty() {
        let lookup = lookup_insert_chord(
            handle,
            BindingMode::Insert,
            partial_chord,
            *chord,
            active_minor_modes,
        );
        return match lookup.result {
            LookupResult::Bound { command, captured } => bound_or_fall_through(
                handle,
                partial_chord,
                *chord,
                active_minor_modes,
                &command,
                &captured,
            ),
            _ => Action::None,
        };
    }

    let lookup = lookup_insert_chord(handle, BindingMode::Insert, &[], *chord, active_minor_modes);
    match lookup.result {
        LookupResult::Bound { command, captured } => {
            bound_or_fall_through(handle, &[], *chord, active_minor_modes, &command, &captured)
        }
        LookupResult::Partial => {
            // Slice 8.i.4.b: every trie `Partial` in Insert mode
            // (currently only `<C-x>`) absorbs into
            // `App::partial_chord` via `AbsorbPartialChord`. The
            // next keystroke runs with this stack as prefix and
            // hits the trie's resolved `[<C-x>, <C-o>]` /
            // `[<C-x>, <C-s>]` binding. `lookup.resolved` is whichever
            // form (raw or normalized) actually matched the `Partial`
            // node, so the next keystroke's prefix is the one the trie
            // will recognize.
            Action::AbsorbPartialChord(lookup.resolved)
        }
        LookupResult::Unbound => literal_text_fallback(chord),
    }
}

/// OS.0b: the outcome of [`lookup_insert_chord`] — the `LookupResult`
/// plus which form of the incoming chord (as pressed, or with
/// ALT/SUPER stripped) actually produced it. A caller that continues a
/// multi-key sequence or re-resolves a fall-through continuation must
/// follow up against the SAME form; silently switching to the other one
/// would look up a chord the trie was never asked about.
pub(crate) struct InsertLookup {
    pub(crate) result: LookupResult,
    pub(crate) resolved: KeyChord,
}

/// OS.0b: look a chord up **as it arrived** first, so a mode or plugin
/// layer that deliberately binds an ALT/SUPER-bearing chord is
/// reachable — the `modes` WIT seam makes no promise against it, unlike
/// the BUILTIN catalog this module's normalize table was designed for
/// (see the module docstring). Fall back to the normalized form only
/// when the raw lookup found nothing AND normalizing would actually
/// change the chord, so a chord carrying neither ALT nor SUPER costs
/// exactly one lookup, as it always did.
///
/// `Partial` counts as a raw hit: an ALT/SUPER-bearing PREFIX is a
/// deliberate registration, and falling back mid-sequence would strand
/// its continuation.
///
/// Shared by `dispatch_insert`'s three lookup sites (the partial-chord
/// branch, the fresh-chord branch, and `resolve_native_action`'s
/// fall-through re-resolve) and by `keymap_select::minor_select_action`
/// (SN.3d.4), which keys its minor bindings the same way and needs the
/// same raw-first rule.
pub(crate) fn lookup_insert_chord(
    handle: &KeymapHandle,
    mode: BindingMode,
    prefix: &[KeyChord],
    chord: KeyChord,
    active_minor_modes: &[ModeId],
) -> InsertLookup {
    let mut raw_path: Vec<KeyChord> = prefix.to_vec();
    raw_path.push(chord);
    let raw = handle.lookup_with_context(mode, &raw_path, active_minor_modes);
    if matches!(raw, LookupResult::Bound { .. } | LookupResult::Partial) {
        return InsertLookup {
            result: raw,
            resolved: chord,
        };
    }
    let normalized = normalize_for_insert_lookup(chord);
    if normalized == chord {
        // Nothing to fall back to -- the raw result (Unbound, since the
        // Bound/Partial case returned above) IS the answer.
        return InsertLookup {
            result: raw,
            resolved: chord,
        };
    }
    let mut normalized_path: Vec<KeyChord> = prefix.to_vec();
    normalized_path.push(normalized);
    InsertLookup {
        result: handle.lookup_with_context(mode, &normalized_path, active_minor_modes),
        resolved: normalized,
    }
}

/// SN.3c.2b: resolve a `Bound` result into an `Action`, honoring
/// `fall_through`. When the bound binding is `fall_through` and lives on
/// a `MinorMode(m)` layer, run its action AND THEN re-resolve the same
/// chord with `m` peeled out of the active set, chaining the native
/// binding's action after it. Bounded: each hop removes a layer, so the
/// recursion terminates at `Builtin` — it cannot loop the way vim's
/// `:map` can.
///
/// OS.0b: takes the ORIGINAL incoming `chord` (not a pre-resolved path)
/// so the fall-through re-resolve can independently try raw-then-
/// normalized against the peeled active set — the layer that bound the
/// ALT-bearing chord may be gone, but a lower layer's NORMALIZED
/// binding (e.g. Builtin's plain `<CR>`) should still be reachable.
fn bound_or_fall_through(
    handle: &KeymapHandle,
    prefix: &[KeyChord],
    chord: KeyChord,
    active_minor_modes: &[ModeId],
    command: &Arc<BoundCommand>,
    captured: &[char],
) -> Action {
    let action = action_from_bound(command, captured);
    if !command.fall_through {
        return action;
    }
    // Peel the binding's own mode out of the active set and re-resolve
    // the same chord against the layers below — the native binding.
    let peeled: Vec<ModeId> = match command.layer {
        KeymapLayer::MinorMode(m) => active_minor_modes
            .iter()
            .copied()
            .filter(|x| *x != m)
            .collect(),
        // A fall_through binding on a non-minor layer has nothing above
        // it to peel; treat as no continuation (defensive — entries set
        // fall_through only on mode layers).
        _ => return action,
    };
    chain_actions(
        action,
        resolve_native_action(handle, prefix, chord, &peeled),
    )
}

/// SN.3c.2b: re-resolve a chord for a fall-through continuation,
/// returning the native binding's `Action` (recursing if that binding
/// is itself `fall_through`). `Unbound` / `Partial` → `Action::None`:
/// the mode action already ran; there is simply nothing native to
/// continue to (so we must NOT fall back to literal-text insertion
/// here, which would type the chord's character).
fn resolve_native_action(
    handle: &KeymapHandle,
    prefix: &[KeyChord],
    chord: KeyChord,
    active_minor_modes: &[ModeId],
) -> Action {
    let lookup = lookup_insert_chord(
        handle,
        BindingMode::Insert,
        prefix,
        chord,
        active_minor_modes,
    );
    match lookup.result {
        LookupResult::Bound { command, captured } => bound_or_fall_through(
            handle,
            prefix,
            chord,
            active_minor_modes,
            &command,
            &captured,
        ),
        _ => Action::None,
    }
}

/// SN.3c.2b: sequence two actions, flattening nested chains and
/// dropping a `None` continuation so a single-action result stays a
/// plain `Action` (no `Chain` wrapper unless there is genuinely a
/// chain).
///
/// SN.3d.4: `pub(crate)` so Select-mode fall-through
/// (`keymap_select::minor_select_action`) reuses the same chaining
/// primitive — a `fall_through` minor binding sequences its mode
/// action with the native continuation identically in both modes.
pub(crate) fn chain_actions(first: Action, rest: Action) -> Action {
    match rest {
        Action::None => first,
        Action::Chain(mut v) => {
            let mut out = Vec::with_capacity(v.len() + 1);
            out.push(first);
            out.append(&mut v);
            Action::Chain(out)
        }
        other => Action::Chain(vec![first, other]),
    }
}

/// Mode-specific modifier strip. See module docstring's table.
///
/// SN.3d.4: `pub(crate)` so the Select-mode minor-binding lookup
/// (`keymap_select::minor_select_action`) normalizes chords the SAME
/// way — minor-mode bindings (e.g. the snippet `<Tab>` / `<S-Tab>` /
/// `<Esc>`) are keyed identically regardless of the host modal, so
/// `<S-Tab>` must keep SHIFT in Select too (the base-Select normalize
/// strips it, which would collapse `<S-Tab>` into `<Tab>`).
pub(crate) fn normalize_for_insert_lookup(chord: KeyChord) -> KeyChord {
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
fn literal_text_fallback(chord: &KeyChord) -> Action {
    if chord.mods.ctrl() {
        return Action::None;
    }
    match chord.key {
        KeyKind::Char(c) => Action::Insert(c.to_string()),
        // A modified space that no binding claimed still types a space.
        //
        // Without this arm, promoting a modified space to `Special(Space)`
        // would make Shift+Space stop inserting anything — it would reach here
        // as a `Special` and fall out as `Action::None`. Narrow (a Unix
        // terminal reports Shift+Space with no modifier at all; the Windows
        // console does not), but "a key that used to type a space now does
        // nothing" is a worse bug than the one being fixed, and GPUI has been
        // silently doing exactly that.
        KeyKind::Special(SpecialKey::Space) => Action::Insert(" ".to_string()),
        _ => Action::None,
    }
}

/// The `<C-Space>` chord as the PARSER spells it.
///
/// `KeyChord::ctrl(' ')` is `Char(' ') + CTRL` and is a different chord — one
/// no real keypress produces. Anything binding `<C-Space>` must agree with
/// `parse_chord_sequence`, which is the path every plugin and user binding
/// takes.
fn ctrl_space() -> KeyChord {
    KeyChord::new(KeyKind::Special(SpecialKey::Space), KeyMods::CTRL)
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

/// CSM.K2: like `bind_invocation` but folds a constant
/// `Args::String(...)` payload into the bound invocation.
/// Used by the popup-mode filter chords (`<C-b>` ->
/// `completion-filter-to-source("gen:buffer-words")`, etc.)
/// so the `captured_string_action` helper can dispatch to the
/// right `AppEffect` without a separate action per source.
fn bind_invocation_with_string(
    trie: &mut KeymapTrie,
    layer: KeymapLayer,
    path: &[ChordPattern],
    command: CommandId,
    payload: &str,
) {
    let inv = CommandInvocation::of(command)
        .with_args(lattice_grammar::Args::String(payload.to_string()));
    let bound = Arc::new(BoundCommand::from_invocation(inv, source(), layer));
    trie.insert(path, bound);
}

/// OS.0b regression tests: `dispatch_insert`'s raw-then-fallback fix
/// must not change any of the behaviour that worked before it. Each
/// test below pins one fact the fix is not allowed to break; see
/// `.superpowers/sdd/org-structure-editing/os0b-brief.md` for why these
/// four were chosen. `crates/lattice-host/tests/plugin_insert_mode_chords.rs`
/// covers the fix's actual acceptance criterion (an ALT-bearing plugin
/// binding reaching apply-action end to end); these are host-local unit
/// tests of the dispatch function itself.
#[cfg(test)]
mod os0b_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

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

    /// A handle carrying only the Builtin Insert catalog — no minor
    /// modes. Enough to test the raw-then-fallback strip against the
    /// bindings that motivated it in the first place.
    fn builtin_handle() -> KeymapHandle {
        let h = KeymapHandle::new();
        register_insert_bindings(&h, shared_actions());
        h
    }

    fn alt(key: KeyKind) -> KeyChord {
        KeyChord::new(key, KeyMods::ALT)
    }

    fn invoked_command(action: &Action) -> CommandId {
        match action {
            Action::Invoke(inv) => inv.command,
            other => panic!("expected Action::Invoke, got {other:?}"),
        }
    }

    /// `<M-CR>` unbound anywhere (no mode claims it) must still fall
    /// back to the normalized `<CR>` and reach the Builtin newline —
    /// exactly what worked before OS.0b, now reached via the fallback
    /// arm of `lookup_insert_chord` rather than unconditional stripping.
    #[test]
    fn alt_enter_still_reaches_the_builtin_newline_when_nothing_binds_it() {
        let h = builtin_handle();
        let chord = alt(KeyKind::Special(SpecialKey::Enter));
        let action = dispatch_insert(&h, &chord, &[], &[]);
        assert_eq!(
            invoked_command(&action),
            shared_actions().insert_newline,
            "<M-CR> with nothing bound to it must still fall back to <CR>'s builtin newline"
        );
    }

    /// `<M-x>` is unbound both raw and normalized, so it must still hit
    /// the literal-text fallback and type a plain `x` — the raw lookup
    /// trying first must not swallow the printable-fallback path.
    #[test]
    fn alt_x_still_types_a_literal_x() {
        let h = builtin_handle();
        let chord = alt(KeyKind::Char('x'));
        match dispatch_insert(&h, &chord, &[], &[]) {
            Action::Insert(s) => assert_eq!(s, "x"),
            other => panic!("expected a literal 'x' insert, got {other:?}"),
        }
    }

    /// SHIFT was never stripped by `normalize_for_insert_lookup`, so
    /// `<S-Tab>` and `<Tab>` must keep resolving to DIFFERENT bindings
    /// via the raw lookup's first try — the fix must not collapse them
    /// the way stripping ALT/SUPER-only would be a no-op here.
    #[test]
    fn shift_tab_is_unaffected() {
        let h = builtin_handle();
        let mode_id = ModeId::new("os0b-shift-tab-mode");
        let shift_tab_command = CommandId::new(u64::MAX - 1);
        h.bind(
            KeymapLayer::MinorMode(mode_id),
            BindingMode::Insert,
            &[lit(KeyChord::new(
                KeyKind::Special(SpecialKey::Tab),
                KeyMods::SHIFT,
            ))],
            CommandInvocation::of(shift_tab_command),
            source(),
        );

        let shift_tab = KeyChord::new(KeyKind::Special(SpecialKey::Tab), KeyMods::SHIFT);
        let action = dispatch_insert(&h, &shift_tab, &[], &[mode_id]);
        assert_eq!(
            invoked_command(&action),
            shift_tab_command,
            "<S-Tab> must resolve to the minor binding on its own, not fall through to <Tab>"
        );

        let plain_tab = KeyChord::special(SpecialKey::Tab);
        let action = dispatch_insert(&h, &plain_tab, &[], &[mode_id]);
        assert_eq!(
            invoked_command(&action),
            shared_actions().insert_tab,
            "plain <Tab> must stay on the Builtin binding, unaffected by the <S-Tab> minor entry"
        );
    }

    /// The partial-chord branch (a previously-absorbed `<C-x>` prefix)
    /// must get the SAME raw-then-fallback treatment as the fresh-chord
    /// branch. Before OS.0b, the second chord of a multi-key sequence
    /// was normalized (ALT stripped) before ever being appended to the
    /// lookup path, so an ALT-bearing two-chord binding died at its own
    /// prefix even though it registered correctly.
    #[test]
    fn the_ctrl_x_ctrl_o_two_chord_still_resolves() {
        let h = builtin_handle();
        let mode_id = ModeId::new("os0b-two-chord-mode");
        let two_chord_command = CommandId::new(u64::MAX - 2);
        h.bind(
            KeymapLayer::MinorMode(mode_id),
            BindingMode::Insert,
            &[lit(KeyChord::ctrl('x')), lit(alt(KeyKind::Char('o')))],
            CommandInvocation::of(two_chord_command),
            source(),
        );

        // First key: <C-x> absorbs as a partial prefix. It carries no
        // ALT/SUPER, so raw == normalized here and this costs one
        // lookup, same as before OS.0b.
        let ctrl_x = KeyChord::ctrl('x');
        let absorbed = match dispatch_insert(&h, &ctrl_x, &[], &[mode_id]) {
            Action::AbsorbPartialChord(c) => c,
            other => panic!("expected <C-x> to absorb as a partial prefix, got {other:?}"),
        };

        // Second key: <M-o> completes the sequence via the
        // partial-chord branch.
        let alt_o = alt(KeyKind::Char('o'));
        let action = dispatch_insert(&h, &alt_o, &[absorbed], &[mode_id]);
        assert_eq!(
            invoked_command(&action),
            two_chord_command,
            "the two-chord <C-x><M-o> binding must resolve through the partial-chord branch"
        );
    }
}
