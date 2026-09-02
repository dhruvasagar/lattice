//! OA.16 — `cr` reaches the clock report, at boot, from a scan view.
//!
//! Slice plan: `docs/dev/operations/slice-plans/org-agenda.md` (OA.16).
//!
//! The unit tests in `lattice-multibuffer` prove the mode registers, the report
//! builds, the toggle action flips the right mode and the guard tears the rows
//! down. None of them prove the chord can be *typed* — and a display mode
//! nobody can turn on is not a feature. That gap is what this file pins, and it
//! is a gap that has shipped before: `magit-project-diff` declared a chord
//! whose command never resolved, and the failure was silent.
//!
//! Three things have to hold together, and each fails quietly on its own:
//!
//! 1. the chord binds on `scan-view-mode`'s own layer,
//! 2. its target is registered in the command registry the keymap resolves
//!    against (an unresolvable name is dropped from the layer, not reported),
//! 3. K.1.c gates it to scan views, so `c` keeps meaning "change" everywhere
//!    else — the price of a `c`-prefixed chord, paid where it can be seen.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, KeymapLayer, KeymapResolution, LayerHit};
use lattice_mode::ModeId;
use lattice_protocol::chord::KeyChord;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn scan_view_mode() -> ModeId {
    ModeId::new("scan-view-mode")
}

fn scan_view_layer_hit(res: &KeymapResolution) -> Option<&LayerHit> {
    res.hits
        .iter()
        .find(|h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == scan_view_mode()))
}

/// The report's mode has to exist before anything can toggle it.
#[test]
fn the_clockreport_mode_is_registered_at_boot() {
    assert!(
        boot()
            .mode_registry
            .load()
            .is_registered(ModeId::new("scan-view-clockreport-mode")),
        "the mode `cr` toggles must be registered at boot"
    );
}

/// `cr` binds on the view's layer AND resolves to the toggle action. Asserting
/// the target — not merely that something is bound — is the half of this that
/// catches an unregistered command name: the translate pass drops an
/// unresolvable entry, leaving a chord that silently does nothing.
#[test]
fn cr_binds_to_the_clock_report_toggle_on_the_scan_view_layer() {
    let editor = boot();
    let expected = editor
        .registry
        .load()
        .id_by_name("action:scan-view-clockreport-toggle")
        .expect("`cr`'s target must be registered at boot");
    let chords = [KeyChord::char('c'), KeyChord::char('r')];
    let res = editor
        .keymap
        .resolve_trace(BindingMode::Normal, &chords, &[scan_view_mode()]);
    let hit = scan_view_layer_hit(&res).expect("`cr` must bind on the scan-view-mode layer");
    assert!(hit.active, "`cr` must fire when the view's mode is active");
    assert_eq!(
        hit.command.command.command, expected,
        "`cr` must target the clock-report toggle"
    );
}

/// …and nowhere else. `c` is the change operator in every ordinary buffer, and
/// a display toggle must not take a grammar letter hostage — K.1.c's
/// per-keystroke filter is what makes a `c`-prefixed chord affordable here.
#[test]
fn cr_is_inactive_outside_a_scan_view() {
    let editor = boot();
    let chords = [KeyChord::char('c'), KeyChord::char('r')];
    let res = editor
        .keymap
        .resolve_trace(BindingMode::Normal, &chords, &[]);
    if let Some(hit) = scan_view_layer_hit(&res) {
        assert!(
            !hit.active,
            "`cr` must be inactive with no scan view active, or `c` stops \
             meaning change"
        );
    }
}
