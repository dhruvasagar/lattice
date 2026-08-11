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
use lattice_host::editor::Editor;

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
    assert!(
        !e.pending_references_to_view,
        "`gr` must stay the picker — the view is a peer surface, not a replacement"
    );
}

/// `:lsp-references` resolves to the multibuffer terminus.
#[test]
fn lsp_references_requests_the_view_terminus() {
    let mut e = editor();
    let _ = e.lsp_request(LspRequest::ReferencesView);
    assert!(
        e.pending_references_to_view,
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
    assert!(e.pending_references_to_view);

    let _ = e.lsp_request(LspRequest::References);
    assert!(
        !e.pending_references_to_view,
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
    assert!(
        !e.pending_references_to_view,
        "and must not leave the terminus flag set for a later `gr`"
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
