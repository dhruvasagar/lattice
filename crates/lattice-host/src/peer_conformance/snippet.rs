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
    install_snippet(peer, "conformance-for", FOR_PREFIX, FOR_BODY);
}

/// Register `body` under `prefix` in the any-language bucket.
fn install_snippet<P: PeerUnderTest>(peer: &mut P, name: &str, prefix: &str, body: &str) {
    let editor = peer.editor_mut();
    let mut next = (**editor.snippet_registry.load()).clone();
    next.insert(
        "*",
        lattice_snippet::Snippet {
            name: name.into(),
            prefixes: vec![prefix.into()],
            body: lattice_snippet::parse::parse(body).expect("snippet parses"),
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

/// Assert the peer is selecting a placeholder default on line 0: charwise
/// Select, the anchor on its first byte and the head on its last.
fn assert_selects<P: PeerUnderTest>(peer: &P, what: &str, start: u32, end: u32) {
    assert_selects_on(peer, what, 0, start, end);
}

/// [`assert_selects`] on `line`.
fn assert_selects_on<P: PeerUnderTest>(peer: &P, what: &str, line: u32, start: u32, end: u32) {
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
        (Position::new(line, start), Position::new(line, end - 1)),
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

// ---- SI.2: placeholders follow edits ----

/// Typing in the first placeholder, then `<Tab>`, selects the second where
/// it now is. Before SI.2 the session held the offsets it was expanded at
/// and nothing moved them: `<Tab>` here selected `" in "`-shifted bytes.
pub fn typing_in_a_placeholder_then_tab_lands_on_the_next<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);

    press(&mut peer, "izzfor<C-x><C-s>name<Tab>");

    assert_eq!(text(&peer), "for name in iter {  }\n");
    assert_selects(&peer, "`<Tab>` after typing `name`", 12, 16);

    press(&mut peer, "<S-Tab>");
    assert_selects(&peer, "`<S-Tab>` back onto what was typed", 4, 8);
}

/// A newline typed inside the first placeholder moves the second to the
/// next line.
pub fn a_newline_in_a_placeholder_moves_the_later_ones_down<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);

    // `ab<CR>cd` for `idx` changes the byte count (+2), so stale offsets
    // could not land on `iter` by accident.
    press(&mut peer, "izzfor<C-x><C-s>ab<CR>cd<Tab>");

    let text = text(&peer);
    let second = text.lines().nth(1).expect("a second line");
    let iter = second.find("iter").expect("`iter` is on the second line") as u32;
    assert_selects_on(&peer, "`<Tab>` onto the moved `$2`", 1, iter, iter + 4);
}

/// An edit made to the document outside the snippet — a language server's
/// `applyEdit`, a formatter, a plugin, none of which go through the cursor —
/// moves the placeholders with it.
pub fn an_edit_above_the_snippet_moves_its_placeholders<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s>");

    peer.editor_mut()
        .apply_edit_blocking(lattice_protocol::edit::Edit::insert(
            Position::new(0, 0),
            "use x;\n",
        ))
        .expect("the edit applies");
    peer.republish();
    press(&mut peer, "<Tab>");

    assert_eq!(text(&peer), "use x;\nfor idx in iter {  }\n");
    assert_selects_on(&peer, "`<Tab>` after a line was added above", 1, 11, 15);
}

/// Overtyping a placeholder removes the placeholders nested in its default:
/// `<Tab>` goes past them to the next one that still exists.
pub fn overtyping_a_placeholder_drops_the_ones_nested_in_it<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    // `fn name(arg) -> T`: `$2` sits inside `$1`'s default.
    install_snippet(
        &mut peer,
        "conformance-nested",
        "zzfn",
        "fn ${1:name${2:_x}}(${3:arg})",
    );
    press(&mut peer, "izzfn<C-x><C-s>");
    assert_selects(&peer, "precondition: `$1` selected", 3, 9);

    press(&mut peer, "go<Tab>");

    assert_eq!(text(&peer), "fn go(arg)\n");
    assert_selects(&peer, "`<Tab>` skips the removed `$2` to `$3`", 6, 9);
}

// ---- SI.3: navigation ----

/// `<Tab>` from a selected default onto the empty final stop leaves Select,
/// so the next key types at the stop instead of overtyping everything from
/// the previous placeholder to it — and the snippet is over.
pub fn tab_onto_the_final_stop_types_there_and_ends_the_snippet<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);

    press(&mut peer, "izzfor<C-x><C-s><Tab><Tab>x");

    assert_eq!(text(&peer), "for idx in iter { x }\n");
    assert!(!session_active(&peer), "reaching `$0` ends the snippet");
    assert!(
        matches!(peer.editor().modal, ModalState::Insert),
        "typing at `$0` is Insert, the mode is {:?}",
        peer.editor().modal
    );
}

/// Once the snippet is over, `<Tab>` is a plain `<Tab>` again: it is not
/// swallowed by a session with nowhere left to go.
pub fn the_tab_after_the_final_stop_is_a_plain_tab<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s><Tab><Tab>");
    assert_eq!(text(&peer), "for idx in iter {  }\n", "precondition");

    press(&mut peer, "<Tab>");

    let text = text(&peer);
    assert!(
        text.starts_with("for idx in iter { ") && text.ends_with(" }\n") && text.len() > 21,
        "`<Tab>` inserted indentation at `$0`: {text:?}"
    );
}

/// `<S-Tab>` back onto a one-character default selects just that character:
/// the selection does not stretch back from where the user was.
pub fn shift_tab_onto_a_one_character_default_selects_only_it<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-ij", "zzij", "${1:i} + ${2:j}");
    press(&mut peer, "izzij<C-x><C-s><Tab>");
    assert_selects(&peer, "`<Tab>` onto `j`", 4, 5);

    press(&mut peer, "<S-Tab>");
    assert_selects(&peer, "`<S-Tab>` onto `i`", 0, 1);

    press(&mut peer, "x");
    assert_eq!(text(&peer), "x + j\n", "only `i` is overtyped");
}

/// A default ending in a multi-byte character is selected — and overtyped —
/// whole.
pub fn a_multibyte_default_is_overtyped_whole<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-cafe", "zzcafe", "${1:café} $2");

    press(&mut peer, "izzcafe<C-x><C-s>x");

    assert_eq!(text(&peer), "x \n");
}

/// A snippet without `$0` exits at its end: the last `<Tab>` puts the
/// cursor after the snippet, not inside its last placeholder.
pub fn without_a_final_stop_the_last_tab_goes_to_the_end<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-ab", "zzab", "${1:a} ${2:b}");

    press(&mut peer, "izzab<C-x><C-s><Tab><Tab>!");

    assert_eq!(text(&peer), "a b!\n");
    assert!(!session_active(&peer), "the snippet is over");
}

/// The same geometry outside snippets: Visual's span ends after the whole
/// character under the head, so `vd` on a multi-byte character deletes it.
/// Visual and Select share the extent (select-mode.md §2), which is how the
/// snippet case above found this.
pub fn visual_delete_takes_a_whole_multibyte_character<P: PeerUnderTest>() {
    let mut peer = boot::<P>("é!\n");

    press(&mut peer, "vd");

    assert_eq!(text(&peer), "!\n");
}

// ---- SI.4: lifecycle ----

/// How many ranges the active document tracks — a snippet's extent and one
/// per placeholder occurrence while a session is live, none after.
fn tracked<P: PeerUnderTest>(peer: &P) -> usize {
    peer.editor().document.snapshot().tracked_ranges.len()
}

/// Wait for the document to have released every tracked range. A session
/// ends synchronously, but the untrack it sends the document is
/// fire-and-forget — nothing waits on cleanup — so the snapshot shows it
/// once the actor has run the message, which under a parallel test run is
/// not immediately.
fn releases_its_ranges<P: PeerUnderTest>(peer: &P) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while tracked(peer) != 0 {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
}

/// Leaving Insert by any route ends the snippet — not only the `<Esc>` the
/// mode binds — and stops tracking its placeholders.
pub fn leaving_insert_ends_the_snippet<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s>n");
    assert!(session_active(&peer), "precondition");

    peer.apply(crate::action::Action::EnterMode(ModalState::Normal));

    assert!(!session_active(&peer), "Normal mode ends the snippet");
    assert!(
        releases_its_ranges(&peer),
        "its ranges are released; {} still tracked",
        tracked(&peer)
    );
}

/// Moving the cursor out of the snippet ends it: a later `<Tab>` is a plain
/// `<Tab>`, not a jump back into text the user has left.
pub fn moving_out_of_the_snippet_ends_it<P: PeerUnderTest>() {
    let mut peer = boot::<P>("x \n");
    install_for_snippet(&mut peer);
    press(&mut peer, "Azzfor<C-x><C-s>n");
    assert!(session_active(&peer), "precondition");

    // To the start of the line, before the snippet. (`<Down>` would be
    // the obvious key, but Insert does not bind it yet.)
    press(&mut peer, "<Home>");

    assert!(
        !session_active(&peer),
        "the cursor left the snippet: cursor {:?}, mode {:?}, text {:?}",
        peer.editor().cursor,
        peer.editor().modal,
        text(&peer)
    );
    assert!(releases_its_ranges(&peer), "its ranges are released");
}

/// Moving inside the snippet keeps it.
pub fn moving_inside_the_snippet_keeps_it<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s>name<Left><Left>");
    assert!(session_active(&peer), "still inside `$1`");

    press(&mut peer, "<Tab>");
    assert_selects(&peer, "`<Tab>` still reaches `$2`", 12, 16);
}

/// Expanding a snippet inside a live one replaces it, and the outer
/// snippet's ranges are released rather than tracked forever.
pub fn expanding_inside_a_snippet_replaces_it<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s>");

    press(&mut peer, "zzfor<C-x><C-s>");

    assert_eq!(text(&peer), "for for idx in iter {  } in iter {  }\n");
    assert_selects(&peer, "the inner snippet's `$1`", 8, 11);
    assert_eq!(
        tracked(&peer),
        4,
        "only the inner snippet's extent, `$1`, `$2` and `$0` are tracked"
    );
}
