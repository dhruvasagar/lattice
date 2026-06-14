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
//! chords. To bridge, [`dispatch_insert`] runs a
//! mode-specific normalisation pass before lookup:
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
        &[lit(KeyChord::ctrl(' '))],
        CommandInvocation::of(actions.completion_trigger),
        source(),
    );
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
        &[lit(KeyChord::ctrl(' '))],
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
    if !partial_chord.is_empty() {
        let chord = normalize_for_insert_lookup(*chord);
        let mut path: Vec<KeyChord> = partial_chord.to_vec();
        path.push(chord);
        return match handle.lookup_with_context(BindingMode::Insert, &path, active_minor_modes) {
            LookupResult::Bound { command, captured } => {
                bound_or_fall_through(handle, &path, active_minor_modes, &command, &captured)
            }
            _ => Action::None,
        };
    }

    let chord = normalize_for_insert_lookup(*chord);
    let path = [chord];
    match handle.lookup_with_context(BindingMode::Insert, &path, active_minor_modes) {
        LookupResult::Bound { command, captured } => {
            bound_or_fall_through(handle, &path, active_minor_modes, &command, &captured)
        }
        LookupResult::Partial => {
            // Slice 8.i.4.b: every trie `Partial` in Insert mode
            // (currently only `<C-x>`) absorbs into
            // `App::partial_chord` via `AbsorbPartialChord`. The
            // next keystroke runs with this stack as prefix and
            // hits the trie's resolved `[<C-x>, <C-o>]` /
            // `[<C-x>, <C-s>]` binding.
            Action::AbsorbPartialChord(chord)
        }
        LookupResult::Unbound => literal_text_fallback(&chord),
    }
}

/// SN.3c.2b: resolve a `Bound` result into an `Action`, honoring
/// `fall_through`. When the bound binding is `fall_through` and lives on
/// a `MinorMode(m)` layer, run its action AND THEN re-resolve the same
/// chord with `m` peeled out of the active set, chaining the native
/// binding's action after it. Bounded: each hop removes a layer, so the
/// recursion terminates at `Builtin` — it cannot loop the way vim's
/// `:map` can.
fn bound_or_fall_through(
    handle: &KeymapHandle,
    path: &[KeyChord],
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
        KeymapLayer::MinorMode(m) => {
            active_minor_modes.iter().copied().filter(|x| *x != m).collect()
        }
        // A fall_through binding on a non-minor layer has nothing above
        // it to peel; treat as no continuation (defensive — entries set
        // fall_through only on mode layers).
        _ => return action,
    };
    chain_actions(action, resolve_native_action(handle, path, &peeled))
}

/// SN.3c.2b: re-resolve a chord for a fall-through continuation,
/// returning the native binding's `Action` (recursing if that binding
/// is itself `fall_through`). `Unbound` / `Partial` → `Action::None`:
/// the mode action already ran; there is simply nothing native to
/// continue to (so we must NOT fall back to literal-text insertion
/// here, which would type the chord's character).
fn resolve_native_action(
    handle: &KeymapHandle,
    path: &[KeyChord],
    active_minor_modes: &[ModeId],
) -> Action {
    match handle.lookup_with_context(BindingMode::Insert, path, active_minor_modes) {
        LookupResult::Bound { command, captured } => {
            bound_or_fall_through(handle, path, active_minor_modes, &command, &captured)
        }
        _ => Action::None,
    }
}

/// SN.3c.2b: sequence two actions, flattening nested chains and
/// dropping a `None` continuation so a single-action result stays a
/// plain `Action` (no `Chain` wrapper unless there is genuinely a
/// chain).
fn chain_actions(first: Action, rest: Action) -> Action {
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
fn normalize_for_insert_lookup(chord: KeyChord) -> KeyChord {
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
        _ => Action::None,
    }
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
