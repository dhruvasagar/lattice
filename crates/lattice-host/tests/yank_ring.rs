//! YR.1 — the yank ring.
//!
//! `store_yank` is already the single write seam for every yank *and* every
//! delete, so the ring hangs off that one call and nothing else changes.
//!
//! The property most at risk here is not the ring's own behaviour but
//! **CB.1's**: deletes push to the ring and must still stay out of the system
//! clipboard. Those two rules read like a contradiction and live four lines
//! apart, which is exactly the shape someone "tidies" later. The two stores
//! have different blast radii — the clipboard is shared with every other
//! application, the ring is internal and bounded — and the test below is what
//! keeps that distinction from being collapsed by accident.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::Register;
use lattice_grammar::effect::YankKind;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("alpha\nbravo\ncharlie\n"))
}

/// `Editor::boot` registers a `FakeClipboard` by default (CB.0). Grab the
/// SAME handle `store_yank` consults, rather than installing a spy — the
/// services registry is an `Arc` and immutable after boot, and a test that
/// registered its own would be asserting against a clipboard the code under
/// test never writes to.
fn test_clipboard(editor: &Editor) -> lattice_core::ClipboardHandle {
    (*editor
        .services
        .get::<lattice_core::ClipboardHandle>()
        .expect("Editor::boot registers a default ClipboardHandle (CB.0)"))
    .clone()
}

fn contents(editor: &Editor) -> Vec<String> {
    editor.yank_ring.iter().map(|e| e.content.clone()).collect()
}

/// `explicit_yank: true` is a yank; `false` is a delete. That flag is the
/// only thing distinguishing them at this seam, which is why it is worth
/// naming rather than passing a bare bool at each call site.
fn yank(editor: &mut Editor, text: &str) {
    editor.store_yank(
        Register::Unnamed,
        text.to_string(),
        YankKind::Charwise,
        true,
    );
}

fn delete(editor: &mut Editor, text: &str) {
    editor.store_yank(
        Register::Unnamed,
        text.to_string(),
        YankKind::Charwise,
        false,
    );
}

#[test]
fn a_yank_lands_in_the_ring() {
    let mut editor = boot();
    yank(&mut editor, "alpha");
    assert_eq!(contents(&editor), vec!["alpha"]);
}

#[test]
fn a_delete_lands_in_the_ring_too() {
    let mut editor = boot();
    delete(&mut editor, "bravo");
    assert_eq!(
        contents(&editor),
        vec!["bravo"],
        "\"get back the line I just deleted\" is the most common reason to \
         open the picker; a yank-only ring would decline it"
    );
}

#[test]
fn the_newest_entry_is_first() {
    let mut editor = boot();
    yank(&mut editor, "one");
    yank(&mut editor, "two");
    yank(&mut editor, "three");
    assert_eq!(contents(&editor), vec!["three", "two", "one"]);
}

/// CB.1's property, guarded from the slice most likely to break it.
#[test]
fn a_delete_reaches_the_ring_but_not_the_clipboard() {
    let mut editor = boot();
    let clipboard = test_clipboard(&editor);
    assert_eq!(
        clipboard.read(),
        None,
        "precondition: clipboard starts empty"
    );

    delete(&mut editor, "deleted-text");

    assert_eq!(contents(&editor), vec!["deleted-text"], "the ring took it");
    assert_eq!(
        clipboard.read(),
        None,
        "the system clipboard must not take a delete — an incidental `x` \
         would clobber what the user copied from another application"
    );
}

/// ...and the yank half still mirrors, so the test above is measuring the
/// yank/delete distinction rather than a clipboard that never works.
#[test]
fn a_yank_still_reaches_the_clipboard() {
    let mut editor = boot();
    let clipboard = test_clipboard(&editor);
    yank(&mut editor, "yanked-text");
    assert_eq!(clipboard.read(), Some("yanked-text".to_string()));
}

#[test]
fn consecutive_duplicates_collapse() {
    let mut editor = boot();
    yank(&mut editor, "same");
    yank(&mut editor, "same");
    yank(&mut editor, "same");
    assert_eq!(
        contents(&editor),
        vec!["same"],
        "`yy` held down must not fill the ring with rows the picker cannot \
         tell apart"
    );
}

/// The other half of the duplicate rule, and deliberately different: a
/// repeat that is *not* consecutive is a real event, and the useful answer
/// is to move it to the top rather than to add a second identical row.
#[test]
fn a_non_consecutive_repeat_is_promoted_not_duplicated() {
    let mut editor = boot();
    yank(&mut editor, "old");
    yank(&mut editor, "other");
    yank(&mut editor, "old");
    assert_eq!(contents(&editor), vec!["old", "other"]);
}

/// Kind is part of an entry's identity: the same text yanked linewise and
/// charwise pastes differently, so collapsing them would make one of the two
/// pastes unreachable.
#[test]
fn the_same_text_in_two_kinds_is_two_entries() {
    let mut editor = boot();
    editor.store_yank(
        Register::Unnamed,
        "text".to_string(),
        YankKind::Charwise,
        true,
    );
    editor.store_yank(
        Register::Unnamed,
        "text".to_string(),
        YankKind::Linewise,
        true,
    );
    assert_eq!(editor.yank_ring.len(), 2);
}

#[test]
fn the_black_hole_register_pushes_nothing() {
    let mut editor = boot();
    editor.store_yank(
        Register::BlackHole,
        "vanishes".to_string(),
        YankKind::Charwise,
        true,
    );
    assert!(editor.yank_ring.is_empty());
}

#[test]
fn the_ring_evicts_oldest_first_at_capacity() {
    let mut editor = boot();
    editor
        .config
        .parse_and_set_command("yank.ring.size=3")
        .expect("settable");
    editor.drain_option_changes();

    for t in ["a", "b", "c", "d"] {
        yank(&mut editor, t);
    }

    assert_eq!(
        contents(&editor),
        vec!["d", "c", "b"],
        "oldest-first eviction is what lets YR.2's \"0-\"9 projection be \
         sound — it reads the newest entries, so dropping from the back \
         cannot change what \"9 means"
    );
}

/// The capacity is read at push time, so lowering it takes effect on the next
/// yank rather than at the next restart.
#[test]
fn lowering_the_capacity_takes_effect_on_the_next_yank() {
    let mut editor = boot();
    for t in ["a", "b", "c", "d", "e"] {
        yank(&mut editor, t);
    }
    assert_eq!(editor.yank_ring.len(), 5);

    editor
        .config
        .parse_and_set_command("yank.ring.size=2")
        .expect("settable");
    editor.drain_option_changes();
    yank(&mut editor, "f");

    assert_eq!(contents(&editor), vec!["f", "e"]);
}

#[test]
fn a_capacity_of_zero_disables_the_ring() {
    let mut editor = boot();
    editor
        .config
        .parse_and_set_command("yank.ring.size=0")
        .expect("settable");
    editor.drain_option_changes();
    yank(&mut editor, "nothing");
    assert!(editor.yank_ring.is_empty());
}

/// The ring is additive to the register model, not a replacement: a named
/// register still receives its content.
#[test]
fn a_named_register_still_gets_its_content() {
    let mut editor = boot();
    editor.store_yank(
        Register::Named('a'),
        "for-a".to_string(),
        YankKind::Charwise,
        true,
    );
    assert_eq!(
        editor
            .registers
            .get(&Register::Named('a'))
            .map(|e| e.content.as_str()),
        Some("for-a")
    );
    assert_eq!(contents(&editor), vec!["for-a"], "and the ring took it too");
}
