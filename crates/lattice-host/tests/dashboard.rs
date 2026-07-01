//! DB.2 — `*dashboard*`-is-a-buffer integration tests.
//!
//! Drives a real `Editor::boot` (which runs the Phase-B install list, so
//! `dashboard-mode` + the `DashboardRegistry` service + `:dashboard` are all
//! registered) and exercises `do_open_dashboard` end-to-end: the buffer is
//! created, named, activated, read-only via `dashboard-mode`, carries the
//! `HelpLinks` local so `<CR>`-follow works, and re-opening is idempotent.
//!
//! Design: `docs/dev/architecture/dashboard.md` §9.

use std::collections::HashSet;

use lattice_core::{BufferKind, Document as CoreDocument};
use lattice_dashboard::DashboardMode;
use lattice_host::editor::Editor;
use lattice_host::modes::HelpLinks;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

/// Build the render state, run the cells worker, and return the set of
/// distinct foreground colours across all non-blank rendered cells.
fn rendered_fg_colors(editor: &mut Editor) -> HashSet<u32> {
    let rs = editor.build_render_state();
    editor.render_state.store(std::sync::Arc::new(rs));
    lattice_host::cells_worker::recompute(&editor.render_state);
    let cells = editor.render_state.load().cells.load();
    let mut fgs = HashSet::new();
    for pane in cells.panes.iter() {
        let matrix = pane.matrix.load();
        for chunk in matrix.chunks.iter() {
            for row in chunk.rows.iter() {
                for cell in row.cells.iter() {
                    if cell.codepoint != 0x20 && cell.codepoint != 0 {
                        fgs.insert(cell.fg);
                    }
                }
            }
        }
    }
    fgs
}

#[test]
fn dashboard_opens_named_and_active() {
    let mut editor = boot();
    editor.do_open_dashboard();

    let id = editor
        .buffers
        .by_name("*dashboard*")
        .expect("*dashboard* buffer should exist after :dashboard");

    // Distinct kind, and it is the active buffer.
    assert_eq!(editor.buffers.kind_of(id), Some(BufferKind::Dashboard));
    assert_eq!(editor.active_buffer, BufferKind::Dashboard);
}

#[test]
fn dashboard_major_is_dashboard_mode() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    // dashboard-mode is the major, which is what contributes ReadOnly /
    // NoFile / gutterless.
    let major = editor.active_modes.get(&id).and_then(|m| m.major());
    assert_eq!(major, Some(DashboardMode::mode_id()));
}

#[test]
fn dashboard_content_has_branding_and_pointers() {
    let mut editor = boot();
    editor.do_open_dashboard();

    // The dashboard is active, so active_text() is its content.
    let text = editor.active_text().as_string();
    assert!(text.contains("Lattice"), "branding wordmark missing:\n{text}");
    // A couple of the built-in section pointers.
    assert!(text.contains("tutor"), "tutor pointer missing");
    assert!(text.contains("help"), "help pointer missing");
}

#[test]
fn dashboard_seeds_help_links_for_follow() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    let links = editor
        .buffer_locals
        .get(&id)
        .and_then(|locals| locals.get::<HelpLinks>())
        .map(|hl| hl.0.len())
        .unwrap_or(0);
    assert!(
        links > 0,
        "dashboard should seed HelpLinks so <CR>-follow works (gated on \
         help-mode-independent path via the Dashboard kind)"
    );
}

#[test]
fn dashboard_registers_theme_elements() {
    let mut editor = boot();
    editor.do_open_dashboard();

    // dashboard-mode's on_activate registers the dashboard.* elements against
    // the host's ThemeRegistry service (DB.3).
    let theme = editor
        .services
        .get::<lattice_theme::ThemeRegistryHandle>()
        .expect("theme registry service registered at boot");
    for name in ["dashboard.logo", "dashboard.cursor", "dashboard.title"] {
        assert!(
            theme
                .id(&lattice_theme::ElementName::from(name.to_string()))
                .is_some(),
            "{name} should be registered after :dashboard"
        );
    }
}

#[test]
fn dashboard_renders_through_colored_matrix_not_raw_fallback() {
    // Regression: the pane-cells builder allowlist must include
    // BufferKind::Dashboard, else the dashboard pane is skipped, no
    // DisplayMatrix is built, and the TUI falls back to uncoloured raw text
    // (the "no colors on the dashboard" bug).
    let mut editor = boot();
    editor.viewport_height = 40;
    editor.do_open_dashboard();

    let fgs = rendered_fg_colors(&mut editor);
    assert!(
        fgs.len() >= 2,
        "dashboard should render multiple distinct fg colors (markdown \
         headings + links + body), got {} — the DisplayMatrix was likely \
         skipped and the pane fell back to raw text",
        fgs.len()
    );
}

#[test]
fn dashboard_reopen_is_idempotent() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let first = editor.buffers.by_name("*dashboard*").unwrap();

    // Switch away, then re-open: same buffer id, no duplicate.
    editor.do_open_dashboard();
    let second = editor.buffers.by_name("*dashboard*").unwrap();
    assert_eq!(first, second, "re-opening must not create a second buffer");
}
