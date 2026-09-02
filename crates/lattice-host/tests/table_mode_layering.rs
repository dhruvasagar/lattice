//! TB.1 — `table-mode`'s chords reach a table, and get out of the way
//! everywhere else.
//!
//! Design: `docs/dev/architecture/table-mode.md`. Slice plan:
//! `docs/dev/operations/slice-plans/table-mode.md`.
//!
//! The unit tests in `lattice-mode` prove the table model: what a table is,
//! where the caret is in it, what a column move does. None of them prove the
//! **layering**, which is the half a user actually feels — `<Tab>` has to
//! advance a cell inside a table and still fold, indent or jump everywhere
//! else, and a `<leader>t…` chord outside a table must not fire the letter it
//! ends with.
//!
//! That second one is not hypothetical. `Effect::Declined` re-resolves the
//! chord against the layers below, and for a MULTI-KEY chord the dispatcher
//! re-runs the trailing key alone — so a declined `<leader>tK` fires a bare
//! `K`. The distinction between `Declined` (shared chord, fall through) and
//! `None` (prefixed chord, consume) is the whole of this file.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::effect::Effect;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, KeymapLayer};
use lattice_mode::{ModeId, TableMode};
use lattice_protocol::chord::KeyChord;

fn boot_with(text: &str) -> Editor {
    Editor::boot(CoreDocument::from_text(text))
}

/// Run `name`'s body over `text` with the caret at `(line, byte)`, through
/// the same `execute` the chord dispatcher calls.
///
/// The registry comes from a REAL boot, so a body that was never registered
/// fails here rather than being quietly skipped — which is the failure mode
/// an unresolvable keymap name has.
fn run(name: &str, text: &str, line: u32, byte: u32) -> Effect {
    let editor = boot_with("scratch\n");
    let registry = editor.registry.load();
    let id = registry
        .id_by_name(name)
        .unwrap_or_else(|| panic!("`{name}` must be registered at boot"));
    let mut doc = CoreDocument::from_text(text);
    lattice_grammar::execute(
        &registry,
        &mut doc,
        lattice_core::BufferId(1),
        lattice_protocol::position::Position::new(line, byte),
        lattice_grammar::CommandInvocation::of(id),
        &lattice_grammar::CancellationToken::new(),
    )
    .expect("a table action never errors — it declines or does nothing")
}

/// `Effect` carries closures and is not `PartialEq`; these two are the only
/// shapes the no-op paths return.
fn is_declined(e: &Effect) -> bool {
    matches!(e, Effect::Declined)
}
fn is_none(e: &Effect) -> bool {
    matches!(e, Effect::None)
}

const TABLE: &str = "| a | bbbb |\n|---|---|\n| cc | d |\n";

// ── Registration ────────────────────────────────────────────────────────

#[test]
fn table_mode_is_registered_at_boot() {
    assert!(
        boot_with("x\n")
            .mode_registry
            .load()
            .is_registered(TableMode::mode_id()),
        "table-mode must be registered at boot"
    );
}

/// The chords resolve on `table-mode`'s own layer. Asserting the *target*
/// catches the failure that matters: `translate_mode_keymaps` drops an entry
/// whose command name is unregistered, so a typo between the keymap and the
/// registration is a whole silent keymap.
#[test]
fn every_chord_binds_to_a_registered_action_on_the_table_layer() {
    let editor = boot_with("x\n");
    let active = [TableMode::mode_id()];
    // `<leader>` is `<Space>` by default, and expands at bind time — so the
    // trie holds `<Space>t…`. A test that looked up a literal `<leader>`
    // would pass on a build where the expansion never happened.
    let cases: &[(&[KeyChord], &str)] = &[
        (
            &[KeyChord::special(lattice_protocol::chord::SpecialKey::Tab)],
            "action:table-next-cell",
        ),
        (
            &[
                KeyChord::char(' '),
                KeyChord::char('t'),
                KeyChord::char('|'),
            ],
            "action:table-align",
        ),
        (
            &[
                KeyChord::char(' '),
                KeyChord::char('t'),
                KeyChord::char('K'),
            ],
            "action:table-row-up",
        ),
        (
            &[
                KeyChord::char(' '),
                KeyChord::char('t'),
                KeyChord::char('d'),
                KeyChord::char('c'),
            ],
            "action:table-delete-column",
        ),
    ];
    for (chords, name) in cases {
        let expected = editor.registry.load().id_by_name(name).unwrap();
        let res = editor
            .keymap
            .resolve_trace(BindingMode::Normal, chords, &active);
        let hit = res
            .hits
            .iter()
            .find(|h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == TableMode::mode_id()))
            .unwrap_or_else(|| panic!("{name}'s chord must bind on the table-mode layer"));
        assert!(hit.active, "{name} must fire when table-mode is active");
        assert_eq!(hit.command.command.command, expected, "{name} target");
    }
}

/// `table-mode` attaches to majors that HAVE pipe tables, and to no others.
/// A table mode on a Rust buffer would take `<Tab>` from completion the
/// moment a line started with `|` — and a match arm does.
#[test]
fn it_activates_only_on_majors_that_have_pipe_tables() {
    let editor = boot_with("x\n");
    let registry = editor.mode_registry.load();
    let mode = registry.get(TableMode::mode_id()).unwrap();
    match mode.activation_policy() {
        lattice_mode::ActivationPolicy::Majors(majors) => {
            assert!(majors.contains(&ModeId::new("markdown-mode")));
            assert!(
                majors.contains(&ModeId::new("org-mode")),
                "org's major is a PLUGIN mode; the policy names it by id so \
                 the host needs no knowledge of the plugin"
            );
            assert!(
                !majors.contains(&ModeId::new("rust-mode")),
                "a table mode on a code buffer takes <Tab> from completion"
            );
        }
        other => panic!("expected a Majors policy, got {other:?}"),
    }
}

// ── Inside a table ──────────────────────────────────────────────────────

/// The base case: align rewrites the whole table in ONE edit. Per-row edits
/// would let `u` undo half a column and leave a corrupt table on screen.
#[test]
fn align_rewrites_the_table_as_a_single_edit() {
    match run("action:table-align", TABLE, 0, 2) {
        Effect::ApplyEdit { edit, cursor, .. } => {
            assert_eq!(edit.range.start.line, 0);
            assert_eq!(edit.range.end.line, 2, "the whole table, not one row");
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            let widths: Vec<usize> = text.lines().map(str::len).collect();
            assert!(
                widths.windows(2).all(|w| w[0] == w[1]),
                "columns line up:\n{text}"
            );
            assert!(cursor.is_some(), "the caret is re-parked after a rewrite");
        }
        other => panic!("expected one ApplyEdit, got {other:?}"),
    }
}

/// `<Tab>` in a table advances a cell and puts the caret ON the next cell's
/// text — not at the row start, which is what a line-granular cursor would
/// give and would make the chord useless for typing.
#[test]
fn tab_advances_to_the_next_cell() {
    match run("action:table-next-cell", TABLE, 0, 2) {
        Effect::ApplyEdit { edit, cursor, .. } => {
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            let first = text.lines().next().unwrap();
            let at = cursor.expect("a cell target").byte as usize;
            assert!(
                first[at..].starts_with("bbbb"),
                "the caret lands on the second cell's text, got {:?} in {first:?}",
                &first[at..]
            );
        }
        other => panic!("expected ApplyEdit, got {other:?}"),
    }
}

/// A structural chord edits every row at once and keeps the caret with the
/// column it moved.
#[test]
fn a_column_move_rewrites_every_row() {
    match run("action:table-column-right", TABLE, 0, 2) {
        Effect::ApplyEdit { edit, .. } => {
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            let mut lines = text.lines();
            assert!(lines.next().unwrap().starts_with("| bbbb"), "{text}");
            lines.next();
            assert!(lines.next().unwrap().starts_with("| d"), "{text}");
        }
        other => panic!("expected ApplyEdit, got {other:?}"),
    }
}

/// An operation that cannot apply consumes the chord rather than declining.
/// The caret IS in a table, so falling through to a headline cycle would be a
/// surprise — the user asked to move a row that has nowhere to go.
#[test]
fn an_impossible_operation_inside_a_table_consumes_the_chord() {
    assert!(
        is_none(&run("action:table-row-up", TABLE, 0, 2)),
        "the top row cannot move up, and the caret is still in a table"
    );
}

// ── Outside a table: the layering ───────────────────────────────────────

/// `<Tab>` is SHARED — org's headline cycle and the builtin jump-forward sit
/// below it — so outside a table it declines and the chord re-resolves.
/// Consuming it here would break `<Tab>` in every org and markdown buffer.
#[test]
fn tab_declines_outside_a_table_so_the_layers_below_still_get_it() {
    for name in ["action:table-next-cell", "action:table-prev-cell"] {
        assert!(
            is_declined(&run(name, "just prose\n", 0, 0)),
            "{name} must fall through when there is no table"
        );
    }
}

/// …and the `<leader>t…` family does NOT decline, which is the opposite
/// answer for the opposite reason.
///
/// A decline re-runs a multi-key chord's TRAILING key alone. `<leader>tK`
/// declining would fire a bare `K`; `<leader>tdc` would fire `c`, the change
/// operator, which then waits for a motion. Nothing below binds these chords,
/// so consuming them is both the honest no-op and the only safe one.
#[test]
fn a_prefixed_chord_outside_a_table_consumes_rather_than_declining() {
    for name in [
        "action:table-align",
        "action:table-row-up",
        "action:table-row-down",
        "action:table-column-left",
        "action:table-column-right",
        "action:table-insert-row",
        "action:table-insert-column",
        "action:table-delete-row",
        "action:table-delete-column",
    ] {
        assert!(
            is_none(&run(name, "just prose\n", 0, 0)),
            "{name} is behind `<leader>t`; declining it would re-run its \
             trailing key alone"
        );
    }
}

/// The table has to be the one under the caret. A buffer containing a table
/// elsewhere must not have its table edited from a prose line — the chord
/// reaching for the nearest table would be an edit in a place the user is not
/// looking.
#[test]
fn a_table_elsewhere_in_the_buffer_is_not_reached() {
    assert!(is_none(&run(
        "action:table-align",
        "prose\n\n| a | b |\n",
        0,
        0
    )));
}
