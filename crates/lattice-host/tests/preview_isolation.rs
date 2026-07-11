//! Preview isolation (PI series) — host integration tests.
//!
//! Design: `docs/dev/architecture/preview-isolation.md`.
//! Slice plan: `docs/dev/operations/slice-plans/preview-isolation.md`.

use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use lattice_host::editor::Editor;
use lattice_host::preview::{PreviewMode, PreviewOverride};
use lattice_mode::ModeId;
use lattice_runtime::spawn_document;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("a-line-0\na-line-1\na-line-2\n"))
}

/// Insert a plain Document buffer into the registry WITHOUT activating it
/// (mirrors what `do_preview` does for an ephemeral preview slot). Returns
/// its id. The buffer is a real registry buffer with a live snapshot, so
/// the cells worker can build its matrix from `buffer_id`.
fn add_document(editor: &mut Editor, raw_id: u32, text: &str, name: &str) -> BufferId {
    let bid = BufferId(raw_id);
    let handle = spawn_document(bid, CoreDocument::from_text(text), editor.registry.clone());
    let arc: Arc<dyn lattice_runtime::Document> = Arc::new(handle);
    editor.buffers.insert(BufferEntry {
        id: bid,
        flags: BufferFlags {
            listed: true,
            hidden: false,
            ephemeral: false,
        },
        data: BufferData::Document(DocumentEntry {
            id: bid,
            handle: Arc::clone(&arc),
        }),
        name: Some(name.to_string()),
    });
    bid
}

/// PI.1: a preview override on the active pane makes the *published*
/// pane-tree leaf and the cells worker render buffer B, while the pane's
/// committed `buffer_id`, the global `document_buffer_id`, and the
/// `option_cache` are all left untouched. Exit is dropping the override.
#[test]
fn preview_override_renders_b_without_disturbing_committed_state() {
    let mut editor = boot();

    // Committed / active state before preview.
    let a_id = editor.document_buffer_id;
    let active_pane = editor.pane_tree.active().id;
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        a_id,
        "active pane is committed to A before preview"
    );
    let option_cache_before = format!("{:?}", editor.option_cache);

    // B is a distinct registry buffer we never activate.
    let b_id = add_document(&mut editor, 9999, "b-line-0\nb-line-1\n", "*B*");

    // Seat a preview override: the active pane now DISPLAYS B.
    editor.set_preview_override(
        active_pane,
        PreviewOverride {
            buffer_id: b_id,
            buffer: lattice_core::BufferKind::Document,
            cursor: lattice_protocol::position::Position::ZERO,
            scroll: 0,
        },
    );

    // Publish + run the cells worker.
    let rs = editor.build_render_state();
    editor.render_state.store(Arc::new(rs));
    lattice_host::cells_worker::recompute(&editor.render_state);

    // The PUBLISHED pane leaf shows B, with A preserved as committed.
    let published = editor.render_state.load();
    let leaf = published.panes.tree.active();
    assert_eq!(leaf.buffer_id, b_id, "published leaf displays B");
    assert_eq!(
        leaf.committed_buffer_id,
        Some(a_id),
        "published leaf preserves committed A"
    );
    assert_eq!(leaf.committed_id(), a_id, "committed_id() reports A");
    assert!(leaf.is_previewing(), "leaf reports previewing");

    // The cells worker built the active pane's matrix for B.
    let cells = published.cells.load();
    let entry = cells
        .panes
        .iter()
        .find(|p| p.pane_id == active_pane)
        .expect("active pane has a cells entry");
    assert_eq!(entry.buffer_id, b_id, "cells matrix keyed on displayed B");

    // Committed / global hot state is UNCHANGED.
    assert_eq!(
        editor.document_buffer_id, a_id,
        "document_buffer_id still A during preview"
    );
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        a_id,
        "live pane_tree stays committed to A"
    );
    assert_eq!(
        format!("{:?}", editor.option_cache),
        option_cache_before,
        "global option_cache unchanged during preview"
    );

    // Exit: dropping the override snaps the pane back to A with no
    // reconstruction.
    let removed = editor.clear_preview_override(active_pane);
    assert_eq!(removed.map(|o| o.buffer_id), Some(b_id));
    let rs = editor.build_render_state();
    editor.render_state.store(Arc::new(rs));
    let published = editor.render_state.load();
    let leaf = published.panes.tree.active();
    assert_eq!(leaf.buffer_id, a_id, "pane back to A after clear");
    assert_eq!(leaf.committed_buffer_id, None, "no override after clear");
}

/// PI.2: `mount_preview` gives the previewed buffer B its own read-only
/// resolved options (via `preview-mode` on B's stack) WITHOUT reassigning
/// `document_buffer_id`, rebuilding the global `option_cache`, or touching
/// the committed origin A's modes / options. `unmount_preview` restores B.
#[test]
fn mount_preview_isolates_read_only_options_from_origin() {
    let mut editor = boot();
    let a_id = editor.document_buffer_id;
    let active_pane = editor.pane_tree.active().id;

    // A distinct buffer B carrying its own major mode (rust-mode).
    let b_id = add_document(&mut editor, 9998, "fn main() {}\n", "*B.rs*");
    let _ = editor.activate_mode_by_id(b_id, ModeId::new("rust-mode"));

    // Origin + global state before mount.
    let a_read_only_before = *editor.resolved_option::<lattice_config::ReadOnly>(a_id);
    let a_modes_before = format!("{:?}", editor.active_modes.get(&a_id));
    let doc_before = editor.document_buffer_id;
    let option_cache_before = format!("{:?}", editor.option_cache);
    assert!(
        !*editor.resolved_option::<lattice_config::ReadOnly>(b_id),
        "B is writable before preview"
    );

    // Mount B as a read-only preview.
    let _ = editor.mount_preview(
        active_pane,
        b_id,
        lattice_protocol::position::Position::ZERO,
        0,
    );

    // B now resolves read-only, keeping its rust-mode major.
    assert!(
        *editor.resolved_option::<lattice_config::ReadOnly>(b_id),
        "B is read-only under preview-mode"
    );
    let b_modes = editor.active_modes.get(&b_id).expect("B has modes");
    assert!(
        b_modes
            .minors()
            .iter()
            .any(|m| *m == PreviewMode::mode_id()),
        "preview-mode is on B's stack (the ephemeral marker)"
    );

    // Origin A + global hot state are byte-identical.
    assert_eq!(
        *editor.resolved_option::<lattice_config::ReadOnly>(a_id),
        a_read_only_before,
        "A's ReadOnly is untouched"
    );
    assert_eq!(
        format!("{:?}", editor.active_modes.get(&a_id)),
        a_modes_before,
        "A's active_modes are untouched"
    );
    assert_eq!(
        editor.document_buffer_id, doc_before,
        "document_buffer_id unchanged"
    );
    assert_eq!(
        format!("{:?}", editor.option_cache),
        option_cache_before,
        "global option_cache unchanged"
    );

    // Unmount restores B and clears the projection.
    let _ = editor.unmount_preview(active_pane);
    assert!(
        !*editor.resolved_option::<lattice_config::ReadOnly>(b_id),
        "B is writable again after unmount"
    );
    assert!(
        !editor
            .active_modes
            .get(&b_id)
            .map(|m| m.minors().iter().any(|x| *x == PreviewMode::mode_id()))
            .unwrap_or(false),
        "preview-mode removed from B on unmount"
    );
    assert!(
        editor.preview_override_for(active_pane).is_none(),
        "pane override cleared on unmount"
    );
}

/// PI.3 (isolation acid test): previewing a rust buffer over a markdown
/// buffer through the real preview funnel leaves the markdown origin's
/// resolved options AND active modes byte-identical before and after, and
/// never moves `document_buffer_id`. This is the guarantee the whole PI
/// series exists to deliver.
#[test]
fn preview_cycle_leaves_markdown_origin_byte_identical() {
    let mut editor = boot();
    let a_id = editor.document_buffer_id;
    // A is a markdown buffer.
    let _ = editor.activate_mode_by_id(a_id, ModeId::new("markdown-mode"));

    // B is a rust buffer (a real, distinct registry buffer).
    let b_id = add_document(&mut editor, 9990, "fn main() {}\n", "*B.rs*");
    let _ = editor.activate_mode_by_id(b_id, ModeId::new("rust-mode"));

    // Capture A's full render-relevant state before any preview.
    let a_opts_before = resolved_snapshot(&editor, a_id);
    let a_modes_before = format!("{:?}", editor.active_modes.get(&a_id));
    let doc_before = editor.document_buffer_id;
    let cache_before = format!("{:?}", editor.option_cache);

    // Preview B (the risky swap in the old model).
    let _ = editor.preview_in_active_pane(b_id, None);
    assert_eq!(
        editor.document_buffer_id, doc_before,
        "document_buffer_id must not move during preview"
    );
    assert_eq!(
        format!("{:?}", editor.active_modes.get(&a_id)),
        a_modes_before,
        "A's modes untouched mid-preview"
    );
    assert_eq!(
        resolved_snapshot(&editor, a_id),
        a_opts_before,
        "A's resolved options untouched mid-preview"
    );

    // Exit the preview.
    let _ = editor.clear_active_preview();

    // A is byte-identical after the full cycle.
    assert_eq!(
        resolved_snapshot(&editor, a_id),
        a_opts_before,
        "A's resolved options byte-identical after preview cycle"
    );
    assert_eq!(
        format!("{:?}", editor.active_modes.get(&a_id)),
        a_modes_before,
        "A's active_modes byte-identical after preview cycle"
    );
    assert_eq!(
        format!("{:?}", editor.option_cache),
        cache_before,
        "global option_cache byte-identical after preview cycle"
    );
    assert_eq!(editor.document_buffer_id, doc_before);
    // B's preview-mode was stripped on exit.
    assert!(
        !editor
            .active_modes
            .get(&b_id)
            .map(|m| m.minors().iter().any(|x| *x == PreviewMode::mode_id()))
            .unwrap_or(false),
        "B is writable again after the preview cycle"
    );
}

/// PI.3 (dashboard-glitch regression): with the dashboard active, a
/// preview cycle must leave the dashboard's resolved `Number` and its
/// centring `content_left_pad` intact. The old swap-and-restore model
/// rebuilt the global `option_cache` from the previewed buffer, collapsing
/// the dashboard's centring to 0 and reverting `Number` to the default.
#[test]
fn dashboard_survives_preview_cycle() {
    let mut editor = boot();
    editor.do_open_dashboard();
    // Give the dashboard pane a width so centring is non-trivial, then
    // rebuild the option cache so `content_left_pad` reflects it.
    {
        let leaf = editor.pane_tree.active_mut();
        leaf.viewport_width = 120;
        leaf.viewport_height = 40;
    }
    editor.rebuild_option_cache();

    let dash_id = editor.document_buffer_id;
    let number_before = *editor.resolved_option::<lattice_config::Number>(dash_id);
    let cache_before = format!("{:?}", editor.option_cache);

    // A rust buffer to preview over the dashboard.
    let b_id = add_document(&mut editor, 9991, "fn main() {}\n", "*B.rs*");
    let _ = editor.activate_mode_by_id(b_id, ModeId::new("rust-mode"));

    let _ = editor.preview_in_active_pane(b_id, None);
    // Dashboard stays the committed/active document, so its cache holds.
    assert_eq!(
        format!("{:?}", editor.option_cache),
        cache_before,
        "dashboard option_cache (incl. content_left_pad) intact during preview"
    );

    let _ = editor.clear_active_preview();
    assert_eq!(
        *editor.resolved_option::<lattice_config::Number>(dash_id),
        number_before,
        "dashboard Number unchanged after preview cycle"
    );
    assert_eq!(
        format!("{:?}", editor.option_cache),
        cache_before,
        "dashboard option_cache byte-identical after preview cycle"
    );
}

/// A stable string of the render-relevant resolved options for `buffer`.
fn resolved_snapshot(editor: &Editor, buffer: BufferId) -> String {
    format!(
        "number={} rnu={} wrap={} readonly={} cursorline={} tabstop={}",
        *editor.resolved_option::<lattice_config::Number>(buffer),
        *editor.resolved_option::<lattice_config::RelativeNumber>(buffer),
        *editor.resolved_option::<lattice_config::Wrap>(buffer),
        *editor.resolved_option::<lattice_config::ReadOnly>(buffer),
        *editor.resolved_option::<lattice_config::CursorLine>(buffer),
        *editor.resolved_option::<lattice_config::Tabstop>(buffer),
    )
}
