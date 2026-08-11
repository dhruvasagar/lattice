//! LR.2 (2026-08-11): `gr` and `:lsp-references` share one request and
//! diverge only at the terminus.
//!
//! Design: `docs/dev/architecture/lsp-architecture.md` §17.
//!
//! The regression that matters most is the first test: `gr` has always
//! opened a picker, and the whole design of this slice rests on that
//! not changing. Everything else here is additive.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::LspRequest;
use lattice_host::editor::{Editor, ReferencesTerminus};

fn editor() -> Editor {
    Editor::boot(CoreDocument::from_text("fn main() {}\n"))
}

/// `gr`'s action resolves to the picker terminus, not the view.
#[test]
fn gr_requests_the_picker_terminus() {
    let mut e = editor();
    // The nav gate may decline without a server attached; what is
    // pinned here is the terminus flag the request records, which is
    // set before any gate.
    let _ = e.lsp_request(LspRequest::References);
    assert_eq!(
        e.pending_references_terminus,
        ReferencesTerminus::Picker,
        "`gr` must stay the picker — the view is a peer surface, not a replacement"
    );
}

/// `:lsp-references` resolves to the multibuffer terminus.
#[test]
fn lsp_references_requests_the_view_terminus() {
    let mut e = editor();
    let _ = e.lsp_request(LspRequest::ReferencesView);
    assert_eq!(
        e.pending_references_terminus,
        ReferencesTerminus::View,
        "`:lsp-references` must record the view terminus"
    );
}

/// The terminus is per-request, not sticky: a `gr` after a
/// `:lsp-references` gets the picker back. Without the reset, one use
/// of the ex-command would silently convert every later `gr`.
#[test]
fn the_terminus_does_not_leak_between_requests() {
    let mut e = editor();
    let _ = e.lsp_request(LspRequest::ReferencesView);
    assert_eq!(e.pending_references_terminus, ReferencesTerminus::View);

    let _ = e.lsp_request(LspRequest::References);
    assert_eq!(
        e.pending_references_terminus,
        ReferencesTerminus::Picker,
        "a later `gr` must not inherit the previous request's terminus"
    );
}

/// Both commands are registered and distinct.
#[test]
fn both_reference_surfaces_are_registered() {
    let e = editor();
    let reg = e
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .unwrap();
    let reg = reg.load();
    let view = reg.id_by_name("ex:lsp-references");
    assert!(view.is_some(), "`:lsp-references` must resolve");
    // The picker path is an action, not an ex-command — they are
    // different surfaces and must not collapse into one id.
    let picker = reg.id_by_name("action:lsp-references");
    assert!(picker.is_some(), "`gr`'s action must still resolve");
    assert_ne!(view, picker, "the two surfaces must stay distinct");
}

// ── LR.3: refresh re-queries the ORIGIN, not the cursor ──────────────

/// `gr` outside a references view must say so, not silently no-op.
/// Before RV.1 an unhandled `gr` was swallowed; the whole point of the
/// shared chord is that absence is spoken.
#[test]
fn refresh_outside_a_references_view_echoes() {
    let mut e = editor();
    let _ = e.lsp_request(LspRequest::ReferencesViewRefresh);
    // No view is active, so no request was issued.
    assert!(
        e.refreshing_references_view.is_none(),
        "a refresh outside a references view must not arm a request"
    );
    assert_eq!(
        e.pending_references_terminus,
        ReferencesTerminus::Picker,
        "and must not leave a terminus armed for a later `gr`"
    );
}

/// The refresh action is registered, so the mode's declared target
/// resolves and RV.1's dispatch can redirect `gr` to it.
#[test]
fn the_refresh_action_resolves() {
    let e = editor();
    let reg = e
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .unwrap();
    assert!(
        reg.load()
            .id_by_name("action:lsp-references-refresh")
            .is_some(),
        "`gr` in the view resolves through this id; unregistered means a dead key"
    );
}

/// The references mode is registered and declares its refresh, which is
/// what pulls `refreshable-view-mode` in through the implies cascade.
#[test]
fn the_references_mode_declares_its_refresh() {
    use lattice_lsp::providers::references::LspReferencesMode;
    use lattice_mode::Mode;
    let e = editor();
    assert!(
        e.mode_registry
            .load()
            .is_registered(LspReferencesMode::mode_id()),
        "the cascade can only pull in a registered mode"
    );
    assert_eq!(
        <LspReferencesMode as Mode>::refresh_action(&LspReferencesMode),
        Some("action:lsp-references-refresh"),
    );
}

// ── EP.6: references as a third, opt-in error-list producer ──────────

/// The third terminus is recorded distinctly from the other two.
#[test]
fn references_to_error_list_records_its_own_terminus() {
    let mut e = editor();
    let _ = e.lsp_request(LspRequest::ReferencesToErrorList);
    assert_eq!(e.pending_references_terminus, ReferencesTerminus::ErrorList,);
}

#[test]
fn the_references_to_error_list_command_resolves() {
    let e = editor();
    let reg = e
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .unwrap();
    assert!(
        reg.load()
            .id_by_name("ex:lsp-references-to-error-list")
            .is_some()
    );
}

/// Opt-in: the option must default OFF. Diagnostics default on because
/// they ARE errors; references would change what `]qq` walks for
/// everyone using it on compile output.
#[test]
fn the_references_option_defaults_off() {
    let e = editor();
    let on = e
        .config
        .get_typed::<lattice_config::core_options::LspReferencesToErrorList>()
        .map(|v| *v)
        .unwrap_or(false);
    assert!(!on, "references must not join the error list by default");
}

/// The clobber regression, per source: a references push replaces only
/// the References slice. This is what made the producer acceptable —
/// §17 rejected it precisely because it would collide with a live
/// compile list, and per-source slices are the answer.
#[test]
fn a_references_push_leaves_compile_and_lsp_slices_intact() {
    use lattice_host::error_list::{ErrorEntry, ErrorSeverity, ErrorSource};
    use lattice_protocol::error_list::ErrorWrite;

    fn entry(p: &str, line: u32) -> ErrorEntry {
        ErrorEntry {
            path: std::path::PathBuf::from(p),
            line,
            col: 0,
            severity: ErrorSeverity::Error,
            message: format!("m{line}"),
        }
    }

    let mut e = editor();
    e.set_error_list(ErrorSource::Compilation, vec![entry("c.rs", 1)]);
    e.write_error_list(
        ErrorSource::Lsp,
        ErrorWrite::Refresh,
        vec![entry("l.rs", 2)],
    );
    e.write_error_list(
        ErrorSource::References,
        ErrorWrite::NewRun,
        vec![entry("r.rs", 3)],
    );

    assert_eq!(e.error_list().len(), 3);
    assert_eq!(
        e.error_list().entries_from(ErrorSource::Compilation).len(),
        1,
        "a references push must not disturb a compile run being walked"
    );
    assert_eq!(e.error_list().entries_from(ErrorSource::Lsp).len(), 1);
    assert_eq!(
        e.error_list().entries_from(ErrorSource::References).len(),
        1
    );
}

// ── LR.5: `<C-q>` sends the filtered set to the error list ───────────
//
// This is telescope's `send_to_qflist`. Across the vim ecosystem
// `<C-q>` in a fuzzy finder populates the quickfix list, so it is
// generic over every picker rather than a per-picker affordance.

/// A picker whose candidates carry no location has nothing to send, and
/// must SAY so rather than swallowing the key.
#[test]
fn bulk_accept_with_no_locations_echoes_and_keeps_the_picker() {
    use lattice_picker::{Picker, PickerAction, PickerSource};

    let mut e = editor();
    e.set_active_picker(Picker::new(
        "buffers",
        PickerSource::Buffers,
        PickerAction::SwitchToBuffer,
    ));

    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    e.do_picker_bulk_accept(&mut out);

    assert!(
        e.picker.is_some(),
        "nothing was sent, so the picker stays up"
    );
    assert!(
        e.error_list()
            .entries_from(lattice_host::error_list::ErrorSource::Picker)
            .is_empty(),
        "an empty send must not create a slice"
    );
}

/// The generic path: a locations picker sends its rows to the error
/// list and dismisses. Not references-specific — the picker source is
/// irrelevant, only whether its candidates have somewhere to jump to.
#[test]
fn bulk_accept_sends_locations_to_the_error_list() {
    use lattice_host::error_list::ErrorSource;
    use lattice_lsp::lsp_types::{Location, Position, Range, Uri};

    let dir = std::env::temp_dir().join(format!("lattice-lr5-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("x.rs");
    std::fs::write(
        &file,
        "one
two
three
",
    )
    .unwrap();

    let loc = |line: u32| Location {
        uri: format!("file://{}", file.display()).parse::<Uri>().unwrap(),
        range: Range {
            start: Position { line, character: 2 },
            end: Position { line, character: 3 },
        },
    };

    let mut e = editor();
    e.open_lsp_locations_picker("references: foo", &[loc(0), loc(2)]);

    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    e.do_picker_bulk_accept(&mut out);

    let sent = e.error_list().entries_from(ErrorSource::Picker);
    assert_eq!(sent.len(), 2, "both rows land in the error list");
    assert_eq!(sent[0].line, 0);
    assert_eq!(sent[1].line, 2);
    assert_eq!(sent[0].col, 2, "the column survives the round trip");
    assert!(e.picker.is_none(), "a successful send dismisses the picker");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The send replaces only its own slice — a compile run being walked
/// survives someone sending a picker's results.
#[test]
fn a_picker_send_leaves_a_compile_run_intact() {
    use lattice_host::error_list::{ErrorEntry, ErrorSeverity, ErrorSource};
    use lattice_lsp::lsp_types::{Location, Position, Range, Uri};

    let dir = std::env::temp_dir().join(format!("lattice-lr5b-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("y.rs");
    std::fs::write(
        &file, "a
b
",
    )
    .unwrap();

    let mut e = editor();
    e.set_error_list(
        ErrorSource::Compilation,
        vec![ErrorEntry {
            path: std::path::PathBuf::from("build.rs"),
            line: 9,
            col: 0,
            severity: ErrorSeverity::Error,
            message: "boom".to_string(),
        }],
    );

    e.open_lsp_locations_picker(
        "refs",
        &[Location {
            uri: format!("file://{}", file.display()).parse::<Uri>().unwrap(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
        }],
    );
    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    e.do_picker_bulk_accept(&mut out);

    assert_eq!(
        e.error_list().entries_from(ErrorSource::Compilation).len(),
        1,
        "sending from a picker must not disturb a compile run"
    );
    assert_eq!(e.error_list().entries_from(ErrorSource::Picker).len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}
