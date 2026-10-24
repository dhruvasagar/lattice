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
    install_snippet_in(peer, "*", name, prefix, body);
}

/// Register `body` under `prefix` in `language`'s pack.
fn install_snippet_in<P: PeerUnderTest>(
    peer: &mut P,
    language: &str,
    name: &str,
    prefix: &str,
    body: &str,
) {
    let editor = peer.editor_mut();
    let mut next = (**editor.snippet_registry.load()).clone();
    next.insert(
        language,
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

// ---- SI.5: identity and language ----

/// Open a fresh, empty file named `name` in a scratch directory.
fn edit_new_file<P: PeerUnderTest>(peer: &mut P, name: &str) {
    let dir = std::env::temp_dir().join(format!(
        "lattice-peer-conformance-snippet-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(name);
    std::fs::write(&path, "\n").expect("write");
    press(peer, &format!(":e {}<CR>", path.display()));
    assert_eq!(
        peer.editor().document.snapshot().path(),
        Some(path.as_path()),
        "precondition: {name} is open"
    );
}

/// Accepting a snippet from the popup expands the buffer's language's
/// snippet of that name, not another language's. Before SI.5 the row
/// carried the name alone and the body came from whichever pack loaded
/// last.
pub fn the_buffers_language_owns_a_shared_snippet_name<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet_in(&mut peer, "rust", "zzif", "zzif", "if ${1:cond} {}");
    install_snippet_in(&mut peer, "python", "zzif", "zzif", "if ${1:cond}:");
    edit_new_file(&mut peer, "main.rs");

    press(&mut peer, "izzi<C-Space><C-y>");

    assert_eq!(
        text(&peer),
        "if cond {}\n",
        "Rust's `zzif`, in a `.rs` file"
    );
}

/// A snippet loaded later on a prefix — the user's pack, after the builtins
/// — is the one `<C-x><C-s>` expands.
pub fn a_later_snippet_on_a_prefix_is_the_one_expanded<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "builtin-ov", "zzov", "builtin");
    install_snippet(&mut peer, "user-ov", "zzov", "mine");

    press(&mut peer, "izzov<C-x><C-s>");

    assert_eq!(text(&peer), "mine\n");
}

/// A Ruby file reaches the `ruby` pack — the language id is `ruby`, not the
/// extension `rb`, wherever snippets are looked up.
pub fn a_ruby_file_expands_ruby_snippets<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet_in(&mut peer, "ruby", "zzdef", "zzdef", "def ${1:name}\nend");
    edit_new_file(&mut peer, "app.rb");

    press(&mut peer, "izzdef<C-x><C-s>");

    assert_eq!(text(&peer), "def name\nend\n");
}

// ---- SI.6: indentation ----

/// A multi-line snippet expanded on an indented line keeps that indentation
/// on every line, and its `\t` levels follow `expandtab` / `shiftwidth`
/// (here the defaults: four spaces).
pub fn a_multi_line_snippet_follows_the_lines_indentation<P: PeerUnderTest>() {
    let mut peer = boot::<P>("fn main() {\n    \n}\n");
    install_snippet(
        &mut peer,
        "conformance-if",
        "zzif",
        "if ${1:cond} {\n\t$0\n}",
    );

    press(&mut peer, "jAzzif<C-x><C-s>");

    assert_eq!(
        text(&peer),
        "fn main() {\n    if cond {\n        \n    }\n}\n"
    );
    assert_selects_on(&peer, "`$1` on the first line", 1, 7, 11);

    press(&mut peer, "<Tab>x");
    assert_eq!(
        text(&peer),
        "fn main() {\n    if cond {\n        x\n    }\n}\n",
        "`$0` sits after the body's indentation"
    );
}

/// With `noexpandtab`, the body's levels stay tabs and the line's own tab
/// indentation carries onto each new line.
pub fn without_expandtab_a_snippet_indents_with_tabs<P: PeerUnderTest>() {
    let mut peer = boot::<P>("{\n\t\n}\n");
    install_snippet(
        &mut peer,
        "conformance-if",
        "zzif",
        "if ${1:cond} {\n\t$0\n}",
    );
    press(&mut peer, ":set noexpandtab<CR>");

    press(&mut peer, "jAzzif<C-x><C-s>");

    assert_eq!(text(&peer), "{\n\tif cond {\n\t\t\n\t}\n}\n");
}

// ---- SI.7: mirrors ----

/// Every copy of a placeholder shows its default, and the first is selected.
pub fn mirrors_show_the_placeholder_text<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-let", "zzlet", "let ${1:x} = $1;");

    press(&mut peer, "izzlet<C-x><C-s>");

    assert_eq!(text(&peer), "let x = x;\n");
    assert_selects(&peer, "the first copy", 4, 5);
}

/// Typing in a placeholder updates its copies as you type, not on `<Tab>`.
pub fn typing_in_a_placeholder_updates_its_mirrors<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-let", "zzlet", "let ${1:x} = $1;");

    press(&mut peer, "izzlet<C-x><C-s>v");
    assert_eq!(text(&peer), "let v = v;\n", "the overtype is copied");

    press(&mut peer, "al");
    assert_eq!(text(&peer), "let val = val;\n", "and every key after it");
}

/// Typing in a later copy grows the earlier one too, and the cursor stays
/// where the user is typing rather than where the text used to be.
pub fn typing_in_a_later_copy_keeps_the_cursor_in_place<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-eq", "zzeq", "${1:x} = $1");
    press(&mut peer, "izzeq<C-x><C-s>a");
    assert_eq!(text(&peer), "a = a\n", "precondition");

    press(&mut peer, "<End>yz");

    assert_eq!(text(&peer), "ayz = ayz\n");
}

// ---- SI.9: a language server's snippets ----

/// A `.rs` file with an in-process language server answering `items`.
/// Returns the peer, the server's runtime (keep it alive) and its log.
fn with_server<P: PeerUnderTest>(
    tag: &str,
    items: Vec<serde_json::Value>,
    extra_capabilities: serde_json::Value,
) -> (P, tokio::runtime::Runtime, super::completion::MockLspLog) {
    let dir = super::completion::temp_dir(tag);
    let path = dir.join("main.rs");
    std::fs::write(&path, "fn main() {}\n").expect("write");
    let mut peer = boot::<P>("\n");
    super::completion::open(&mut peer, &path);
    let (rt, log) = super::completion::attach_mock_lsp_with(
        &mut peer,
        &path,
        items,
        std::time::Duration::ZERO,
        extra_capabilities,
    );
    (peer, rt, log)
}

/// Open the popup on `keys` and accept `label` once the server's row is in.
fn complete_from_server<P: PeerUnderTest>(peer: &mut P, keys: &str, label: &str) {
    press(peer, keys);
    let lsp = lattice_completion::LSP_COMPLETION_SOURCE_ID;
    assert!(
        super::completion::settle(peer, |p| super::completion::shows(p, label, lsp)),
        "the server's `{label}` reached the popup"
    );
    press(peer, "<C-y>");
}

/// The client tells servers it takes snippets. Without `snippetSupport`
/// servers send plain text only, which is why no language server's
/// snippet ever reached the snippet engine. It does not offer to resolve
/// `additionalTextEdits` lazily: accept does not wait for a resolve, so a
/// server that deferred them would lose its auto-imports.
pub fn the_client_tells_servers_it_takes_snippets<P: PeerUnderTest>() {
    let (_peer, _rt, log) = with_server::<P>("lsp-caps", Vec::new(), serde_json::json!({}));
    let log = log.lock().expect("log");
    let (_, params) = log
        .iter()
        .find(|(method, _)| method == "initialize")
        .expect("the client initialized");
    let item = &params["capabilities"]["textDocument"]["completion"]["completionItem"];
    assert_eq!(item["snippetSupport"], serde_json::json!(true), "{item}");
    let resolvable = item["resolveSupport"]["properties"].to_string();
    assert!(!resolvable.contains("additionalTextEdits"), "{resolvable}");
}

/// A server's snippet expands through the snippet engine: its placeholder
/// is selected for overtyping, and the session is live.
pub fn a_server_snippet_expands_with_its_placeholders<P: PeerUnderTest>() {
    let item = serde_json::json!({
        "label": "push",
        "insertTextFormat": 2,
        "insertText": "push(${1:value})$0"
    });
    let (mut peer, _rt, _log) = with_server::<P>("lsp-snippet", vec![item], serde_json::json!({}));

    complete_from_server(&mut peer, "Gopu<C-Space>", "push");

    assert_eq!(text(&peer), "fn main() {}\npush(value)\n");
    assert_selects_on(&peer, "the server's placeholder", 1, 5, 10);
    assert!(session_active(&peer));
}

/// A server snippet replaces the range its `textEdit` names, not only the
/// word the popup was opened on.
pub fn a_server_snippet_replaces_its_own_range<P: PeerUnderTest>() {
    let item = serde_json::json!({
        "label": "push",
        "insertTextFormat": 2,
        "textEdit": {
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 7 } },
            "newText": "self.push(${1:v})"
        }
    });
    let (mut peer, _rt, _log) = with_server::<P>("lsp-range", vec![item], serde_json::json!({}));

    complete_from_server(&mut peer, "Goself.pu<C-Space>", "push");

    assert_eq!(text(&peer), "fn main() {}\nself.push(v)\n");
}

/// An import the server adds above (`additionalTextEdits`) moves the
/// snippet down a line; its placeholders move with it.
pub fn an_import_above_keeps_the_snippets_placeholders<P: PeerUnderTest>() {
    let item = serde_json::json!({
        "label": "wr",
        "insertTextFormat": 2,
        "insertText": "write(${1:f})",
        "additionalTextEdits": [{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
            "newText": "use std::fmt;\n"
        }]
    });
    let (mut peer, _rt, _log) = with_server::<P>("lsp-import", vec![item], serde_json::json!({}));

    complete_from_server(&mut peer, "Gowr<C-Space>", "wr");

    assert_eq!(text(&peer), "use std::fmt;\nfn main() {}\nwrite(f)\n");
    assert_selects_on(&peer, "`f`, a line further down", 2, 6, 7);
}

/// An accepted server snippet's `command` runs, as a plain item's does.
pub fn a_server_snippets_command_runs<P: PeerUnderTest>() {
    let item = serde_json::json!({
        "label": "cmd",
        "insertTextFormat": 2,
        "insertText": "cmd(${1:x})",
        "command": { "title": "after", "command": "conformance.after" }
    });
    let (mut peer, _rt, log) = with_server::<P>(
        "lsp-command",
        vec![item],
        serde_json::json!({ "executeCommandProvider": { "commands": ["conformance.after"] } }),
    );

    complete_from_server(&mut peer, "Gocm<C-Space>", "cmd");

    assert!(
        super::completion::settle(&mut peer, |_| {
            log.lock()
                .expect("log")
                .iter()
                .any(|(method, _)| method == "workspace/executeCommand")
        }),
        "the server was asked to run the item's command"
    );
}

/// `insertTextMode: asIs` keeps the server's whitespace: no re-indent.
pub fn an_as_is_server_snippet_keeps_its_whitespace<P: PeerUnderTest>() {
    let item = serde_json::json!({
        "label": "blk",
        "insertTextFormat": 2,
        "insertTextMode": 1,
        "insertText": "blk {\n\t$0\n}"
    });
    let (mut peer, _rt, _log) = with_server::<P>("lsp-as-is", vec![item], serde_json::json!({}));

    complete_from_server(&mut peer, "Go    bl<C-Space>", "blk");

    assert_eq!(text(&peer), "fn main() {}\n    blk {\n\t\n}\n");
}

// ---- SI.8: transforms, unknown variables, partial packs ----

/// Leaving a placeholder rewrites its transformed copies from what was typed
/// (VS Code's rule: on leaving, not on every key).
pub fn leaving_a_placeholder_rewrites_its_transformed_copy<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(
        &mut peer,
        "conformance-transform",
        "zztr",
        "${1:name} ${1/(.*)/${1:/upcase}/}$0",
    );
    press(&mut peer, "izztr<C-x><C-s>");
    assert_eq!(text(&peer), "name NAME\n", "the transform of the default");

    press(&mut peer, "foo<Tab>");

    assert_eq!(text(&peer), "foo FOO\n");
}

/// An unknown variable is a placeholder holding its name, selected for
/// overtyping — a typo shows instead of vanishing.
pub fn an_unknown_variable_is_a_placeholder<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-var", "zzvar", "hi $NOPE!");

    press(&mut peer, "izzvar<C-x><C-s>");

    assert_eq!(text(&peer), "hi NOPE!\n");
    assert_selects(&peer, "the variable's name", 3, 7);
}

/// A pack with one malformed entry still loads the rest, and the reload
/// says what it skipped. One bad entry used to drop the whole file.
pub fn a_bad_entry_skips_itself_not_its_pack<P: PeerUnderTest>() {
    let dir = super::completion::temp_dir("snippet-pack");
    std::fs::write(
        dir.join("_global.json"),
        r#"{
            "broken": { "prefix": "zzbroken" },
            "fine": { "prefix": "zzfine", "body": "fine $1" }
        }"#,
    )
    .expect("write the pack");
    let mut peer = boot::<P>("\n");
    peer.editor_mut().snippet_dirs = vec![dir];

    press(&mut peer, ":reload-snippets<CR>");

    let message = peer
        .editor()
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        message.contains("broken"),
        "the reload names the skip: {message:?}"
    );
    press(&mut peer, "izzfine<C-x><C-s>");
    assert_eq!(text(&peer), "fine \n", "the good entry loaded");
}

// ---- SI.10: variables ----

/// The language's comment leader and the file's names reach a snippet.
pub fn file_and_comment_variables_expand<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(
        &mut peer,
        "conformance-vars",
        "zzhdr",
        "$LINE_COMMENT $TM_FILENAME_BASE in $RELATIVE_FILEPATH",
    );
    edit_new_file(&mut peer, "main.rs");

    press(&mut peer, "izzhdr<C-x><C-s>");

    let text = text(&peer);
    assert!(
        text.starts_with("// main in ") && text.trim_end().ends_with("main.rs"),
        "comment leader, base name and relative path: {text:?}"
    );
}

/// `$CLIPBOARD` is what the system clipboard holds — text copied in another
/// program — not only the editor's own `+` register.
pub fn the_clipboard_variable_reads_the_clipboard<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(&mut peer, "conformance-clip", "zzclip", "[$CLIPBOARD]");
    peer.editor()
        .services
        .get::<lattice_core::ClipboardHandle>()
        .expect("boot registers a clipboard")
        .write("copied elsewhere".into());

    press(&mut peer, "izzclip<C-x><C-s>");

    assert_eq!(text(&peer), "[copied elsewhere]\n");
}

// ---- SI.11: where packs load from ----

/// `:set snippet.dirs=…` adds a directory of packs, and takes effect at once.
pub fn the_snippet_dirs_option_adds_a_directory<P: PeerUnderTest>() {
    let dir = super::completion::temp_dir("snippet-dirs");
    std::fs::write(
        dir.join("_global.json"),
        r#"{ "extra": { "prefix": "zzextra", "body": "from snippet.dirs" } }"#,
    )
    .expect("write the pack");
    let mut peer = boot::<P>("\n");
    peer.editor_mut().snippet_dirs = Vec::new();

    press(
        &mut peer,
        &format!(":set snippet.dirs={}<CR>", dir.display()),
    );
    press(&mut peer, "izzextra<C-x><C-s>");

    assert_eq!(text(&peer), "from snippet.dirs\n");
}

/// A project's `.lattice/snippets` loads, and wins over the user's own on a
/// shared prefix.
pub fn a_projects_snippets_override_the_users<P: PeerUnderTest>() {
    let user = super::completion::temp_dir("snippet-user");
    std::fs::write(
        user.join("_global.json"),
        r#"{ "mine": { "prefix": "zzhdr", "body": "user header" } }"#,
    )
    .expect("write the user pack");
    let project = super::completion::temp_dir("snippet-project");
    std::fs::create_dir_all(project.join(".git")).expect("a project root");
    std::fs::create_dir_all(project.join(".lattice/snippets")).expect("the project's packs");
    std::fs::write(
        project.join(".lattice/snippets/_global.json"),
        r#"{ "ours": { "prefix": "zzhdr", "body": "project header" } }"#,
    )
    .expect("write the project pack");
    let file = project.join("main.rs");
    std::fs::write(&file, "\n").expect("write the file");
    let mut peer = boot::<P>("\n");
    peer.editor_mut().snippet_dirs = vec![user];
    press(&mut peer, &format!(":e {}<CR>", file.display()));

    press(&mut peer, ":reload-snippets<CR>");
    press(&mut peer, "izzhdr<C-x><C-s>");

    assert_eq!(text(&peer), "project header\n");
}

/// A friendly-snippets checkout loads as it is: its `package.json` says
/// which file serves which languages.
pub fn a_friendly_snippets_checkout_loads<P: PeerUnderTest>() {
    let dir = super::completion::temp_dir("friendly-snippets");
    std::fs::create_dir_all(dir.join("snippets/global")).expect("dirs");
    std::fs::write(
        dir.join("package.json"),
        r#"{ "contributes": { "snippets": [
            { "language": ["plaintext", "rust"], "path": "./snippets/global/all.json" }
        ] } }"#,
    )
    .expect("write the manifest");
    std::fs::write(
        dir.join("snippets/global/all.json"),
        r#"{ "fs": { "prefix": "zzfs", "body": "friendly" } }"#,
    )
    .expect("write the pack");
    let mut peer = boot::<P>("\n");
    peer.editor_mut().snippet_dirs = vec![dir];
    edit_new_file(&mut peer, "lib.rs");

    press(&mut peer, ":reload-snippets<CR>");
    press(&mut peer, "izzfs<C-x><C-s>");

    assert_eq!(text(&peer), "friendly\n");
}

/// Packages under `snippet-packs/` load first: the user's own snippet wins
/// over a package's on a shared prefix (design §8.3's order).
pub fn the_users_snippet_overrides_a_packages<P: PeerUnderTest>() {
    let packs = super::completion::temp_dir("snippet-packs");
    std::fs::create_dir_all(packs.join("friendly")).expect("a package");
    std::fs::write(
        packs.join("friendly/_global.json"),
        r#"{ "theirs": { "prefix": "zzpk", "body": "from the package" } }"#,
    )
    .expect("write the package");
    let user = super::completion::temp_dir("snippet-user-over-pack");
    std::fs::write(
        user.join("_global.json"),
        r#"{ "mine": { "prefix": "zzpk", "body": "mine" } }"#,
    )
    .expect("write the user pack");
    let mut peer = boot::<P>("\n");
    peer.editor_mut().snippet_pack_dirs = vec![packs];
    peer.editor_mut().snippet_dirs = vec![user];

    press(&mut peer, ":reload-snippets<CR>");
    press(&mut peer, "izzpk<C-x><C-s>");

    assert_eq!(text(&peer), "mine\n");
}

// ---- SI.12: choices ----

/// `<C-n>` / `<C-p>` on a focused choice replace it with the next / previous
/// option, wrapping, in every copy — and keep it selected, so typing still
/// overtypes it.
pub fn ctrl_n_and_ctrl_p_cycle_a_choice<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_snippet(
        &mut peer,
        "conformance-pick",
        "zzpick",
        "${1|aa,b,cc|} = $1",
    );
    press(&mut peer, "izzpick<C-x><C-s>");
    assert_eq!(text(&peer), "aa = aa\n", "the first option is inserted");
    assert_selects(&peer, "the choice", 0, 2);

    press(&mut peer, "<C-n>");
    assert_eq!(text(&peer), "b = b\n", "the next option, in every copy");
    assert_selects(&peer, "the cycled choice", 0, 1);

    press(&mut peer, "<C-n><C-n>");
    assert_eq!(
        text(&peer),
        "aa = aa\n",
        "the last option wraps to the first"
    );

    press(&mut peer, "<C-p>");
    assert_eq!(text(&peer), "cc = cc\n", "and back to the last");
    assert_selects(&peer, "after <C-p>", 0, 2);

    press(&mut peer, "x");
    assert_eq!(text(&peer), "x = x\n", "typing overtypes the choice");
}

/// Off a choice, `<C-n>` changes nothing.
pub fn ctrl_n_off_a_choice_changes_nothing<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    install_for_snippet(&mut peer);
    press(&mut peer, "izzfor<C-x><C-s><C-n>");

    assert_eq!(text(&peer), "for idx in iter {  }\n");
    assert_selects(&peer, "the placeholder", 4, 7);
}
