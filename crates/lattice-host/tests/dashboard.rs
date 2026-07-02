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
fn dashboard_content_has_section_pointers() {
    let mut editor = boot();
    editor.do_open_dashboard();

    // The dashboard is active, so active_text() is its body content. The brand
    // wordmark lives in the DB.4 virtual-row banner, not the body.
    let text = editor.active_text().as_string();
    assert!(text.contains("tutor"), "tutor pointer missing");
    assert!(text.contains("help"), "help pointer missing");
}

#[test]
fn dashboard_registers_branding_virtual_rows() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    // DB.4: the branding provider is registered for the dashboard buffer and
    // emits the mark + wordmark rows, colored with the brand blue.
    let providers = editor.virtual_row_providers.snapshot(id);
    assert!(!providers.is_empty(), "branding provider should be registered");
    let brand_blue = lattice_theme::Color::Rgb(0x1f, 0x6f, 0xeb).to_rgb_u32(0);
    let has_blue_glyph = providers.iter().flat_map(|p| p.collect()).any(|row| {
        row.cells.iter().any(|c| c.fg == brand_blue && c.codepoint != 0x20)
    });
    let has_wordmark = providers.iter().flat_map(|p| p.collect()).any(|row| {
        let t: String = row.cells.iter().filter_map(|c| char::from_u32(c.codepoint)).collect();
        t.contains("Lattice")
    });
    assert!(has_blue_glyph, "branding should render brand-blue mark glyphs");
    assert!(has_wordmark, "branding should render the Lattice wordmark");
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
fn dashboard_activates_help_mode_and_is_gutterless() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    // help-mode is a companion minor (its good defaults: read-only, wrap,
    // no-file, gutterless).
    let has_help = editor
        .active_modes
        .get(&id)
        .map(|m| m.has_minor(lattice_mode::HelpMode::mode_id()))
        .unwrap_or(false);
    assert!(has_help, "help-mode should be active on the dashboard");
    assert!(!editor.option_cache.show_line_numbers, "no line numbers");
    assert!(!editor.option_cache.sign_column, "no signcolumn");
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
fn dashboard_branding_reaches_pane_virtual_row_matrix() {
    // Regression: registering the provider is not enough — the branding rows
    // must reach the pane's VirtualRowMatrix (the "registered but not
    // rendered" failure mode). Drive the virtual-rows worker and assert the
    // dashboard pane's matrix carries the banner rows.
    let mut editor = boot();
    editor.viewport_height = 40;
    editor.do_open_dashboard();

    let rs = editor.build_render_state();
    editor.render_state.store(std::sync::Arc::new(rs));
    let mut state = lattice_host::virtual_rows_worker::VirtualRowsWorkerState::default();
    lattice_host::virtual_rows_worker::recompute(
        &mut state,
        &editor.render_state,
        &editor.virtual_row_providers,
    );

    let cells = editor.render_state.load().cells.load();
    let total_rows: usize = cells
        .panes
        .iter()
        .map(|p| p.virtual_rows_matrix.load().rows.len())
        .sum();
    assert!(
        total_rows >= lattice_dashboard::BRANDING_ROW_COUNT,
        "branding rows should reach the pane virtual-row matrix, got {total_rows}"
    );
}

#[test]
fn dashboard_body_is_block_centered_to_pane_width() {
    let mut editor = boot();
    editor.pane_tree.active_mut().viewport_width = 120;
    editor.do_open_dashboard();

    // Body content is block-centred: every non-blank line carries a uniform
    // leading inset (lines that had internal indentation carry inset + that).
    let text = editor.active_text().as_string();
    let min_inset = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    assert!(
        min_inset > 0,
        "body should be centred (non-zero leading inset) at width 120"
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
