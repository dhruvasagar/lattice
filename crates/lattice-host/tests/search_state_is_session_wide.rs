//! Search state ownership: the pattern is session-wide, the match ranges are not.
//!
//! Vim keeps the last search pattern in the `/` register — a SESSION
//! construct. It survives `:e other-file`, `:bn`, a tab switch, everything.
//! `n` in a freshly-opened buffer searches for the pattern you last typed
//! somewhere else. `hlsearch` likewise applies in every window: each one
//! highlights *its own* occurrences of that one pattern.
//!
//! Lattice had these exactly inverted:
//!
//! * `last_search` (the pattern — genuinely global) was wiped by the
//!   fresh-file open path, so `n` after `:e` reported `E35: no previous
//!   regular expression` and kept reporting it forever.
//! * `all_matches` (resolved byte ranges — inherently per-buffer) was
//!   carried across buffer switches untouched, so the ranges computed
//!   against one file were painted onto the text of another.
//!
//! These tests pin the correct split: pattern persists, ranges are
//! recomputed against whatever buffer just became active.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use lattice_core::Document as CoreDocument;
use lattice_grammar::SearchDirection;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

fn tmp(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lattice-search-state-{tag}-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The reported bug, minimally: search in one file, open another that has
/// no match, and `n` is dead from then on — in every buffer, not just that
/// one. The pattern is the `/` register; opening a file does not clear it.
#[test]
fn opening_a_file_keeps_the_search_pattern() {
    let dir = tmp("e35");
    let other = dir.join("other.txt");
    std::fs::write(&other, "nothing to find here\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text("alpha beta\n"));
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);
    assert!(e.last_search.is_some(), "sanity: the search was recorded");

    e.do_edit(Some(other), false);

    assert!(
        e.last_search.is_some(),
        "`:e other-file` must not clear the `/` register — `n` would report E35 forever",
    );
    assert_eq!(e.last_search.as_ref().unwrap().pattern, "alpha");
}

/// `n` in the newly-opened buffer runs the pattern rather than erroring.
/// This is the user-visible half of the test above: no `E35`.
#[test]
fn repeat_search_works_in_a_newly_opened_buffer() {
    let dir = tmp("repeat");
    let other = dir.join("other.txt");
    std::fs::write(&other, "one alpha two\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text("alpha beta\n"));
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);
    e.do_edit(Some(other), false);

    e.repeat_search(false);

    let msg = e.last_message.as_ref().map(|m| m.text.clone());
    assert!(
        !msg.as_deref().unwrap_or("").contains("E35"),
        "`n` after opening a file must not report E35; got {msg:?}",
    );
    assert_eq!(
        e.cursor,
        Position::new(0, 4),
        "`n` lands on this buffer's own occurrence of the pattern",
    );
}

/// hlsearch ranges are buffer-local resolutions of a session-wide pattern.
/// Switching to a buffer with no occurrences must leave nothing highlighted
/// — carrying the previous buffer's ranges over paints them at byte offsets
/// that mean something else entirely in the new text.
#[test]
fn switching_to_a_buffer_without_matches_clears_the_highlight() {
    let dir = tmp("bleed");
    let other = dir.join("other.txt");
    std::fs::write(&other, "x\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text("alpha alpha alpha\n"));
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);
    assert_eq!(
        e.all_matches.len(),
        3,
        "sanity: three matches in the origin"
    );

    e.do_edit(Some(other), false);

    assert!(
        e.all_matches.is_empty(),
        "stale ranges from the previous buffer bled into a buffer that has no matches: {:?}",
        e.all_matches,
    );
    assert!(
        e.current_match.is_none(),
        "the primary highlight belongs to the match the cursor was on, in the other buffer",
    );
}

/// The other side of the same rule: a buffer that DOES contain the pattern
/// highlights its OWN occurrences. Vim's hlsearch is on in every window.
#[test]
fn switching_to_a_buffer_with_matches_rehighlights_it() {
    let dir = tmp("rehl");
    let other = dir.join("other.txt");
    std::fs::write(&other, "pad\npad alpha\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text("alpha alpha alpha\n"));
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);

    e.do_edit(Some(other), false);

    assert_eq!(
        e.all_matches.len(),
        1,
        "the new buffer's single occurrence is the whole highlight set",
    );
    assert_eq!(e.all_matches[0].start, Position::new(1, 4));
}

/// Switching back to an ALREADY-OPEN buffer (`:bn` / `:b N` / a tab switch)
/// goes through `activate_document`, not the fresh-open path. Same rule.
#[test]
fn switching_between_open_buffers_recomputes_the_highlight() {
    let dir = tmp("switch");
    let other = dir.join("other.txt");
    std::fs::write(&other, "no hits\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text("alpha alpha alpha\n"));
    let origin = e.document_buffer_id;
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);
    e.do_edit(Some(other), false);
    let second = e.document_buffer_id;
    assert_ne!(origin, second, "sanity: two distinct buffers");

    // Back to the buffer that has the matches.
    e.activate_document(origin);
    assert_eq!(
        e.all_matches.len(),
        3,
        "returning to the searched buffer restores its own highlights",
    );

    // And away again.
    e.activate_document(second);
    assert!(
        e.all_matches.is_empty(),
        "switching to an open buffer with no matches must not keep the other buffer's ranges: {:?}",
        e.all_matches,
    );
}

/// The headline symptom as reported: hlsearch bleeding across a split
/// (and, by the same funnel, across tabs — `do_switch_to_tab` and
/// `activate_pane` both land in `load_active_pane`).
///
/// This path is the one that had NO handling at all: `activate_document`
/// and `do_edit` at least cleared the ranges by hand, but a pane switch
/// swapped the document out from under them and left the old buffer's
/// byte offsets to be painted over the new buffer's text.
#[test]
fn moving_between_panes_rehighlights_each_buffer() {
    use lattice_core::ui::pane::SplitOrientation;

    let dir = tmp("panes");
    let other = dir.join("other.txt");
    std::fs::write(&other, "not a single hit\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text("alpha alpha alpha\n"));
    let searched = e.document_buffer_id;
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);
    assert_eq!(e.all_matches.len(), 3, "sanity");

    // Split, and point the new pane at the other file.
    let new_idx = e.pane_tree.split_active(SplitOrientation::Vertical);
    e.activate_pane(new_idx);
    e.do_edit(Some(other), false);
    let unsearched = e.document_buffer_id;
    assert_ne!(searched, unsearched, "sanity: the panes show two buffers");
    assert!(
        e.all_matches.is_empty(),
        "the pane showing a match-free buffer must highlight nothing: {:?}",
        e.all_matches,
    );

    // Back to the pane that has the matches.
    let searched_idx = e
        .pane_tree
        .leaves()
        .iter()
        .position(|l| l.buffer_id == searched)
        .expect("the original pane is still open");
    e.activate_pane(searched_idx);
    assert_eq!(
        e.all_matches.len(),
        3,
        "returning to the searched pane restores its own highlights",
    );

    // And across again — this is the direction that bled.
    e.activate_pane(new_idx);
    assert!(
        e.all_matches.is_empty(),
        "the searched buffer's ranges bled into the other pane: {:?}",
        e.all_matches,
    );
}

/// Typing a pattern on the `/` line must not have its live preview
/// clobbered. While the search line is open the active document is the
/// `*search-line*` synthetic buffer, but the preview resolves against
/// the buffer being searched — so the swap seam has to stand back.
#[test]
fn the_search_line_preview_owns_the_highlight_while_it_is_open() {
    let mut e = Editor::boot(CoreDocument::from_text("alpha alpha alpha\n"));
    e.execute_search("alpha", SearchDirection::Forward, Position::ZERO);
    assert_eq!(e.all_matches.len(), 3);

    e.resync_hlsearch_to_active_buffer();
    assert_eq!(
        e.all_matches.len(),
        3,
        "a resync on the searched buffer is a no-op, not a wipe",
    );
}
