//! LL.12: `Document::append_to_tracked`, the anchored append behind streamed
//! inline output (design `llm-layer.md` §9).
//!
//! The response stays one contiguous span whatever the user types around
//! it; the user's text is never interleaved into it; undo removes the whole
//! response when nothing interleaved and the latest segment otherwise; an
//! Insert session's undo group is neither closed nor absorbed.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document;
use lattice_core::protocol::edit::Edit;
use lattice_core::protocol::position::{Position, Range};
use lattice_core::tracked::{AppendOutcome, Stickiness, TrackedRangeId};
use proptest::prelude::*;

/// A document with an empty `NeverGrows` stream anchor at byte `at`.
fn anchored(text: &str, at: usize) -> (Document, TrackedRangeId) {
    let mut doc = Document::from_text(text);
    let id = TrackedRangeId::next();
    doc.update_tracked_ranges(|t| t.insert(id, at..at, Stickiness::NeverGrows));
    (doc, id)
}

fn span(doc: &Document, id: TrackedRangeId) -> String {
    let r = doc.tracked_ranges().get(id).unwrap();
    doc.text()[r.start..r.end].to_string()
}

fn insert_at_byte(doc: &mut Document, byte: usize, text: &str) {
    let at = doc.buffer().byte_to_position(byte).unwrap();
    doc.apply_edit(Edit::insert(at, text)).unwrap();
}

#[test]
fn an_append_writes_at_the_anchor_and_the_range_grows_over_it() {
    let (mut doc, id) = anchored("before\nafter\n", 7);
    assert_eq!(
        doc.append_to_tracked(id, "Hello").unwrap(),
        AppendOutcome::Appended
    );
    assert_eq!(
        doc.append_to_tracked(id, ", world\n").unwrap(),
        AppendOutcome::Appended
    );
    assert_eq!(doc.text(), "before\nHello, world\nafter\n");
    assert_eq!(span(&doc, id), "Hello, world\n");
    assert_eq!(doc.append_version(), doc.text_version());
}

/// Typing at the insertion point stays after the response, which goes on
/// growing before it: design §9's correction of "typed text stays ahead".
#[test]
fn text_typed_at_the_insertion_point_stays_after_the_response() {
    let (mut doc, id) = anchored("", 0);
    doc.append_to_tracked(id, "Hello").unwrap();
    insert_at_byte(&mut doc, 5, "X");
    doc.append_to_tracked(id, " world").unwrap();
    assert_eq!(doc.text(), "Hello worldX");
    assert_eq!(span(&doc, id), "Hello world");
}

#[test]
fn text_typed_above_moves_the_insertion_point() {
    let (mut doc, id) = anchored("a\nb\n", 2);
    doc.append_to_tracked(id, "R1 ").unwrap();
    insert_at_byte(&mut doc, 0, "top\n");
    doc.append_to_tracked(id, "R2").unwrap();
    assert_eq!(doc.text(), "top\na\nR1 R2b\n");
}

#[test]
fn a_deleted_insertion_point_refuses_the_write() {
    let (mut doc, id) = anchored("one two three", 4);
    doc.append_to_tracked(id, "X").unwrap();
    // Delete across the whole response and beyond.
    doc.apply_edit(Edit::delete(Range::new(
        Position::new(0, 2),
        Position::new(0, 8),
    )))
    .unwrap();
    let before = doc.text();
    assert_eq!(
        doc.append_to_tracked(id, "more").unwrap(),
        AppendOutcome::Collapsed
    );
    assert_eq!(doc.text(), before, "nothing written");
}

#[test]
fn an_unknown_range_is_refused() {
    let mut doc = Document::from_text("x");
    assert_eq!(
        doc.append_to_tracked(TrackedRangeId::next(), "y").unwrap(),
        AppendOutcome::Unknown
    );
    assert_eq!(doc.text(), "x");
}

#[test]
fn one_undo_removes_a_whole_response_when_nothing_interleaved() {
    let (mut doc, id) = anchored("a\n", 2);
    for chunk in ["one ", "two ", "three"] {
        doc.append_to_tracked(id, chunk).unwrap();
    }
    doc.undo().unwrap();
    assert_eq!(doc.text(), "a\n");
    assert!(doc.undo().is_err(), "one entry, not three");
}

/// A user edit seals the stream's entry: `u` takes the latest segment, then
/// the user's edit, then the first segment. Linear, nothing amended under.
#[test]
fn an_interleaved_edit_splits_the_response_into_segments() {
    let (mut doc, id) = anchored("x\n", 2);
    doc.append_to_tracked(id, "first ").unwrap();
    insert_at_byte(&mut doc, 0, "U");
    doc.append_to_tracked(id, "second").unwrap();
    assert_eq!(doc.text(), "Ux\nfirst second");
    doc.undo().unwrap();
    assert_eq!(doc.text(), "Ux\nfirst ");
    doc.undo().unwrap();
    assert_eq!(doc.text(), "x\nfirst ");
    doc.undo().unwrap();
    assert_eq!(doc.text(), "x\n");
}

/// The stream never takes the Insert session's coalescing group: typing
/// before and after an append are separate entries, typing after it still
/// coalesces, and the stream's entry holds no typed character.
#[test]
fn an_insert_sessions_undo_group_is_untouched() {
    let (mut doc, id) = anchored("\n", 1);
    doc.begin_undo_group();
    insert_at_byte(&mut doc, 0, "a");
    doc.append_to_tracked(id, "R").unwrap();
    insert_at_byte(&mut doc, 1, "b");
    insert_at_byte(&mut doc, 2, "c");
    doc.end_undo_group();
    assert_eq!(doc.text(), "abc\nR");
    doc.undo().unwrap();
    assert_eq!(
        doc.text(),
        "a\nR",
        "`bc` coalesced, after the stream's entry"
    );
    doc.undo().unwrap();
    assert_eq!(doc.text(), "a\n", "the stream's entry held only `R`");
    doc.undo().unwrap();
    assert_eq!(doc.text(), "\n");
}

#[test]
fn the_caret_at_or_after_the_insertion_point_moves_with_the_response() {
    use lattice_core::protocol::selection::{Selection, SelectionSet};
    let (mut doc, id) = anchored("ab\ncd\n", 3);
    doc.set_selections(SelectionSet::single(Selection::cursor(Position::new(1, 1))));
    doc.append_to_tracked(id, "new\n").unwrap();
    assert_eq!(doc.selections().primary().head, Position::new(2, 1));
    doc.set_selections(SelectionSet::single(Selection::cursor(Position::new(0, 1))));
    doc.append_to_tracked(id, "more\n").unwrap();
    assert_eq!(
        doc.selections().primary().head,
        Position::new(0, 1),
        "a caret above stays put"
    );
}

#[test]
fn a_save_during_a_stream_does_not_survive_the_next_write() {
    let (mut doc, id) = anchored("", 0);
    doc.append_to_tracked(id, "one").unwrap();
    doc.mark_clean();
    assert!(!doc.dirty());
    doc.append_to_tracked(id, " two").unwrap();
    assert!(doc.dirty(), "the amended entry is not what was saved");
}

#[test]
fn a_stream_write_clears_redo() {
    let (mut doc, id) = anchored("x", 1);
    insert_at_byte(&mut doc, 0, "U");
    doc.undo().unwrap();
    doc.append_to_tracked(id, "R").unwrap();
    assert!(doc.redo().is_err());
}

#[derive(Debug, Clone)]
enum Op {
    Append(String),
    /// A user insert of `#` at a position chosen outside the response's
    /// interior: before it, at either edge, or after it.
    Type(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        "[a-z ]{1,6}".prop_map(Op::Append),
        (0usize..1000).prop_map(Op::Type),
    ]
}

proptest! {
    /// Whatever the user types around a stream, the response is exactly the
    /// appended chunks in order, contiguous, and every typed character is
    /// outside it.
    #[test]
    fn interleaved_typing_never_breaks_the_response(
        seed in "[x\n]{0,20}",
        at in 0usize..21,
        ops in proptest::collection::vec(op(), 1..40),
    ) {
        let at = at.min(seed.len());
        let (mut doc, id) = anchored(&seed, at);
        let mut expected = String::new();
        let mut typed = 0usize;
        for op in ops {
            match op {
                Op::Append(chunk) => {
                    prop_assert_eq!(doc.append_to_tracked(id, &chunk).unwrap(), AppendOutcome::Appended);
                    expected.push_str(&chunk);
                }
                Op::Type(pick) => {
                    let r = doc.tracked_ranges().get(id).unwrap();
                    let len = doc.text().len();
                    // Positions outside the open interval (start, end).
                    let outside: Vec<usize> =
                        (0..=len).filter(|&b| b <= r.start || b >= r.end).collect();
                    let byte = outside[pick % outside.len()];
                    insert_at_byte(&mut doc, byte, "#");
                    typed += 1;
                }
            }
        }
        prop_assert_eq!(span(&doc, id), expected.clone());
        let text = doc.text();
        prop_assert_eq!(text.matches('#').count(), typed);
        prop_assert_eq!(text.len(), seed.len() + expected.len() + typed);
    }
}
