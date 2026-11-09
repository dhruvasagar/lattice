//! LL.12: `Document::append_to_tracked`, the anchored append behind streamed
//! inline output (design `llm-layer.md` §9).
//!
//! The response stays one contiguous span whatever the user types around
//! it; the user's text is never interleaved into it; undo removes the whole
//! response when nothing interleaved and the latest segment otherwise; an
//! Insert session's undo group is neither closed nor absorbed.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document;
use lattice_core::document::BaseMismatch;
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

// ---- LL.13a: writes positioned before an append, and `u` during a stream ----

fn pos(doc: &Document, byte: usize) -> Position {
    doc.buffer().byte_to_position(byte).unwrap()
}

/// The race a stream opens: the host positions a key from its caret, and an
/// append lands before the key reaches the document. Rebased, the key goes
/// where the caret visibly was: after the response.
#[test]
fn a_key_typed_at_the_point_before_an_append_lands_after_it() {
    let (mut doc, id) = anchored("ab\n", 2);
    doc.append_to_tracked(id, "R1").unwrap();
    let base = doc.text_version();
    let caret = pos(&doc, 4); // after "R1": the end of the response
    doc.append_to_tracked(id, "R2").unwrap();
    doc.apply_edit_based_on(Edit::insert(caret, "X"), base)
        .unwrap();
    assert_eq!(doc.text(), "abR1R2X\n");
    assert_eq!(span(&doc, id), "R1R2");
}

#[test]
fn a_key_below_the_point_moves_with_the_append() {
    let (mut doc, id) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    let caret = Position::new(1, 1);
    doc.append_to_tracked(id, "new\n").unwrap();
    doc.apply_edit_based_on(Edit::insert(caret, "X"), base)
        .unwrap();
    assert_eq!(doc.text(), "ab\nnew\ncXd\n");
}

#[test]
fn a_key_above_the_point_is_not_moved() {
    let (mut doc, id) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    doc.append_to_tracked(id, "new\n").unwrap();
    doc.apply_edit_based_on(Edit::insert(Position::new(0, 1), "X"), base)
        .unwrap();
    assert_eq!(doc.text(), "aXb\nnew\ncd\n");
}

/// Backspace at the end of the response, against a version before the next
/// chunk: it deletes the character the user saw, not the chunk with it.
#[test]
fn a_deletion_ending_at_the_point_does_not_take_the_append() {
    let (mut doc, id) = anchored("", 0);
    doc.append_to_tracked(id, "abc").unwrap();
    let base = doc.text_version();
    doc.append_to_tracked(id, "def").unwrap();
    doc.apply_edit_based_on(
        Edit::delete(Range::new(Position::new(0, 2), Position::new(0, 3))),
        base,
    )
    .unwrap();
    assert_eq!(doc.text(), "abdef");
}

/// Anything but an append in between, and the position cannot be corrected:
/// it is used as given, which is what happened before rebasing existed.
#[test]
fn a_non_append_change_since_leaves_the_edit_as_given() {
    let (mut doc, id) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    insert_at_byte(&mut doc, 0, "Z");
    doc.append_to_tracked(id, "new\n").unwrap();
    doc.apply_edit_based_on(Edit::insert(Position::new(1, 1), "X"), base)
        .unwrap();
    // `(1, 1)` of the text as it now stands: inside the appended line.
    assert_eq!(doc.text(), "Zab\nnXew\ncd\n");
}

#[test]
fn a_caret_written_through_late_is_moved_by_the_append_it_missed() {
    use lattice_core::protocol::selection::{Selection, SelectionSet};
    let (mut doc, id) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    doc.append_to_tracked(id, "one\ntwo\n").unwrap();
    doc.set_selections_based_on(
        SelectionSet::single(Selection::cursor(Position::new(1, 1))),
        base,
    );
    assert_eq!(doc.selections().primary().head, Position::new(3, 1));
}

#[test]
fn the_version_before_trailing_appends_is_what_an_editor_has_seen() {
    let (mut doc, id) = anchored("x\n", 2);
    assert_eq!(doc.pre_append_version(), doc.text_version());
    insert_at_byte(&mut doc, 0, "U");
    let mine = doc.text_version();
    doc.append_to_tracked(id, "a").unwrap();
    doc.append_to_tracked(id, "b").unwrap();
    assert_eq!(doc.pre_append_version(), mine);
    insert_at_byte(&mut doc, 0, "V");
    assert_eq!(doc.pre_append_version(), doc.text_version());
}

#[test]
fn an_undo_during_a_stream_refuses_its_next_chunk() {
    let (mut doc, id) = anchored("x\n", 2);
    doc.append_to_tracked(id, "one").unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.text(), "x\n", "the undo took the response");
    assert_eq!(
        doc.append_to_tracked(id, " two").unwrap(),
        AppendOutcome::Undone
    );
    assert_eq!(doc.text(), "x\n", "nothing written after the `u`");
}

/// Any `u` stops the stream, not only one that reached its text: the user
/// is going back, and a response still arriving would fight them.
#[test]
fn an_undo_of_the_users_own_edit_also_stops_the_stream() {
    let (mut doc, id) = anchored("x\n", 2);
    doc.append_to_tracked(id, "one").unwrap();
    insert_at_byte(&mut doc, 0, "U");
    doc.undo().unwrap();
    assert_eq!(
        doc.append_to_tracked(id, " two").unwrap(),
        AppendOutcome::Undone
    );
    assert_eq!(doc.text(), "x\none");
}

#[test]
fn an_undo_before_the_first_chunk_does_not_stop_it() {
    let (mut doc, id) = anchored("x\n", 0);
    insert_at_byte(&mut doc, 2, "U");
    doc.undo().unwrap();
    assert_eq!(
        doc.append_to_tracked(id, "R").unwrap(),
        AppendOutcome::Appended
    );
}

proptest! {
    /// A key typed at a caret read before any number of appends lands where
    /// it would have with no stream running: before the response if the
    /// caret was before the point, after the whole response if at or after.
    #[test]
    fn a_key_typed_against_an_old_version_lands_on_its_text(
        seed in "[ab\n]{0,16}",
        point in 0usize..17,
        caret in 0usize..17,
        chunks in proptest::collection::vec("[xy\n]{1,4}", 1..8),
    ) {
        let point = point.min(seed.len());
        let caret = caret.min(seed.len());
        let (mut doc, id) = anchored(&seed, point);
        let base = doc.text_version();
        let at = pos(&doc, caret);
        let response: String = chunks.concat();
        for c in &chunks {
            doc.append_to_tracked(id, c).unwrap();
        }
        doc.apply_edit_based_on(Edit::insert(at, "#"), base).unwrap();
        let expected = if caret < point {
            format!("{}#{}{}{}", &seed[..caret], &seed[caret..point], response, &seed[point..])
        } else {
            format!("{}{}{}#{}", &seed[..point], response, &seed[point..caret], &seed[caret..])
        };
        prop_assert_eq!(doc.text(), expected);
        prop_assert_eq!(span(&doc, id), response);
    }
}

// ---- Batches positioned before an append ----

fn del(a: (u32, u32), b: (u32, u32)) -> Edit {
    Edit::delete(Range::new(Position::new(a.0, a.1), Position::new(b.0, b.1)))
}

/// `dd` on the line under the response, against a version before a chunk
/// arrived: it deletes the line the user saw, not the chunk.
#[test]
fn a_batch_is_carried_over_an_append_it_had_not_seen() {
    let (mut doc, id) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    doc.append_to_tracked(id, "new\n").unwrap();
    doc.apply_edit_batch_based_on(vec![del((1, 0), (2, 0))], base, BaseMismatch::Refuse)
        .unwrap();
    assert_eq!(doc.text(), "ab\nnew\n");
}

/// The second edit is in the coordinates the first produces, and meets the
/// append where the first edit left it.
#[test]
fn each_edit_of_a_batch_meets_the_append_where_the_last_left_it() {
    let (mut doc, id) = anchored("ab\ncd\nef\n", 3);
    let base = doc.text_version();
    doc.append_to_tracked(id, "X\n").unwrap();
    doc.apply_edit_batch_based_on(
        vec![
            Edit::insert(Position::new(0, 0), "1\n"),
            del((3, 0), (4, 0)),
        ],
        base,
        BaseMismatch::Refuse,
    )
    .unwrap();
    assert_eq!(doc.text(), "1\nab\nX\ncd\n");
}

/// A change starting right after the append point keeps the appended text,
/// as a single rebased edit does.
#[test]
fn a_change_at_the_point_leaves_the_append_before_it() {
    let (mut doc, id) = anchored("abcd\n", 2);
    let base = doc.text_version();
    doc.append_to_tracked(id, "XY").unwrap();
    doc.apply_edit_batch_based_on(
        vec![
            Edit::replace(Range::new(Position::new(0, 2), Position::new(0, 3)), "C"),
            Edit::insert(Position::new(0, 4), "!"),
        ],
        base,
        BaseMismatch::Refuse,
    )
    .unwrap();
    assert_eq!(doc.text(), "abXYCd!\n");
}

/// An edit spanning the point takes the appended text with it; the append
/// then moves nothing for the edits after it.
#[test]
fn an_append_inside_a_deletion_goes_with_it() {
    let (mut doc, id) = anchored("abcd\n", 2);
    let base = doc.text_version();
    doc.append_to_tracked(id, "XY").unwrap();
    doc.apply_edit_batch_based_on(
        vec![del((0, 1), (0, 3)), Edit::insert(Position::new(0, 1), "Z")],
        base,
        BaseMismatch::Refuse,
    )
    .unwrap();
    assert_eq!(doc.text(), "aZd\n");
}

#[test]
fn a_rebased_batch_is_one_undo_unit() {
    let (mut doc, id) = anchored("ab\ncd\nef\n", 3);
    let base = doc.text_version();
    doc.append_to_tracked(id, "X\n").unwrap();
    let before = doc.text();
    doc.apply_edit_batch_based_on(
        vec![del((1, 0), (2, 0)), del((1, 0), (2, 0))],
        base,
        BaseMismatch::Refuse,
    )
    .unwrap();
    assert_eq!(doc.text(), "ab\nX\n");
    doc.undo().unwrap();
    assert_eq!(doc.text(), before);
}

/// Anything but an append since: `Refuse` applies nothing and says so;
/// `UseAsGiven` applies the batch where it stands, as before rebasing.
#[test]
fn a_non_append_change_since_refuses_or_applies_as_given() {
    let (mut doc, _) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    insert_at_byte(&mut doc, 0, "Z");
    let err = doc
        .apply_edit_batch_based_on(vec![del((1, 0), (2, 0))], base, BaseMismatch::Refuse)
        .unwrap_err();
    assert!(matches!(err, lattice_core::CoreError::Stale), "{err:?}");
    assert_eq!(doc.text(), "Zab\ncd\n", "nothing applied");

    doc.apply_edit_batch_based_on(vec![del((1, 0), (2, 0))], base, BaseMismatch::UseAsGiven)
        .unwrap();
    assert_eq!(doc.text(), "Zab\n");
}

#[test]
fn with_nothing_since_a_batch_applies_as_given() {
    let (mut doc, _) = anchored("ab\ncd\n", 3);
    let base = doc.text_version();
    doc.apply_edit_batch_based_on(vec![del((0, 0), (1, 0))], base, BaseMismatch::Refuse)
        .unwrap();
    assert_eq!(doc.text(), "cd\n");
}

proptest! {
    /// A batch of line edits that never touch the response's line lands as
    /// if it had been applied before the stream wrote: the same text as
    /// applying the batch first and appending the chunks after, through the
    /// anchor the batch moved.
    #[test]
    fn a_rebased_batch_equals_the_batch_applied_before_the_appends(
        lines in 2usize..8,
        anchor_line in 0usize..8,
        ops in proptest::collection::vec((any::<bool>(), 0usize..10), 1..6),
        chunks in proptest::collection::vec("[xy\n]{1,4}", 1..5),
    ) {
        let seed: String = (0..lines).map(|i| format!("l{i}\n")).collect();
        let anchor_line = anchor_line.min(lines - 1);
        // Mid-line, so no line edit's endpoint is the anchor.
        let at = seed.lines().take(anchor_line).map(|l| l.len() + 1).sum::<usize>() + 1;

        let mut line_count = lines;
        let mut l = anchor_line;
        let mut edits = Vec::new();
        for (insert, i) in ops {
            if insert {
                let i = i % (line_count + 1);
                edits.push(Edit::insert(Position::new(i as u32, 0), "n\n"));
                if i <= l { l += 1; }
                line_count += 1;
            } else {
                let i = i % line_count;
                if i == l || line_count == 1 { continue; }
                edits.push(del((i as u32, 0), (i as u32 + 1, 0)));
                if i < l { l -= 1; }
                line_count -= 1;
            }
        }

        let (mut oracle, oid) = anchored(&seed, at);
        oracle.apply_edit_batch(edits.clone()).unwrap();
        for c in &chunks { oracle.append_to_tracked(oid, c).unwrap(); }

        let (mut doc, id) = anchored(&seed, at);
        let base = doc.text_version();
        for c in &chunks { doc.append_to_tracked(id, c).unwrap(); }
        doc.apply_edit_batch_based_on(edits, base, BaseMismatch::Refuse).unwrap();

        prop_assert_eq!(doc.text(), oracle.text());
    }
}
