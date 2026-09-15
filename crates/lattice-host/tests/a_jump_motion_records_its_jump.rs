//! VM.3a / VM.3b: `MotionSpec::jump` decides what a jump is, and `%` is a
//! motion.
//!
//! ## VM.3a
//!
//! `jump` had been declared on every motion since the grammar's first slice
//! and read by nobody. `run_document_invocation` decided what counted with
//!
//! ```ignore
//! if inv.command == self.builtins.goto_first_line.0
//!     || inv.command == self.builtins.goto_last_line.0
//! ```
//!
//! so `gg` and `G` were jumps and `}`, `{`, `(`, `)`, the sixteen tree-sitter
//! structural motions and every plugin motion were not — all of them setting
//! `jump: true` into a field that went nowhere. Org's headline motions say so
//! in their own source: a headline jump "is somewhere you want `<C-o>` to
//! bring you back from". It was not.
//!
//! ## VM.3b
//!
//! `%` was `action:match-bracket`. Actions do not compose with operators and
//! VM.1's derivation only mirrors motions into Visual, so `d%`, `y%` and `v%`
//! were all silently unbound — in vim every one of them works, and `%` is
//! listed under motions, not under commands.
//!
//! ## Why the operator half is not tested here
//!
//! `Editor::dispatch_chord` does not compose operator+motion: the absorb
//! effect pushes the operator's prefix into `Editor::partial_chord`, while
//! `dispatch_chord` resolves against the `&mut Vec` its CALLER threads, and
//! the two are different vectors. So `d%` driven through this harness silently
//! fires a bare `%` — which is exactly what it did while this file was being
//! written, and `edit.rs` already carries the warning in prose. `d%` and `y%`
//! are covered in `lattice-ui-tui`'s `motion_composition` module, over
//! `press_chars`, which is the real keystroke path.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;
use lattice_host::keymap_trie::{KeymapLayer, LookupResult};
use lattice_keymap::BindingMode;
use lattice_protocol::ChordPattern;

fn press(editor: &mut Editor, chords: &[KeyChord]) {
    let mut partial = Vec::new();
    for c in chords {
        let _ = editor.dispatch_chord(*c, &mut partial);
    }
}

fn ch(c: char) -> KeyChord {
    KeyChord::char(c)
}

// ── VM.3a: what counts as a jump ──────────────────────────────────────────

/// `}` declares `jump: true`, so it records where the user was.
///
/// The assertion is on `position_history` rather than on a round-trip through
/// `<C-o>`, because the ring is what the old hardcoded pair wrote to and the
/// ring is where the gap was.
#[test]
fn a_paragraph_motion_records_its_jump() {
    let mut editor = Editor::boot(CoreDocument::from_text(
        "alpha\nbeta\n\ngamma\ndelta\n\nepsilon\n",
    ));
    editor.cursor.line = 0;
    let before = editor.position_history.len();

    press(&mut editor, &[ch('}')]);

    assert!(
        editor.cursor.line > 0,
        "test premise: `}}` must have moved the cursor"
    );
    assert_eq!(
        editor.position_history.len(),
        before + 1,
        "`}}` declares jump: true and must push a position-history entry"
    );
    assert_eq!(
        editor.position_history.last().unwrap().position.line,
        0,
        "the entry holds where the user WAS, not where the motion landed"
    );
}

/// The negative half of the property: an ordinary motion declares
/// `jump: false` and must stay out of the ring, or `<C-o>` degenerates into
/// an undo of every keystroke.
#[test]
fn an_ordinary_motion_records_nothing() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha beta gamma delta\n"));
    let before = editor.position_history.len();

    press(&mut editor, &[ch('w'), ch('w')]);

    assert!(editor.cursor.byte > 0, "test premise: `w` must have moved");
    assert_eq!(
        editor.position_history.len(),
        before,
        "`w` is not a jump and must not push"
    );
}

// ── VM.3b: `%` is a motion ────────────────────────────────────────────────

#[test]
fn percent_jumps_to_the_matching_bracket() {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main(a, b) {}\n"));
    editor.cursor.line = 0;
    editor.cursor.byte = 0;

    press(&mut editor, &[ch('%')]);

    // First bracket at or after the cursor is `(` at byte 7; its partner is
    // `)` at byte 12.
    assert_eq!(editor.cursor.byte, 12, "`%` must land on the partner");
}

/// Nesting is counted, not pattern-matched.
#[test]
fn percent_counts_nesting() {
    let mut editor = Editor::boot(CoreDocument::from_text("(a(b)c)\n"));
    editor.cursor.byte = 0;

    press(&mut editor, &[ch('%')]);

    assert_eq!(
        editor.cursor.byte, 6,
        "the partner of the outer `(` is the LAST `)`, not the first"
    );
}

/// And it extends a selection, via VM.1's derivation rather than any
/// `%`-specific Visual wiring.
#[test]
fn v_percent_extends_the_selection_to_the_match() {
    let mut editor = Editor::boot(CoreDocument::from_text("x(abc)y\n"));
    editor.cursor.byte = 1;

    press(&mut editor, &[ch('v'), ch('%')]);

    let region = editor
        .visual_selection_range()
        .expect("`v` then `%` leaves Visual live");
    assert_eq!(region.start.byte, 1);
    assert_eq!(
        region.end.byte, 6,
        "the extent is HALF-OPEN, so reaching the `)` at byte 5 reads as 6"
    );
}

/// `%` on a line with no bracket does not go hunting down the buffer — vim
/// stops at the newline. Returning the cursor unmoved rather than an error
/// matters for `d%`: a failed `%` must not delete something surprising.
#[test]
fn percent_does_not_leave_the_line_looking_for_a_bracket() {
    let mut editor = Editor::boot(CoreDocument::from_text("no brackets here\n(later)\n"));
    editor.cursor.line = 0;
    editor.cursor.byte = 0;

    press(&mut editor, &[ch('%')]);

    assert_eq!(editor.cursor.line, 0);
    assert_eq!(editor.cursor.byte, 0);
}

/// Being a motion is what puts it in the Visual and Select tries at all — the
/// binding is written by `expand_grammar_rows`, not by `keymap_visual`.
#[test]
fn percent_is_a_motion_in_every_mode_that_takes_one() {
    let editor = Editor::boot(CoreDocument::from_text("(x)\n"));
    let commands = editor.registry.load();

    let bound = editor
        .keymap
        .layer_bindings(KeymapLayer::Builtin, BindingMode::Normal)
        .into_iter()
        .find(|(p, _)| p.as_slice() == [ChordPattern::Literal(ch('%'))])
        .map(|(_, b)| b)
        .expect("`%` is bound in Normal");
    let spec = commands
        .lookup(bound.command.command)
        .expect("`%` resolves");
    assert!(
        matches!(spec.kind, lattice_grammar::CommandKind::Motion),
        "`%` must be a Motion, got {:?} ({})",
        spec.kind,
        spec.name
    );

    for mode in [BindingMode::Visual, BindingMode::Select] {
        assert!(
            matches!(
                editor.keymap.lookup(mode, &[ch('%')]),
                LookupResult::Bound { .. }
            ),
            "`%` must be Bound in {mode:?}"
        );
    }
}
