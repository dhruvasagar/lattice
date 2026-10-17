//! `:version` opens the `*version*` buffer — `markdown-mode` with
//! `version-mode` on top — showing the report the binary handed over before
//! boot, and `Y` there copies the page.
//!
//! One process-global report (`set_version_report` is a `OnceLock`), so every
//! test here sets the same one.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, LookupResult};
use lattice_mode::{BufferStoreHandle, VersionMode, VersionReport, VersionSection};

fn install_report() {
    lattice_mode::set_version_report(VersionReport {
        version: "9.9.9-test".into(),
        sections: vec![VersionSection {
            heading: "Build".into(),
            rows: vec![("Commit".into(), "abc123def456 (clean), 2026-10-16".into())],
        }],
    });
}

fn text_of(editor: &Editor, name: &str) -> String {
    let id = editor.buffers.by_name(name).expect("buffer exists");
    let store = editor.services.get::<BufferStoreHandle>().unwrap();
    let snap = store.handle_for(id).unwrap().snapshot();
    // `line` returns a line without its newline.
    (0..snap.buffer.rope_line_count())
        .map(|n| snap.buffer.line(n).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run `:version` the way the `:` line does, applying the open it returns.
fn run_version(editor: &mut Editor) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("version", &mut out);
    for effect in out.effects {
        if let lattice_grammar::Effect::OpenSyntheticBuffer {
            name,
            mode_id,
            content,
            cursor,
            activate_minor,
        } = effect
        {
            editor.open_synthetic_buffer_seeded(
                &name,
                &mode_id,
                content.as_deref(),
                cursor,
                activate_minor.as_deref(),
            );
        }
    }
}

#[tokio::test]
async fn version_opens_the_report_under_version_mode() {
    install_report();
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    run_version(&mut editor);

    let id = editor
        .buffers
        .by_name("*version*")
        .expect("`:version` opens *version*");
    let modes = editor.active_modes.get(&id).expect("the buffer has modes");
    assert_eq!(
        modes.major(),
        Some(lattice_mode::ModeId::new("markdown-mode")),
        "the page is markdown, so it highlights as markdown"
    );
    assert!(
        modes.is_active(VersionMode::mode_id()),
        "version-mode rides on it for `Y` and read-only"
    );
    assert!(
        modes.is_active(lattice_mode::modes::ReadOnlyMode::mode_id()),
        "the page refuses edits: version-mode implies read-only-mode"
    );

    let text = text_of(&editor, "*version*");
    assert!(text.starts_with("# Lattice 9.9.9-test\n"), "{text}");
    assert!(text.contains("- **Commit:** abc123def456 (clean), 2026-10-16"));
}

/// `Y` is the page copy in this buffer; `y` is left to vim's yank.
#[test]
fn capital_y_is_bound_to_the_page_copy_in_version_mode() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let registry = editor.registry.load();
    let copy = registry
        .id_by_name("action:version-copy")
        .expect("the copy action registers at boot");
    let chord = lattice_protocol::parse_chord_sequence("Y").unwrap();
    let LookupResult::Bound { command, .. } =
        editor
            .keymap
            .lookup_with_context(BindingMode::Normal, &chord, &[VersionMode::mode_id()])
    else {
        panic!("`Y` resolves in version-mode");
    };
    assert_eq!(command.command.command, copy);
}
