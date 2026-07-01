//! DB.2 — `*dashboard*`-is-a-buffer integration tests.
//!
//! Drives a real `Editor::boot` (which runs the Phase-B install list, so
//! `dashboard-mode` + the `DashboardRegistry` service + `:dashboard` are all
//! registered) and exercises `do_open_dashboard` end-to-end: the buffer is
//! created, named, activated, read-only via `dashboard-mode`, carries the
//! `HelpLinks` local so `<CR>`-follow works, and re-opening is idempotent.
//!
//! Design: `docs/dev/architecture/dashboard.md` §9.

use lattice_core::{BufferKind, Document as CoreDocument};
use lattice_dashboard::DashboardMode;
use lattice_host::editor::Editor;
use lattice_host::modes::HelpLinks;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
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
fn dashboard_reopen_is_idempotent() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let first = editor.buffers.by_name("*dashboard*").unwrap();

    // Switch away, then re-open: same buffer id, no duplicate.
    editor.do_open_dashboard();
    let second = editor.buffers.by_name("*dashboard*").unwrap();
    assert_eq!(first, second, "re-opening must not create a second buffer");
}
