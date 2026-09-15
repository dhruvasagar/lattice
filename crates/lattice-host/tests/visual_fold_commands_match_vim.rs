//! VM.3h: the fold commands in Visual (and the recursive ones in Normal)
//! behave as vim 9.2 does, row for row.
//!
//! Every expectation here was observed in real vim, run headless with
//! `-u NONE` and `foldmethod=manual`, not read off the help text. The help
//! gets two of them wrong: it describes `zC` and `zD` the same way ("in the
//! selected area") though `zC` reaches enclosing folds and `zD` doesn't, and it
//! says `zO` leaves folds that don't contain the cursor alone, though it opens
//! the folds nested below the cursor. The scripts that produced each row are
//! `vimcheck2.vim`, `vimcheck_za2.vim`, `vimcheck_normal_zOCD.vim` and
//! `vimcheck_normal_zOC3.vim` from the VM.3h work.
//!
//! Lines below are 0-based; the vim scripts' 1-based line numbers are given in
//! each test's doc for cross-checking.
//!
//! These are single, non-operator chords, so `dispatch_chord` drives them
//! faithfully. `zf` is an operator and is tested over real keystrokes in
//! `lattice-ui-tui` instead.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::Fold;
use lattice_grammar::ModalState;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    let text: String = (1..=14).map(|n| format!("line {n}\n")).collect();
    Editor::boot(CoreDocument::from_text(&text))
}

fn fold(start_line: u32, end_line: u32, closed: bool) -> Fold {
    Fold {
        start_line,
        end_line,
        closed,
        identity: None,
    }
}

fn press(editor: &mut Editor, keys: &str) {
    let mut partial = Vec::new();
    for c in keys.chars() {
        let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
}

/// `Some(closed)` for the fold spanning exactly `start..=end`, `None` if it's
/// gone.
fn state(editor: &Editor, start_line: u32, end_line: u32) -> Option<bool> {
    editor
        .folds
        .iter()
        .find(|f| f.start_line == start_line && f.end_line == end_line)
        .map(|f| f.closed)
}

/// Linewise-select `lo..=hi`: `V` on `lo`, cursor moved to `hi` directly, so no
/// motion can reshape the selection around a closed fold (the hazard that made
/// the first vim run untrustworthy).
fn select_lines(editor: &mut Editor, lo: u32, hi: u32) {
    editor.cursor.line = lo;
    editor.cursor.byte = 0;
    press(editor, "V");
    assert!(
        matches!(editor.modal, ModalState::Visual(_)),
        "test premise: `V` enters Visual"
    );
    editor.cursor.line = hi;
}

/// vim layout for the Visual rows: outer 2–7 ⊃ inner 3–4, separate 10–11.
fn visual_layout(editor: &mut Editor, closed: bool) {
    editor.folds = vec![fold(1, 6, closed), fold(2, 3, closed), fold(9, 10, closed)];
}

/// vim layout for the Normal rows: outer 2–9 ⊃ mid 3–6 ⊃ inner 4–5, separate
/// 11–12.
fn nested_layout(editor: &mut Editor, closed: bool) {
    editor.folds = vec![
        fold(1, 8, closed),
        fold(2, 5, closed),
        fold(3, 4, closed),
        fold(10, 11, closed),
    ];
}

fn assert_left_visual(editor: &Editor) {
    assert_eq!(
        editor.modal,
        ModalState::Normal,
        "the Visual form ends Visual"
    );
}

// ── Visual ────────────────────────────────────────────────────────────────

/// vim: all closed, select 1–4, `zo` → outer opens, inner 3–4 stays closed,
/// 10–11 untouched.
#[test]
fn visual_zo_opens_one_level_and_leaves_the_inner_fold_closed() {
    let mut e = boot();
    visual_layout(&mut e, true);
    select_lines(&mut e, 0, 3);
    press(&mut e, "zo");
    assert_eq!(state(&e, 1, 6), Some(false), "outer opens");
    assert_eq!(state(&e, 2, 3), Some(true), "inner stays closed: one level");
    assert_eq!(
        state(&e, 9, 10),
        Some(true),
        "a fold outside the selection is untouched"
    );
    assert_left_visual(&e);
}

/// vim: all closed, select 1–4, `zO` → outer and inner open; 10–11 untouched.
#[test]
fn visual_z_upper_o_opens_the_nested_folds_too() {
    let mut e = boot();
    visual_layout(&mut e, true);
    select_lines(&mut e, 0, 3);
    press(&mut e, "zO");
    assert_eq!(state(&e, 1, 6), Some(false));
    assert_eq!(state(&e, 2, 3), Some(false));
    assert_eq!(state(&e, 9, 10), Some(true));
    assert_left_visual(&e);
}

/// vim: all open, select 3–4, `zc` → inner closes, outer stays open.
#[test]
fn visual_zc_closes_the_innermost_fold_only() {
    let mut e = boot();
    visual_layout(&mut e, false);
    select_lines(&mut e, 2, 3);
    press(&mut e, "zc");
    assert_eq!(state(&e, 2, 3), Some(true));
    assert_eq!(
        state(&e, 1, 6),
        Some(false),
        "the enclosing fold stays open"
    );
    assert_left_visual(&e);
}

/// vim: all open, select 3–4, `zC` → inner closes AND the enclosing outer 2–7,
/// although it's only partly selected.
#[test]
fn visual_z_upper_c_closes_the_fold_enclosing_the_selection() {
    let mut e = boot();
    visual_layout(&mut e, false);
    select_lines(&mut e, 2, 3);
    press(&mut e, "zC");
    assert_eq!(state(&e, 2, 3), Some(true));
    assert_eq!(state(&e, 1, 6), Some(true), "zC reaches the enclosing fold");
    assert_eq!(state(&e, 9, 10), Some(false));
    assert_left_visual(&e);
}

/// vim: select 3–4, `zd` → inner deleted, outer kept.
#[test]
fn visual_zd_deletes_the_inner_fold_and_keeps_the_outer() {
    let mut e = boot();
    visual_layout(&mut e, false);
    select_lines(&mut e, 2, 3);
    press(&mut e, "zd");
    assert_eq!(state(&e, 2, 3), None);
    assert_eq!(state(&e, 1, 6), Some(false));
    assert_left_visual(&e);
}

/// vim: select 3–4, `zD` → inner deleted, and the ENCLOSING outer kept, unlike
/// `zC`, which closes it.
#[test]
fn visual_z_upper_d_keeps_the_enclosing_fold() {
    let mut e = boot();
    visual_layout(&mut e, false);
    select_lines(&mut e, 2, 3);
    press(&mut e, "zD");
    assert_eq!(state(&e, 2, 3), None);
    assert_eq!(
        state(&e, 1, 6),
        Some(false),
        "zD does not reach enclosing folds"
    );
    assert_eq!(state(&e, 9, 10), Some(false));
    assert_left_visual(&e);
}

/// vim: `za` has no Visual form. Over 3–4 with the cursor on 4 it toggles at
/// the cursor, one level, and Visual STAYS, in all three fold states.
#[test]
fn visual_za_acts_at_the_cursor_and_keeps_visual() {
    // All open: the inner fold at the cursor closes.
    let mut e = boot();
    visual_layout(&mut e, false);
    select_lines(&mut e, 2, 3);
    press(&mut e, "za");
    assert_eq!(state(&e, 2, 3), Some(true));
    assert_eq!(state(&e, 1, 6), Some(false));
    assert!(matches!(e.modal, ModalState::Visual(_)), "za keeps Visual");

    // Inner closed: it opens.
    let mut e = boot();
    visual_layout(&mut e, false);
    e.folds[1].closed = true;
    select_lines(&mut e, 2, 3);
    press(&mut e, "za");
    assert_eq!(state(&e, 2, 3), Some(false));
    assert!(matches!(e.modal, ModalState::Visual(_)));

    // Outer and inner closed: the outer opens, the inner stays closed.
    let mut e = boot();
    visual_layout(&mut e, false);
    e.folds[0].closed = true;
    e.folds[1].closed = true;
    select_lines(&mut e, 2, 3);
    press(&mut e, "za");
    assert_eq!(state(&e, 1, 6), Some(false));
    assert_eq!(state(&e, 2, 3), Some(true));
    assert!(matches!(e.modal, ModalState::Visual(_)));
}

// ── Normal: the recursive commands ────────────────────────────────────────

/// vim: all closed, cursor on 3 (mid and outer, not inner 4–5), `zO` → outer,
/// mid AND inner open; 11–12 stays closed. The help says folds not containing
/// the cursor are unchanged; vim opens the nested inner anyway.
#[test]
fn normal_z_upper_o_opens_folds_nested_below_the_cursor() {
    let mut e = boot();
    nested_layout(&mut e, true);
    e.cursor.line = 2;
    press(&mut e, "zO");
    assert_eq!(state(&e, 1, 8), Some(false));
    assert_eq!(state(&e, 2, 5), Some(false));
    assert_eq!(
        state(&e, 3, 4),
        Some(false),
        "the nested inner fold opens too"
    );
    assert_eq!(state(&e, 10, 11), Some(true));
}

/// vim: all open, cursor on 3, `zC` → outer and mid close, inner 4–5 stays
/// open. Not the mirror of `zO`.
#[test]
fn normal_z_upper_c_closes_only_folds_containing_the_cursor() {
    let mut e = boot();
    nested_layout(&mut e, false);
    e.cursor.line = 2;
    press(&mut e, "zC");
    assert_eq!(state(&e, 1, 8), Some(true));
    assert_eq!(state(&e, 2, 5), Some(true));
    assert_eq!(
        state(&e, 3, 4),
        Some(false),
        "a fold below the cursor stays open"
    );
}

/// vim: cursor on 3, `zD` → mid and its nested inner deleted, outer kept.
/// Cursor on 4 → inner only, the same as `zd`.
#[test]
fn normal_z_upper_d_deletes_the_innermost_fold_and_what_it_contains() {
    let mut e = boot();
    nested_layout(&mut e, false);
    e.cursor.line = 2;
    press(&mut e, "zD");
    assert_eq!(state(&e, 2, 5), None);
    assert_eq!(state(&e, 3, 4), None);
    assert_eq!(state(&e, 1, 8), Some(false));
    assert_eq!(state(&e, 10, 11), Some(false));

    let mut e = boot();
    nested_layout(&mut e, false);
    e.cursor.line = 3;
    press(&mut e, "zD");
    assert_eq!(state(&e, 3, 4), None);
    assert_eq!(state(&e, 2, 5), Some(false));
    assert_eq!(state(&e, 1, 8), Some(false));
}

// ── VM.3i: `zj` / `zk` are motions ────────────────────────────────────────

/// The vim 9.2 layout for `zj` / `zk`: folds on 4–6 and 9–10 (1-based).
fn zj_layout(editor: &mut Editor, closed: bool) {
    editor.folds = vec![fold(3, 5, closed), fold(8, 9, closed)];
}

/// vim: `zj` from 1,3 → 4,1, again → 9,1, `zk` from 12,3 → 10,1. Through real
/// chords, so this proves the host hands the motion its fold table on the
/// actor path, which a grammar test can't.
#[test]
fn zj_and_zk_move_through_real_chords() {
    let mut e = boot();
    zj_layout(&mut e, false);
    e.cursor = lattice_protocol::position::Position::new(0, 2);
    press(&mut e, "zj");
    assert_eq!((e.cursor.line, e.cursor.byte), (3, 0));

    // A second `zj`, not `2zj`: this harness's `dispatch_chord` doesn't carry
    // counts. The count form is tested over real keystrokes in lattice-ui-tui.
    press(&mut e, "zj");
    assert_eq!((e.cursor.line, e.cursor.byte), (8, 0));

    e.cursor = lattice_protocol::position::Position::new(11, 2);
    press(&mut e, "zk");
    assert_eq!((e.cursor.line, e.cursor.byte), (9, 0));
}

/// vim: "a closed fold is counted as one fold". A fold nested inside a closed
/// one isn't a stop, because it's on the closed fold's row.
#[test]
fn zj_counts_a_closed_fold_as_one() {
    let mut e = boot();
    e.folds = vec![fold(3, 7, true), fold(4, 5, true), fold(10, 11, false)];
    e.cursor = lattice_protocol::position::Position::new(0, 0);
    press(&mut e, "zj");
    assert_eq!(e.cursor.line, 3, "test premise: the closed fold's start");
    press(&mut e, "zj");
    assert_eq!(
        e.cursor.line, 10,
        "the nested fold at 4 is not a second stop"
    );
}

/// Being a motion is what makes `zj` live in Visual: the selection grows to
/// the fold edge and Visual stays.
#[test]
fn zj_in_visual_extends_the_selection() {
    let mut e = boot();
    zj_layout(&mut e, false);
    e.cursor = lattice_protocol::position::Position::new(0, 2);
    press(&mut e, "v");
    let anchor = e.visual_anchor.expect("`v` arms the anchor");
    press(&mut e, "zj");
    assert!(matches!(e.modal, ModalState::Visual(_)));
    assert_eq!(e.visual_anchor, Some(anchor));
    assert_eq!((e.cursor.line, e.cursor.byte), (3, 0));
}
