//! Moving the cursor out of a closed fold ends even when two closed folds
//! overlap without nesting.
//!
//! Two fold providers can each be right and still produce such a pair. A diff
//! session folds the unchanged stretches of a file, and org folds each heading
//! to the end of its subtree; in a conflicted org file opened ours | working |
//! theirs, a heading that starts inside an unchanged stretch and runs to the end
//! of the file overlaps that stretch's fold. The snap used to hop past the diff
//! fold, land inside the heading's fold, hop back up to its head (inside the
//! diff fold), and repeat forever, freezing the editor on the first `j`.
//!
//! Each test drives the editor on its own thread and waits with a timeout, so
//! a regression fails here rather than hanging the suite.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::mpsc;
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_core::Fold;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

/// Ten lines, so the last addressable line is 9.
fn boot() -> Editor {
    let text: String = (1..=10).map(|n| format!("line {n}\n")).collect();
    Editor::boot(CoreDocument::from_text(&text))
}

fn closed(start_line: u32, end_line: u32) -> Fold {
    Fold {
        start_line,
        end_line,
        closed: true,
        identity: None,
    }
}

/// `1..=5` and `3..=9`: the second starts inside the first and runs past it to
/// the end of the file. Visible rows are 0 and 1 only.
fn overlapping() -> Vec<Fold> {
    vec![closed(1, 5), closed(3, 9)]
}

/// Run `f` on a fresh editor carrying `folds`, and return the cursor line, or
/// fail if it does not come back.
fn cursor_after(folds: Vec<Fold>, start: u32, f: fn(&mut Editor)) -> u32 {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut editor = boot();
        editor.folds = folds;
        editor.cursor.line = start;
        editor.cursor.byte = 0;
        f(&mut editor);
        let _ = tx.send(editor.cursor.line);
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("the cursor walk did not end")
}

#[test]
fn j_onto_overlapping_folds_ends_on_a_visible_line() {
    // From line 1, `j` walks past both folds to the end of the file, and the
    // snap has nowhere visible below to go.
    let line = cursor_after(overlapping(), 1, |e| {
        let mut partial = Vec::new();
        let _ = e.dispatch_chord(KeyChord::char('j'), &mut partial);
    });
    assert_eq!(line, 1, "nothing is visible below, so the cursor stays");
}

#[test]
fn going_down_into_overlapping_folds_lands_on_the_outer_head() {
    let line = cursor_after(overlapping(), 0, |e| {
        e.cursor.line = 4;
        e.snap_cursor_past_closed_folds(0);
    });
    assert_eq!(line, 1, "the head of the fold that hides line 3");
}

#[test]
fn going_up_into_overlapping_folds_lands_on_the_outer_head() {
    let line = cursor_after(overlapping(), 9, |e| {
        e.cursor.line = 7;
        e.snap_cursor_past_closed_folds(9);
    });
    assert_eq!(line, 1);
}

#[test]
fn going_down_past_a_fold_lands_below_it() {
    // The ordinary case, unchanged: a fold with room below it is stepped over.
    let line = cursor_after(vec![closed(2, 5)], 2, |e| {
        e.cursor.line = 3;
        e.snap_cursor_past_closed_folds(2);
    });
    assert_eq!(line, 6);
}
