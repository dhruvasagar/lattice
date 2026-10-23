//! Peer conformance: one set of scenarios, run by every renderer peer.
//!
//! A renderer peer wraps [`Editor`] with its own key entry and its own
//! dispatch entry, and the contract between the two is in
//! `docs/dev/architecture/gui-default.md` §2. The host's own tests cannot
//! check that contract: they drive `Editor` directly and so go around the
//! very code that keeps it. That is how the GUI peer ran for months without
//! `sync_keymap_overlays` — every host test passed, and every TUI test
//! passed, and nothing drove the GUI's dispatch entry with a completion
//! popup open.
//!
//! So the scenarios live here, beside the contract, and each peer runs all of
//! them through its real entry points by implementing [`PeerUnderTest`] and
//! writing one line:
//!
//! ```ignore
//! lattice_host::peer_conformance_tests!(MyPeer);
//! ```
//!
//! **Adding a scenario** is a function in this file plus its name in the
//! macro's list. Both peers pick it up without being touched, and a peer in
//! which it fails does not get to skip it.
//!
//! **What belongs here** is behaviour that must be identical in every peer:
//! the state of the editor after a sequence of keys, and how many times the
//! actor was crossed to get there. Paint does not belong here.
//!
//! The scenarios run inside each peer's own unit tests because that is where
//! a peer exposes an in-process editor (`cfg(test)`); there is no crate that
//! could host them for both. The module is behind the `peer-conformance`
//! feature, which the peers enable from `[dev-dependencies]` only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::action::Action;
use crate::chord::KeyChord;
use crate::editor::Editor;

mod completion;
pub use completion::*;
mod snippet;
pub use snippet::*;

/// A renderer peer, as the scenarios see it.
///
/// Every method must go through the peer's REAL path. `press_chord` is the
/// one the peer's input loop takes from a normalised chord to a dispatched
/// action; `apply` is its dispatch entry. An implementation that reaches for
/// `Editor::dispatch` to make a scenario pass has stopped testing the peer.
pub trait PeerUnderTest {
    /// A peer over an in-memory document, with nothing loaded from the
    /// developer's machine.
    fn boot(text: &str) -> Self;

    /// One normalised chord through the peer's key path: translate against
    /// the peer's published state, then its dispatch entry.
    fn press_chord(&mut self, chord: KeyChord);

    /// One action through the peer's dispatch entry.
    fn apply(&mut self, action: Action);

    /// What the peer does when it is woken to repaint with no input pending.
    fn idle_wake(&mut self);

    /// Give the active pane this many rows, the way the peer's layout pass
    /// would.
    fn set_viewport_height(&mut self, rows: u32);

    /// The in-process editor.
    fn editor(&self) -> &Editor;

    /// The in-process editor, for seeding state a scenario cannot reach
    /// through keys. Follow with [`Self::republish`].
    fn editor_mut(&mut self) -> &mut Editor;

    /// Publish render state after seeding through [`Self::editor_mut`], so
    /// the peer's next published read sees it.
    fn republish(&mut self) {
        self.editor_mut().publish_render_state();
    }

    /// Crossings of the peer's actor seam so far, on this thread.
    fn actor_crossings(&self) -> u64;
}

fn boot<P: PeerUnderTest>(text: &str) -> P {
    // Before the boot spawns plugin discovery: a developer's installed
    // plugins (auto-pair, an `init.rs`) must not reach a scenario.
    crate::disable_autoload();
    let peer = P::boot(text);
    // Nor may their language servers. The builtin configs name whatever is
    // on `PATH`, so a scenario opening a `.rs` file started rust-analyzer on
    // a machine that has it: an async completion round the scenario did not
    // ask for, and a different code path than on a machine without it. A
    // scenario that wants a server attaches an in-process one.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(peer.editor().lsp.set_configs(Vec::new()))
        .expect("the supervisor takes the empty set");
    peer
}

/// Press a vim-notation key sequence (`"ihello<Esc>"`, `":q<CR>"`).
pub fn press<P: PeerUnderTest>(peer: &mut P, keys: &str) {
    let chords = lattice_protocol::parse_chord_sequence(keys)
        .unwrap_or_else(|e| panic!("key sequence {keys:?} does not parse: {e:?}"));
    for chord in chords {
        peer.press_chord(chord);
    }
}

fn text<P: PeerUnderTest>(peer: &P) -> String {
    peer.editor().document.snapshot().buffer.as_string()
}

fn temp_path(tag: &str) -> std::path::PathBuf {
    // Thread id as well as pid: both peers' copies of a scenario can run in
    // one process, on different test threads.
    std::env::temp_dir().join(format!(
        "lattice-peer-conformance-{tag}-{}-{:?}.txt",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// Keys reach the editor: an edit, an operator, an undo.
///
/// The floor under everything else. Also the partial-chord lifecycle — `dd`
/// and `gg` are two keys each, and a peer whose housekeeping between
/// keystrokes clears the pending chord breaks both.
pub fn keys_edit_the_buffer<P: PeerUnderTest>() {
    let mut peer = boot::<P>("one\ntwo\nthree\n");

    press(&mut peer, "ihello <Esc>");
    assert_eq!(text(&peer), "hello one\ntwo\nthree\n", "insert");

    press(&mut peer, "jdd");
    assert_eq!(text(&peer), "hello one\nthree\n", "a two-key operator");

    press(&mut peer, "u");
    assert_eq!(text(&peer), "hello one\ntwo\nthree\n", "undo");

    press(&mut peer, "Ggg");
    assert_eq!(peer.editor().cursor.line, 0, "a two-key motion");
}

/// A typed character is exactly one crossing of the actor seam.
///
/// `Editor::dispatch_fused` runs the dispatch and its whole tail in one
/// crossing. A second crossing here is a tail step that a peer added beside
/// the fused call instead of inside it; each is a blocking mailbox
/// round-trip (input-latency.md § I.7).
pub fn a_typed_character_is_one_actor_crossing<P: PeerUnderTest>() {
    let mut peer = boot::<P>("one\ntwo\n");
    press(&mut peer, "i");
    // Warm-up: the first key in a mode may pay one-time setup.
    press(&mut peer, "a");

    let before = peer.actor_crossings();
    press(&mut peer, "b");
    let crossings = peer.actor_crossings() - before;

    assert_eq!(text(&peer), "abone\ntwo\n", "the insert ran");
    assert_eq!(crossings, 1, "a typed character is one actor crossing");
}

/// A motion and an operator are one crossing each, like a typed character.
///
/// Their effects — a `CursorMove`, an `Edits`, a `Yank` — are finished by
/// the host, so nothing stands between the dispatch and its tail
/// (`effect_completeness`). Before GD.0b the fuse was declined for any
/// non-empty effect list, and these were several round-trips each in both
/// peers. Held-key `j` is the keystroke whose latency is felt most.
pub fn a_motion_and_an_operator_are_one_actor_crossing_each<P: PeerUnderTest>() {
    let mut peer = boot::<P>("one\ntwo\nthree\nfour\n");
    // Warm-up, and leaves the cursor on line 1.
    press(&mut peer, "j");

    let before = peer.actor_crossings();
    press(&mut peer, "j");
    let crossings = peer.actor_crossings() - before;
    assert_eq!(peer.editor().cursor.line, 2, "the motion ran");
    assert_eq!(crossings, 1, "a motion is one actor crossing");

    let before = peer.actor_crossings();
    press(&mut peer, "x");
    let crossings = peer.actor_crossings() - before;
    assert_eq!(text(&peer), "one\ntwo\nhree\nfour\n", "the operator ran");
    assert_eq!(crossings, 1, "a one-key operator is one actor crossing");
}

/// The cursor is kept in view by the dispatch tail.
///
/// No peer may rely on its paint path to do this: `ensure_cursor_visible` is
/// a tail step, and after the key that moves the cursor the published scroll
/// already shows it.
pub fn the_cursor_stays_in_view<P: PeerUnderTest>() {
    let lines: String = (0..200).map(|n| format!("line {n}\n")).collect();
    let mut peer = boot::<P>(&lines);
    peer.set_viewport_height(10);
    let in_view = |peer: &P| {
        let e = peer.editor();
        (e.scroll..e.scroll + e.viewport_height).contains(&e.cursor.line)
    };

    press(&mut peer, "G");
    assert!(peer.editor().cursor.line >= 199, "`G` went to the end");
    assert!(in_view(&peer), "the view followed the cursor to the end");

    press(&mut peer, "gg");
    assert_eq!(peer.editor().scroll, 0, "and back to the top");

    press(&mut peer, "50j");
    assert!(in_view(&peer), "and after a counted motion past the window");
}

/// The completion popup's keymap layer is active exactly while it is open.
///
/// `completion-popup-mode` carries the popup's keys. Only
/// `Editor::sync_keymap_overlays` — a tail step — activates it, so a peer
/// that skips the tail opens a popup none of whose keys work. Both
/// directions: a layer left on after the popup closed takes `<C-n>` from
/// everything else.
pub fn the_completion_popups_keymap_layer_follows_the_popup<P: PeerUnderTest>() {
    use lattice_completion::{CompletionTrigger, InsertCompletionState};
    use lattice_protocol::position::Position;

    let mut peer = boot::<P>("x\n");
    let buffer = peer.editor().document_buffer_id;
    let popup_mode = crate::keymap_insert::completion_popup_mode_id();
    assert!(
        !peer.editor().minor_mode_enabled_for(buffer, popup_mode),
        "precondition: no popup, no layer"
    );

    peer.editor_mut().insert_completion = Some(InsertCompletionState::open(
        CompletionTrigger::Manual,
        Position::new(0, 0),
        Position::new(0, 1),
        "x".to_string(),
    ));
    peer.republish();
    peer.apply(Action::None);
    assert!(
        peer.editor().minor_mode_enabled_for(buffer, popup_mode),
        "the tail activates the popup's mode once a popup is open"
    );

    peer.editor_mut().insert_completion = None;
    peer.republish();
    peer.apply(Action::None);
    assert!(
        !peer.editor().minor_mode_enabled_for(buffer, popup_mode),
        "and deactivates it when the popup is gone"
    );
}

/// A follow-up action runs after the effects of the action before it.
///
/// A macro replays as a list of follow-up actions. "Open a file, then type"
/// has to open before it types; a peer that dispatches the follow-ups bare
/// and applies their renderer-coupled effects afterwards types into the
/// buffer the macro was started from.
pub fn a_replayed_macro_types_into_the_buffer_it_opened<P: PeerUnderTest>() {
    let path = temp_path("macro-order");
    std::fs::write(&path, "target\n").expect("temp file writes");

    let mut peer = boot::<P>("origin\n");
    let origin = peer.editor().document_buffer_id;
    press(&mut peer, &format!("qa:e {}<CR>ihi <Esc>q", path.display()));
    let target = peer.editor().document_buffer_id;
    assert_ne!(target, origin, "test premise: `:e` switched buffers");
    assert_eq!(text(&peer), "hi target\n", "test premise: recording typed");

    peer.editor_mut().activate_buffer(origin);
    peer.republish();
    press(&mut peer, "gg0@a");

    let _ = std::fs::remove_file(&path);
    assert_eq!(
        peer.editor().document_buffer_id,
        target,
        "the replay opened the file"
    );
    assert_eq!(
        text(&peer),
        "hi hi target\n",
        "and typed into it, not into the buffer it began in"
    );
}

/// A renderer effect queued with no keystroke in flight is applied on the
/// peer's idle wake.
///
/// The async picker accept and its peers queue what they cannot apply off
/// the renderer. A peer that drains the queue only in a keystroke's tail
/// shows the result when the user next presses something. Tested the way it
/// fails: nothing is dispatched between the queueing and the assertion.
pub fn a_queued_renderer_effect_is_applied_on_an_idle_wake<P: PeerUnderTest>() {
    let path = temp_path("idle-effect");
    std::fs::write(&path, "opened while idle\n").expect("temp file writes");

    let mut peer = boot::<P>("origin\n");
    peer.editor_mut()
        .pending_renderer_effects
        .push(lattice_grammar::Effect::OpenBuffer {
            path: Some(path.clone()),
            force: false,
        });
    peer.republish();

    peer.idle_wake();

    let _ = std::fs::remove_file(&path);
    assert_eq!(
        text(&peer),
        "opened while idle\n",
        "the queued open ran, with no action dispatched"
    );
    assert!(
        peer.editor().pending_renderer_effects.is_empty(),
        "and the queue is drained"
    );
}

/// `:q` on a clean buffer asks the peer to quit.
///
/// The request is published state (`lifecycle.should_quit`); what a peer
/// does with it is its own, but it has to arrive.
pub fn quitting_is_published<P: PeerUnderTest>() {
    let mut peer = boot::<P>("x\n");
    assert!(!peer.editor().should_quit, "precondition");

    press(&mut peer, ":q<CR>");

    assert!(peer.editor().should_quit, "`:q` on a clean buffer quits");
    assert!(
        peer.editor().render_state.load().lifecycle.should_quit,
        "and the request is published for the peer's loop to read"
    );
}

/// A chord whose binding DECLINES falls through to the layer below.
///
/// A mode can bind a key it only sometimes wants: `table-mode` binds
/// `<Tab>` in Insert and declines outside a table. The peer must then
/// re-resolve the chord without that mode's layer
/// ([`DeclinePeel`](crate::decline::DeclinePeel)), which here reaches the
/// builtin insert-tab. A peer without the walk makes every such key dead —
/// the GUI was one for a while, and lost `( [ { " '` and backspace to
/// auto-pair the same way.
///
/// The walk is in each peer's key path and not in the host's dispatch,
/// because only the peer still holds the chord when the decline is known.
pub fn a_declined_chord_falls_through_to_the_layer_below<P: PeerUnderTest>() {
    let mut peer = boot::<P>("x\n");
    let table_mode = lattice_mode::TableMode::mode_id();
    let buffer = peer.editor().document_buffer_id;
    let _ = peer.editor_mut().activate_mode_by_id(buffer, table_mode);
    peer.republish();
    assert!(
        peer.editor()
            .render_state
            .load()
            .translator
            .active_minor_modes
            .contains(&table_mode),
        "precondition: table-mode is an active layer, or nothing declines \
         and the scenario proves nothing"
    );

    press(&mut peer, "i<Tab>");

    assert!(
        text(&peer).contains('\t'),
        "`<Tab>` outside a table falls through to insert-tab; the buffer is {:?}",
        text(&peer)
    );
}

/// Instantiate every conformance scenario as a `#[test]` for one peer.
///
/// Call it inside a `#[cfg(test)]` module of the peer crate. The scenario
/// list is here, not at the call site, so a new scenario reaches every peer.
#[macro_export]
macro_rules! peer_conformance_tests {
    ($peer:ty) => {
        $crate::peer_conformance_tests!(@each $peer;
            keys_edit_the_buffer,
            a_typed_character_is_one_actor_crossing,
            a_motion_and_an_operator_are_one_actor_crossing_each,
            the_cursor_stays_in_view,
            the_completion_popups_keymap_layer_follows_the_popup,
            a_replayed_macro_types_into_the_buffer_it_opened,
            a_queued_renderer_effect_is_applied_on_an_idle_wake,
            quitting_is_published,
            a_declined_chord_falls_through_to_the_layer_below,
            ctrl_space_completes_from_buffer_words,
            ctrl_space_completes_from_tree_sitter_symbols,
            ctrl_space_completes_paths_inside_a_string,
            ctrl_space_completes_from_snippets,
            ctrl_space_completes_from_a_background_source,
            ctrl_space_completes_from_a_language_server,
            a_slow_source_lands_after_an_empty_one_answered,
            typing_while_a_source_answers_keeps_its_rows,
            an_empty_round_says_no_completions,
            a_source_filter_survives_a_late_answer,
            every_source_keeps_its_priority,
            a_background_sources_rows_carry_its_own_id,
            a_source_registered_after_the_buffer_opened_completes_there,
            a_text_file_completes_from_its_plain_text_sources,
            typing_does_not_open_completion_by_default,
            auto_trigger_opens_once_the_word_is_long_enough,
            a_trigger_character_opens_the_sources_that_declared_it,
            a_language_can_turn_auto_trigger_on_alone,
            describe_buffer_lists_the_completion_sources,
            accepting_a_snippet_expands_it_and_selects_the_first_placeholder,
            ctrl_x_ctrl_s_expands_the_word_before_the_cursor,
            typing_over_a_placeholder_replaces_its_default,
            tab_and_shift_tab_walk_the_placeholders,
            esc_leaves_the_snippet,
            typing_in_a_placeholder_then_tab_lands_on_the_next,
            a_newline_in_a_placeholder_moves_the_later_ones_down,
            an_edit_above_the_snippet_moves_its_placeholders,
            overtyping_a_placeholder_drops_the_ones_nested_in_it,
        );
    };
    (@each $peer:ty; $($scenario:ident),+ $(,)?) => {
        $(
            #[test]
            fn $scenario() {
                $crate::peer_conformance::$scenario::<$peer>();
            }
        )+
    };
}
