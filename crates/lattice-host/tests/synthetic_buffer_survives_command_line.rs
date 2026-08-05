//! MG.47 — a synthetic buffer stays the pane's buffer across a `:` round-trip.
//!
//! Reported against magit: opening `:` in a magit buffer made the pane render
//! the file the magit buffer was opened *from*, and `<Esc>` left the modeline
//! naming that file while the content was still magit's. Only re-opening the
//! file cleared it.
//!
//! The renderer routes the focused pane to the registry-keyed *inactive* path
//! while the `:` line is open (`draw_panes`: `is_active` is false when
//! `command_line_active`), and that path renders `pane.buffer_id`. So the
//! invariant the renderer depends on is that opening and closing the command
//! line never disturbs the pane's committed buffer — which is exactly what
//! `focus_editing_buffer` promises ("WITHOUT touching the pane tree").
//!
//! These pin that promise. `*plugins*` covers the generic mechanism — any
//! `Effect::OpenSyntheticBuffer` open onto a provider-registered mode — and a
//! real `*magit:status*` covers the mode it was reported against, which
//! differs in ways that could matter (async content, a headerline virtual row,
//! a `prev_pane_for_popup` stash taken at open).
//!
//! **Status: these all PASS as written.** They are recorded as regression pins
//! for the invariant, not as a reproduction — the reported fault is not in the
//! state layer, so whatever triggers it lies outside what these construct.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// The pane's committed buffer must not move when the `:` line opens.
///
/// If it does, the renderer's inactive path paints whatever the pane now
/// points at — the previously-open file — which is the reported symptom.
#[tokio::test]
async fn opening_the_command_line_leaves_the_panes_buffer_alone() {
    let mut editor = Editor::boot(CoreDocument::from_text("original file\n"));
    let origin = editor.document_buffer_id;

    editor.open_synthetic_buffer("*plugins*", "plugins-mode");
    let synthetic = editor
        .buffers
        .by_name("*plugins*")
        .expect("the synthetic buffer exists");
    assert_ne!(synthetic, origin, "the open switched buffers");
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        synthetic,
        "opening a synthetic buffer commits it to the pane",
    );

    editor.open_command_line("");

    assert_eq!(
        editor.pane_tree.active().buffer_id,
        synthetic,
        "the `:` line must not move the pane off its buffer — the renderer \
         paints `pane.buffer_id` while the command line is open, so a moved \
         id shows the wrong buffer's text",
    );
    assert_ne!(
        editor.pane_tree.active().buffer_id,
        origin,
        "specifically: it must not fall back to the buffer the synthetic one \
         was opened from",
    );
}

/// The renderer does not read `editor.pane_tree` — it reads the **published**
/// `PanesRenderState`. That is the state the inactive path resolves
/// `pane.buffer_id` from, so it is the one that has to be right.
#[tokio::test]
async fn the_published_pane_state_names_the_synthetic_buffer_while_the_line_is_open() {
    let mut editor = Editor::boot(CoreDocument::from_text("original file\n"));
    let origin = editor.document_buffer_id;

    editor.open_synthetic_buffer("*plugins*", "plugins-mode");
    let synthetic = editor.buffers.by_name("*plugins*").unwrap();

    editor.open_command_line("");
    editor.publish_render_state();

    let published = editor.render_state.load().panes.clone();
    let active = published.tree.active();
    assert_eq!(
        active.buffer_id, synthetic,
        "the renderer paints the PUBLISHED pane's buffer while `:` is open; \
         if this still names the origin file, the publish is stale and the \
         pane shows the wrong buffer's text",
    );
    assert_ne!(active.buffer_id, origin, "not the file it was opened from");
}

/// The same round-trip on a **real magit buffer**, which is where it was
/// reported. `*plugins*` above proves the generic mechanism; this proves the
/// mode that actually broke, since magit differs in ways that could matter
/// (async content, a headerline virtual row, a `prev_pane_for_popup` stash).
#[tokio::test]
async fn a_magit_buffer_survives_the_command_line_round_trip() {
    let mut editor = Editor::boot(CoreDocument::from_text("ORIGINFILECONTENT\n"));
    let origin = editor.document_buffer_id;

    editor.open_synthetic_buffer("*magit:status*", "magit-status-mode");
    let magit = editor
        .buffers
        .by_name("*magit:status*")
        .expect("the magit status buffer exists after :magit-status");
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        magit,
        "opening magit commits its buffer to the pane",
    );

    editor.open_command_line("");
    editor.publish_render_state();
    let published = editor.render_state.load().panes.clone();
    assert_eq!(
        published.tree.active().buffer_id,
        magit,
        "while `:` is open the pane must still name the magit buffer — the \
         renderer paints THIS id, so the origin file here is the reported bug",
    );
    assert_ne!(published.tree.active().buffer_id, origin);

    editor.restore_editing_buffer();
    editor.publish_render_state();
    assert_eq!(
        editor.document_buffer_id, magit,
        "`<Esc>` returns editing focus to the magit buffer, not the file it \
         was opened from — a modeline naming the origin file is the second \
         half of the report",
    );
    assert_eq!(
        editor.render_state.load().panes.tree.active().buffer_id,
        editor.document_buffer_id,
        "pane and editing focus must agree after the round-trip",
    );
}

/// **The reported flow.** Launch → dashboard → `<C-x>g` → magit → `:`.
///
/// The origin here is the `*dashboard*` buffer, not a file, and that is the
/// difference from every case above: `BufferKind::Dashboard` is its own kind
/// that rides the Document activation pipeline. If the magit open leaves the
/// pane pointing at the dashboard, the renderer's inactive path paints
/// dashboard content the moment `:` drops `is_active` — exactly as reported.
#[tokio::test]
async fn magit_opened_from_the_dashboard_survives_the_command_line() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.do_open_dashboard();
    let dashboard = editor
        .buffers
        .by_name("*dashboard*")
        .expect("the dashboard buffer exists");
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        dashboard,
        "sanity: the dashboard is what the pane shows at launch",
    );

    editor.open_synthetic_buffer("*magit:status*", "magit-status-mode");
    let magit = editor.buffers.by_name("*magit:status*").unwrap();
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        magit,
        "`<C-x>g` from the dashboard must commit the magit buffer to the pane",
    );

    editor.open_command_line("");
    editor.publish_render_state();
    let published = editor.render_state.load().panes.clone();
    assert_eq!(
        published.tree.active().buffer_id,
        magit,
        "while `:` is open the pane must still name the magit buffer; naming \
         the dashboard here is the reported bug",
    );
    assert_ne!(
        published.tree.active().buffer_id,
        dashboard,
        "the pane must not fall back to the dashboard",
    );

    editor.restore_editing_buffer();
    editor.publish_render_state();
    assert_eq!(
        editor.document_buffer_id, magit,
        "`<Esc>` returns editing focus to magit, not the dashboard",
    );
    assert_eq!(
        editor.render_state.load().panes.tree.active().buffer_id,
        editor.document_buffer_id,
        "pane and editing focus must agree — disagreement is what makes the \
         modeline say dashboard while the content is magit's",
    );
}

/// ...and `<Esc>` must put the editing focus back on the synthetic buffer,
/// not on the file underneath it.
///
/// The reported symptom for this half was a modeline naming the previous file
/// while the pane still showed magit's text — `document_buffer_id` and
/// `pane.buffer_id` disagreeing after the round-trip.
#[tokio::test]
async fn closing_the_command_line_restores_the_synthetic_buffer_not_its_origin() {
    let mut editor = Editor::boot(CoreDocument::from_text("original file\n"));
    let origin = editor.document_buffer_id;

    editor.open_synthetic_buffer("*plugins*", "plugins-mode");
    let synthetic = editor.buffers.by_name("*plugins*").unwrap();

    editor.open_command_line("");
    editor.restore_editing_buffer();

    assert_eq!(
        editor.document_buffer_id, synthetic,
        "`<Esc>` returns editing focus to the buffer the `:` was opened from",
    );
    assert_ne!(
        editor.document_buffer_id, origin,
        "not to the file the synthetic buffer was opened from",
    );
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        editor.document_buffer_id,
        "after the round-trip the pane and the editing focus must agree — \
         when they disagree the modeline names one buffer and the content \
         shows the other",
    );
}
