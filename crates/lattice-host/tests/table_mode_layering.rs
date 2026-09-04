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
    run_in(name, text, line, byte, None)
}

/// [`run`], for a document backed by `path` — which is what `<leader>t-`
/// reads to pick a rule style when the table has none to copy.
fn run_in(name: &str, text: &str, line: u32, byte: u32, path: Option<&str>) -> Effect {
    let editor = boot_with("scratch\n");
    let registry = editor.registry.load();
    let id = registry
        .id_by_name(name)
        .unwrap_or_else(|| panic!("`{name}` must be registered at boot"));
    let mut doc = match path {
        Some(p) => lattice_core::DocumentBuilder::default()
            .with_path(p)
            .with_text(text)
            .build(),
        None => CoreDocument::from_text(text),
    };
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

/// **The replaced range is the OLD table's span**, whatever the new one is.
///
/// Deriving the end line from the rendered result instead agrees with this
/// for align and the swaps — same row count — and then silently breaks the
/// two operations that change the count: an insert addresses a line past the
/// buffer's end and does nothing, a delete leaves its last row behind. That
/// shipped, and it was the org plugin's integration suite that caught it,
/// because every unit test here asserted the rendered TEXT and none asserted
/// the range it lands in.
#[test]
fn the_replaced_range_is_the_old_table_not_the_new_one() {
    let one_row = "| a | b |\n";
    match run("action:table-insert-row", one_row, 0, 2) {
        Effect::ApplyEdit { edit, .. } => {
            assert_eq!(
                (edit.range.start.line, edit.range.end.line),
                (0, 0),
                "the table occupies line 0 alone; the replacement is two lines \
                 long but it still REPLACES one"
            );
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            assert_eq!(
                text.lines().count(),
                2,
                "…and inserts the new row: {text:?}"
            );
        }
        other => panic!("expected ApplyEdit, got {other:?}"),
    }
    // The same in the other direction: a delete shrinks the text but still
    // spans every line the table had.
    match run("action:table-delete-row", "| a |\n| b |\n", 0, 2) {
        Effect::ApplyEdit { edit, .. } => {
            assert_eq!((edit.range.start.line, edit.range.end.line), (0, 1));
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            assert_eq!(text.lines().count(), 1, "one row left: {text:?}");
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

// ── TB.3 ────────────────────────────────────────────────────────────────

fn replaced(e: &Effect) -> String {
    match e {
        Effect::ApplyEdit { edit, .. } => {
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            text.clone()
        }
        other => panic!("expected ApplyEdit, got {other:?}"),
    }
}

/// Emacs' behaviour, and how people actually build a table: type a row, Tab,
/// type the next. `<Tab>` at the last cell has to ADD a row rather than
/// declining — declining there would hand the key to org's headline cycle
/// from inside a table, which is the wrong answer twice over.
#[test]
fn tab_at_the_last_cell_adds_a_row() {
    let out = replaced(&run("action:table-next-cell", "| a | b |\n", 0, 6));
    assert_eq!(out.lines().count(), 2, "a row was added: {out:?}");
    assert_eq!(
        out.lines().nth(1).unwrap().matches('|').count(),
        3,
        "and it is as wide as the table: {out:?}"
    );
}

/// The one place the dialect is not in the buffer: a table with no rule to
/// copy. The file's extension answers, because the thing that decides
/// whether `|---+---|` is idiomatic IS whether this is an org file.
#[test]
fn a_rule_inserted_into_a_ruleless_table_follows_the_file_type() {
    let org = replaced(&run_in(
        "action:table-insert-rule",
        "| a | b |\n",
        0,
        2,
        Some("/tmp/notes.org"),
    ));
    assert!(
        org.lines().any(|l| l.contains('+')),
        "org gets `+`: {org:?}"
    );

    let md = replaced(&run_in(
        "action:table-insert-rule",
        "| a | b |\n",
        0,
        2,
        Some("/tmp/notes.md"),
    ));
    assert!(
        md.lines()
            .filter(|l| l.contains("---"))
            .all(|l| !l.contains('+')),
        "markdown gets pipes: {md:?}"
    );
}

/// …and when the table HAS a rule, that wins over the file type. Otherwise a
/// `+`-joined table pasted into a `.md` file would grow a mismatched second
/// rule, which is the disagreement design §2 exists to prevent.
#[test]
fn an_existing_rule_beats_the_file_type() {
    let out = replaced(&run_in(
        "action:table-insert-rule",
        "| a | b |\n|--+--|\n| c | d |\n",
        2,
        2,
        Some("/tmp/notes.md"),
    ));
    let rules: Vec<&str> = out.lines().filter(|l| l.contains("--")).collect();
    assert_eq!(rules.len(), 2, "{out:?}");
    assert!(
        rules.iter().all(|l| l.contains('+')),
        "the table's own style won: {out:?}"
    );
}

/// Numbers sort as numbers. `10` after `9` is the single most noticeable way
/// a sort can be wrong, and it is what lexicographic order gets backwards.
#[test]
fn sort_picks_its_comparator_from_the_data() {
    let out = replaced(&run("action:table-sort", "| 9 |\n| 10 |\n| 2 |\n", 0, 2));
    let values: Vec<String> = out
        .lines()
        .map(|l| l.trim_matches(['|', ' ']).to_string())
        .collect();
    assert_eq!(values, vec!["2", "9", "10"], "{out:?}");
}

/// A sort must not drag the header row into the body.
#[test]
fn sort_stays_inside_the_section_the_cursor_is_in() {
    let out = replaced(&run(
        "action:table-sort",
        "| header |\n|---|\n| b |\n| a |\n",
        2,
        2,
    ));
    assert!(
        out.lines().next().unwrap().contains("header"),
        "the header stayed above the rule: {out:?}"
    );
}

#[test]
fn copy_down_fills_a_series() {
    let out = replaced(&run("action:table-copy-down", "| Q3 |\n| |\n", 0, 2));
    assert!(out.lines().nth(1).unwrap().contains("Q4"), "{out:?}");
}

#[test]
fn transpose_swaps_the_axes() {
    let out = replaced(&run(
        "action:table-transpose",
        "| a | b | c |\n| 1 | 2 | 3 |\n",
        0,
        2,
    ));
    assert_eq!(out.lines().count(), 3, "{out:?}");
}

/// Every TB.3 chord is behind `<leader>t`, so like its TB.1 peers it
/// CONSUMES outside a table rather than declining — a decline would re-run
/// its trailing key alone, and `<leader>tS` would fire `S` (substitute-line).
#[test]
fn the_new_chords_also_consume_outside_a_table() {
    for name in [
        "action:table-insert-rule",
        "action:table-sort",
        "action:table-sort-descending",
        "action:table-blank-cell",
        "action:table-copy-down",
        "action:table-transpose",
    ] {
        assert!(
            is_none(&run(name, "just prose\n", 0, 0)),
            "{name} must consume, not decline"
        );
    }
}

// ── TB.4: the modal split ───────────────────────────────────────────────

/// The same key, resolved per modal state. `<Tab>` is bound in BOTH Normal
/// and Insert on this mode's layer, and to the same body — moving to a cell
/// and realigning is one operation, and it emits no `Effect::EnterMode`, so
/// Insert is simply kept.
#[test]
fn tab_is_bound_in_both_normal_and_insert() {
    let editor = boot_with("x\n");
    let active = [TableMode::mode_id()];
    let expected = editor
        .registry
        .load()
        .id_by_name("action:table-next-cell")
        .unwrap();
    let tab = [KeyChord::special(lattice_protocol::chord::SpecialKey::Tab)];
    for mode in [BindingMode::Normal, BindingMode::Insert] {
        let res = editor.keymap.resolve_trace(mode, &tab, &active);
        let hit = res
            .hits
            .iter()
            .find(|h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == TableMode::mode_id()))
            .unwrap_or_else(|| panic!("`<Tab>` must bind on the table layer in {mode:?}"));
        assert!(hit.active, "{mode:?}");
        assert_eq!(hit.command.command.command, expected, "{mode:?}");
    }
}

/// `<CR>` is bound in Insert ONLY, and the asymmetry is the design. In Normal
/// it already means something a table row wants — first non-blank of the next
/// line — where in Insert it means "split this line", which inside a table row
/// is never what you meant.
#[test]
fn cr_is_bound_in_insert_only() {
    let editor = boot_with("x\n");
    let active = [TableMode::mode_id()];
    let cr = [KeyChord::special(
        lattice_protocol::chord::SpecialKey::Enter,
    )];
    let on_layer = |mode| {
        editor
            .keymap
            .resolve_trace(mode, &cr, &active)
            .hits
            .iter()
            .any(|h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == TableMode::mode_id()))
    };
    assert!(
        on_layer(BindingMode::Insert),
        "Insert `<CR>` is the table's"
    );
    assert!(
        !on_layer(BindingMode::Normal),
        "Normal `<CR>` is NOT — it already does what a table row wants"
    );
}

/// `<Esc>` realigns and then falls through, so the mode never hardcodes
/// exit-insert. Without `fall_through` this binding would trap the user in
/// Insert mode inside every table, which is about the worst outcome available.
#[test]
fn esc_realigns_and_falls_through_to_the_native_exit() {
    let editor = boot_with("x\n");
    let active = [TableMode::mode_id()];
    let esc = [KeyChord::special(lattice_protocol::chord::SpecialKey::Esc)];
    let res = editor
        .keymap
        .resolve_trace(BindingMode::Insert, &esc, &active);
    let hit = res
        .hits
        .iter()
        .find(|h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == TableMode::mode_id()))
        .expect("`<Esc>` binds on the table layer in Insert");
    assert!(
        hit.command.fall_through,
        "`<Esc>` MUST augment-and-continue, or the user cannot leave Insert"
    );
}

/// `<CR>` in the last row adds one, which is what makes it the way you enter
/// a table's contents — emacs' `org-table-next-row` does the same.
#[test]
fn cr_at_the_bottom_adds_a_row() {
    let out = replaced(&run("action:table-next-row", "| a | b |\n", 0, 2));
    assert_eq!(out.lines().count(), 2, "{out:?}");
}

/// …and it keeps the COLUMN, where `<Tab>` wraps to the start of the next
/// row. That is the difference between filling a column and filling a row.
#[test]
fn cr_keeps_the_column_where_tab_wraps() {
    match run("action:table-next-row", "| a | b |\n| c | d |\n", 0, 6) {
        Effect::ApplyEdit { cursor, edit, .. } => {
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            let at = cursor.expect("a target").byte as usize;
            let line = text.lines().nth(1).unwrap();
            assert!(
                line[at..].starts_with('d'),
                "landed in the second column: {:?} of {line:?}",
                &line[at..]
            );
        }
        // An already-aligned table needs no edit; the cursor still moves.
        Effect::CursorMove(p) => assert_eq!(p.line, 1),
        other => panic!("expected ApplyEdit or CursorMove, got {other:?}"),
    }
}

/// `<CR>` and `<Esc>` are SHARED with the native newline and exit-insert, so
/// they decline outside a table. Consuming either would break it in every
/// markdown and org buffer — a far larger surface than this mode.
#[test]
fn the_insert_chords_decline_outside_a_table() {
    for name in ["action:table-next-row", "action:table-realign"] {
        assert!(
            is_declined(&run(name, "just prose\n", 0, 0)),
            "{name} must fall through to its native meaning"
        );
    }
}

/// **No edit when nothing changed.** Walking an already-aligned table would
/// otherwise push an undo entry per cell, and `<Esc>` one every time you left
/// Insert in a table — undo steps for edits that changed nothing, which makes
/// `u` stop meaning "undo my last change".
#[test]
fn an_aligned_table_moves_the_caret_without_editing() {
    let aligned = "| a  | bb |\n| cc | d  |\n";
    match run("action:table-next-cell", aligned, 0, 2) {
        Effect::CursorMove(_) => {}
        other => panic!("expected a cursor-only effect, got {other:?}"),
    }
    match run("action:table-realign", aligned, 0, 2) {
        Effect::CursorMove(_) => {}
        other => panic!("realigning an aligned table must not edit, got {other:?}"),
    }
}

/// **The completion popup wins `<Tab>` while it is up.**
///
/// Binding `<Tab>` in Insert puts this mode in the same chord as
/// completion-accept and snippet-placeholder jump. The design says that is
/// safe because minor layers overlay in ACTIVATION order and both of those
/// activate later than `table-mode` — the popup when it opens, the snippet
/// session when it starts. This is that claim as a test rather than as
/// reasoning: it is exactly the kind of ordering assumption that is true
/// until someone changes an activation site.
///
/// Goes through `lookup_with_context`, which is what dispatch calls and the
/// only thing that RANKS the layers. `resolve_trace` lists every hit in
/// enumeration order for `:describe-key` and says nothing about which wins —
/// asserting against it looked like a failure here and was a wrong test.
#[test]
fn the_completion_popup_shadows_the_table_tab_while_it_is_up() {
    let editor = boot_with("x\n");
    let popup = lattice_host::keymap_insert::completion_popup_mode_id();
    editor.keymap.push_layer(
        lattice_host::keymap_registry::PushLayerKind::MinorMode(popup),
        "completion-popup",
        lattice_host::keymap_insert::completion_popup_layer_bindings(&editor.action_ids),
    );
    let tab = [KeyChord::special(lattice_protocol::chord::SpecialKey::Tab)];

    // Activation order: the table attached when the buffer opened, the popup
    // when it opened — so the popup is LAST and overlays.
    match editor.keymap.lookup_with_context(
        BindingMode::Insert,
        &tab,
        &[TableMode::mode_id(), popup],
    ) {
        lattice_host::keymap_trie::LookupResult::Bound { command, .. } => {
            assert_eq!(
                command.layer,
                KeymapLayer::MinorMode(popup),
                "the popup must win `<Tab>` while it is up, or accepting a \
                 completion inside a table jumps a cell instead"
            );
        }
        other => panic!("expected Bound, got {other:?}"),
    }

    // …and with the popup down, the table gets it back.
    match editor
        .keymap
        .lookup_with_context(BindingMode::Insert, &tab, &[TableMode::mode_id()])
    {
        lattice_host::keymap_trie::LookupResult::Bound { command, .. } => {
            assert_eq!(command.layer, KeymapLayer::MinorMode(TableMode::mode_id()));
        }
        other => panic!("expected Bound, got {other:?}"),
    }
}

// ── OE.4: `C-c C-c` on a table ──────────────────────────────────────────

/// Emacs' `C-c C-c` realigns the table at the cursor, and `table-mode` is
/// where that arm belongs: it owns pipe tables for markdown AND org, and org
/// cannot reach `action:table-align` at all — a guest cannot invoke a
/// registered command (`org-mode.md` §5.4).
#[test]
fn ctrl_c_ctrl_c_realigns_the_table_at_the_cursor() {
    match run("action:table-realign", TABLE, 0, 2) {
        Effect::ApplyEdit { edit, .. } => {
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;
            let widths: Vec<usize> = text.lines().map(str::len).collect();
            assert!(
                widths.windows(2).all(|w| w[0] == w[1]),
                "columns line up:\n{text}"
            );
        }
        other => panic!("expected one ApplyEdit, got {other:?}"),
    }
}

/// …and OUTSIDE a table it DECLINES, which is the whole reason the chord is
/// bound to `REALIGN` and not to `ALIGN`.
///
/// `table-mode` is active on the entire buffer, not only inside tables. A
/// consuming action here would swallow `C-c C-c` everywhere and leave org's
/// dispatcher — the layer below — dead. `Effect::Declined` is what makes the
/// two compose; the org-side half of this pair lives in the plugin's
/// `outside_a_table_the_chord_reaches_orgs_dispatcher`.
#[test]
fn outside_a_table_ctrl_c_ctrl_c_declines_so_the_layer_below_gets_it() {
    assert!(
        matches!(
            run("action:table-realign", "not a table at all\n", 0, 0),
            Effect::Declined
        ),
        "declining is what lets org's `C-c C-c` arms exist at all"
    );
}
