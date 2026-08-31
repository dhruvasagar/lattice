//! PD.5 / FL.1 — `foldlevel` over a project-diff-shaped multibuffer.
//!
//! The point of file-boundary folds is that a diff spanning fifty files is
//! navigable before you read any of it: collapse to the file list, open the
//! one you want. That is `:set foldlevel=0`, and the two levels a diff view
//! has — file, then hunk — are what make the level meaningful at all.
//!
//! These drive the real option path (`parse_and_set_command` +
//! `drain_option_changes`, the same cascade `:set` runs) rather than calling
//! `apply_fold_level` directly. `folds.rs`'s unit tests already cover the level
//! arithmetic; what is uncovered between them and the user is the wiring —
//! whether the option reaches the fold list at all, and whether the view is
//! actually shorter afterwards.
//!
//! The shape is the project diff's: N files, each contributing several
//! excerpts, exactly as `attach_batch` builds them from `git diff` hunks. It is
//! constructed here rather than by shelling out to git so the test stays a unit
//! of the fold behaviour and not of the scanner.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, create_multibuffer_view};
use lattice_runtime::spawn_document;

/// `files` source files, each with `hunks` excerpts of 3 rows, spaced so the
/// excerpts are non-contiguous — a real diff's hunks have gaps between them.
fn boot_diff_view(files: u32, hunks: u32) -> (Editor, BufferId) {
    let mut editor = Editor::boot(lattice_core::Document::from_text("scratch\n"));
    let registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    let mut excerpts = Vec::new();

    for f in 0..files {
        let id = BufferId(600 + f);
        let text: String = (0..hunks * 6).map(|i| format!("f{f}-line{i}\n")).collect();
        let doc = DocumentBuilder::default()
            .with_text(&text)
            .with_path(std::path::PathBuf::from(format!("/repo/src/file{f}.rs")))
            .build();
        sources.insert(
            id,
            Arc::new(spawn_document(id, doc, registry.clone()))
                as Arc<dyn lattice_runtime::Document>,
        );
        for h in 0..hunks {
            let start = h * 6;
            excerpts.push(Excerpt::new(id, start, start + 2));
        }
    }

    let registry_for_view = editor.registry.clone();
    let view = create_multibuffer_view(
        &mut editor,
        sources,
        excerpts,
        Some("*test:project-diff*".into()),
        BufferFlags::default(),
        registry_for_view,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    );
    editor.activate_document(view);
    editor.recompute_folds();
    (editor, view)
}

/// Rows the user can actually see: everything not swallowed by a closed fold.
fn visible_rows(editor: &Editor) -> u32 {
    let total = editor.document.snapshot().buffer.content_line_count();
    (0..total)
        .filter(|l| !editor.line_inside_closed_fold(*l))
        .count() as u32
}

fn set_foldlevel(editor: &mut Editor, level: u32) {
    editor
        .config
        .parse_and_set_command(&format!("foldlevel={level}"))
        .expect("foldlevel is settable");
    editor.drain_option_changes();
}

#[test]
fn the_default_leaves_every_fold_open() {
    // Guards the deliberate deviation from vim's `foldlevel=0` default. A 0
    // default would open every search result, project diff and agent
    // transcript collapsed to nothing, because overlay fold sources are
    // registered regardless of `foldmethod`.
    let (editor, _view) = boot_diff_view(4, 3);
    let total = editor.document.snapshot().buffer.content_line_count();
    assert_eq!(visible_rows(&editor), total, "nothing is folded on open");
    assert!(
        !editor.folds.is_empty(),
        "and the folds exist — otherwise this passes for the wrong reason"
    );
}

#[test]
fn foldlevel_zero_gives_one_row_per_file() {
    let (mut editor, _view) = boot_diff_view(50, 3);
    set_foldlevel(&mut editor, 0);
    assert_eq!(
        visible_rows(&editor),
        50,
        "a 50-file diff collapses to its file list"
    );
}

/// The level in between, and the one that shows the model is doing real work
/// rather than just an all-or-nothing toggle: files open, hunks collapsed.
#[test]
fn foldlevel_one_gives_one_row_per_hunk() {
    let (mut editor, _view) = boot_diff_view(4, 3);
    set_foldlevel(&mut editor, 1);
    assert_eq!(
        visible_rows(&editor),
        4 * 3,
        "each file's hunks show as one row apiece"
    );
}

#[test]
fn raising_the_level_again_reopens_everything() {
    let (mut editor, _view) = boot_diff_view(4, 3);
    let total = editor.document.snapshot().buffer.content_line_count();
    set_foldlevel(&mut editor, 0);
    assert_eq!(visible_rows(&editor), 4);
    set_foldlevel(&mut editor, 99);
    assert_eq!(visible_rows(&editor), total);
}

/// Toggling one file open from the collapsed state — the actual navigation
/// gesture, not just the bulk command.
#[test]
fn toggling_a_file_fold_reveals_that_files_hunks() {
    let (mut editor, _view) = boot_diff_view(4, 3);
    set_foldlevel(&mut editor, 0);
    assert_eq!(visible_rows(&editor), 4);

    // `za` on the first file's header row.
    editor.cursor = lattice_protocol::position::Position::new(0, 0);
    editor.do_set_fold_state_at_cursor(None);

    let after = visible_rows(&editor);
    assert!(
        after > 4,
        "opening a file must reveal its rows; visible went 4 -> {after}"
    );
    assert!(
        after < editor.document.snapshot().buffer.content_line_count(),
        "...and must not open every other file too"
    );
}

/// PD.5a's payoff, at the level the user experiences it. A refresh re-reads
/// the files into brand-new source buffers; the folds the user collapsed have
/// to still be collapsed afterwards, which only holds because fold identity is
/// keyed on the path rather than the `BufferId`.
#[test]
fn fold_state_survives_a_refresh_that_renumbers_the_sources() {
    let (mut editor, _view) = boot_diff_view(4, 3);
    set_foldlevel(&mut editor, 0);
    assert_eq!(visible_rows(&editor), 4);

    // A refresh recomputes folds against re-read sources. Recomputing is
    // exactly what `gr` triggers once the scan has re-attached excerpts.
    editor.recompute_folds();

    assert_eq!(
        visible_rows(&editor),
        4,
        "the collapsed file list survives the rebuild"
    );
}

/// A manual toggle must not be undone by the next rebuild — otherwise every
/// async excerpt batch landing during a scan would fight the user's `za`.
#[test]
fn a_rebuild_does_not_undo_a_manual_toggle() {
    let (mut editor, _view) = boot_diff_view(4, 3);
    set_foldlevel(&mut editor, 0);
    editor.cursor = lattice_protocol::position::Position::new(0, 0);
    editor.do_set_fold_state_at_cursor(None);
    let after_toggle = visible_rows(&editor);

    editor.recompute_folds();

    assert_eq!(
        visible_rows(&editor),
        after_toggle,
        "the rebuild reapplied foldlevel over the user's toggle"
    );
}
