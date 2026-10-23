//! Snippets, through each peer's real key path: expansion from the popup and
//! from `<C-x><C-s>`, overtyping a placeholder, walking the stops, leaving.
//!
//! The host's snippet tests call `expand_snippet_from_range` and
//! `do_completion_accept` directly, so none of them pressed the keys a user
//! presses. These do, in both renderers (snippet-integrity SI.1).

use std::sync::Arc;

use lattice_grammar::{ModalState, VisualKind};
use lattice_protocol::position::Position;

use super::{PeerUnderTest, boot, press, text};

/// The snippet every scenario expands: two multi-character defaults and a
/// final stop. Rendered, it reads `for idx in iter {  }` — `idx` at bytes
/// 4..7, `iter` at 11..15, `$0` at 18.
const FOR_BODY: &str = "for ${1:idx} in ${2:iter} { $0 }";
const FOR_PREFIX: &str = "zzfor";

/// Register [`FOR_BODY`] under [`FOR_PREFIX`] in the any-language bucket, so
/// a pathless buffer reaches it.
fn install_for_snippet<P: PeerUnderTest>(peer: &mut P) {
    let editor = peer.editor_mut();
    let mut next = (**editor.snippet_registry.load()).clone();
    next.insert(
        "*",
        lattice_snippet::Snippet {
            name: "conformance-for".into(),
            prefixes: vec![FOR_PREFIX.into()],
            body: lattice_snippet::parse::parse(FOR_BODY).expect("snippet parses"),
            description: None,
            scope: String::new(),
        },
    );
    editor.snippet_registry.store(Arc::new(next));
    peer.republish();
}

fn session_active<P: PeerUnderTest>(peer: &P) -> bool {
    let editor = peer.editor();
    editor.snippet_session.is_active(editor.document_buffer_id)
}

/// The primary selection's `(anchor, head)`.
fn selection<P: PeerUnderTest>(peer: &P) -> (Position, Position) {
    let sel = *peer.editor().document.selections().primary();
    (sel.anchor, sel.head)
}

/// Assert the peer is selecting a placeholder default: charwise Select, the
/// anchor on its first byte and the head on its last.
fn assert_selects<P: PeerUnderTest>(peer: &P, what: &str, start: u32, end: u32) {
    assert!(
        matches!(
            peer.editor().modal,
            ModalState::Select(VisualKind::Charwise)
        ),
        "{what}: a default is selected in Select, the mode is {:?}",
        peer.editor().modal
    );
    assert_eq!(
        selection(peer),
        (Position::new(0, start), Position::new(0, end - 1)),
        "{what}: the selection covers the default; the buffer is {:?}",
        text(peer)
    );
}

/// Accepting a snippet row from the popup expands it and selects the first
/// placeholder's default.
pub fn accepting_a_snippet_expands_it_and_selects_the_first_placeholder<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);

    press(&mut peer, "izzf<C-Space><C-y>");

    assert_eq!(text(&peer), "for idx in iter {  }\n");
    assert!(session_active(&peer), "the expansion starts a session");
    assert_selects(&peer, "after accept", 4, 7);
}

/// `<C-x><C-s>` expands the snippet whose prefix is the word before the
/// cursor, without the popup.
pub fn ctrl_x_ctrl_s_expands_the_word_before_the_cursor<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);

    press(&mut peer, "izzfor<C-x><C-s>");

    assert_eq!(text(&peer), "for idx in iter {  }\n");
    assert!(
        peer.editor().insert_completion.is_none(),
        "no popup is opened"
    );
    assert!(session_active(&peer), "the expansion starts a session");
    assert_selects(&peer, "after <C-x><C-s>", 4, 7);
}

/// Typing over a selected default replaces all of it.
pub fn typing_over_a_placeholder_replaces_its_default<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);

    press(&mut peer, "izzfor<C-x><C-s>n");

    assert_eq!(text(&peer), "for n in iter {  }\n");
    assert!(
        matches!(peer.editor().modal, ModalState::Insert),
        "overtyping drops to Insert, the mode is {:?}",
        peer.editor().modal
    );
}

/// `<Tab>` moves to the next placeholder and `<S-Tab>` back, each selecting
/// that placeholder's default.
pub fn tab_and_shift_tab_walk_the_placeholders<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s>");

    press(&mut peer, "<Tab>");
    assert_selects(&peer, "<Tab> to $2", 11, 15);

    press(&mut peer, "<S-Tab>");
    assert_selects(&peer, "<S-Tab> back to $1", 4, 7);
    assert_eq!(
        text(&peer),
        "for idx in iter {  }\n",
        "walking edits nothing"
    );
}

/// `<Esc>` ends the session and, by vim convention, leaves for Normal.
pub fn esc_leaves_the_snippet<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s>");
    assert!(session_active(&peer), "precondition");

    press(&mut peer, "<Esc>");

    assert!(!session_active(&peer), "`<Esc>` ends the session");
    assert!(
        matches!(peer.editor().modal, ModalState::Normal),
        "and leaves for Normal, the mode is {:?}",
        peer.editor().modal
    );
    assert_eq!(text(&peer), "for idx in iter {  }\n", "the text stays");
}
