//! DX.1 — diff-subsystem boot regression pins (the gate for the BC.6 extraction).
//!
//! BC.6 moves the whole host-side diff subsystem (`crate::diff`, 7 files) out of
//! `lattice-host` into the `lattice-diff` crate and installs it through the
//! `SubsystemBoot` seam (`lattice_diff::install(boot)`), decomposing it into
//! `diff-mode` + `diff-conflict-mode`. That is a multi-slice cross-crate
//! extraction (DX.2–DX.final). These pins capture the diff subsystem's CURRENT
//! boot contract BEFORE anything moves, so a later slice that silently drops a
//! piece of wiring fails here, not in the field.
//!
//! Design + sequence: `docs/dev/operations/slice-plans/diff-extraction.md`
//! (DX.1). Parent initiative: `docs/dev/operations/slice-plans/boot-composition.md`
//! (BC.6). Sibling generic pins: `tests/boot_regression_pins.rs` (BC.2) — which
//! already pins `diff-mode` registered; this file pins the FULL diff boot
//! contract the extraction must preserve.
//!
//! ## What each pin guards against in the move
//!
//! - **mode + ex-commands + modeline element registered** — DX.7 collapses
//!   diff's `register_diff_modes` / `register_diff_modeline_element` / the
//!   `:diff` family registration into one `install(boot)` line; these pin that
//!   the surface survives the collapse.
//! - **`do`/`dp` on the diff-mode layer, gated by mode-active** — DX.5 converts
//!   the layer builder to resolve actions *by name* (dropping the typed
//!   `&ActionIds`) and DX.7 re-pushes the layer through `install`; these pin
//!   that the chords still bind to the diff-get/diff-put actions AND stay
//!   invisible (inactive) on non-diff buffers (the K.1.c gating).
//! - **subsystem bound to the editor's bus** — DX.7 rewires `diff_subsystem.bind`
//!   with a host-provided resolver; the end-to-end `DocumentClosed` pin proves
//!   the bind targets `editor.event_bus` + the real registry resolver, which a
//!   plain `guard.is_some()` could not (it would pass even if bound to the wrong
//!   bus). The drain *mechanism* itself is unit-pinned in `diff/subsystem.rs`
//!   (`bind_routes_document_changed_to_debounced_recompute` /
//!   `bind_routes_document_closed_to_drop_session`), which move with the file.
//!
//! The sign-gutter decoration path is mode-owned, so it is pinned as a unit test
//! in `crates/lattice-host/src/diff/mode.rs`
//! (`gutter_decorations_emit_diff_signs_from_sign_map`) — it moves with the mode
//! into `lattice-diff` at DX.6 and stays green there.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_diff::DiffAlgorithm;
use lattice_host::diff::mode::{DIFF_ELEMENT, DiffConflictMode, DiffMode};
use lattice_host::editor::Editor;
use lattice_host::excommand;
use lattice_keymap::{BindingMode, KeymapLayer, KeymapResolution, LayerHit};
use lattice_mode::{ElementId, ModeId};
use lattice_protocol::chord::KeyChord;
use lattice_protocol::event::Event;

/// Boot a real editor on a scratch document. `Editor::boot` is synchronous,
/// side-effect-free (no LSP attach / blocking I/O), and acquires the
/// process-wide shared runtime — so the diff drainer task spawned during
/// `DiffSubsystem::bind` is live for the end-to-end pin below.
fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

// ── Mode ────────────────────────────────────────────────────────────────────

#[test]
fn diff_mode_registered_at_boot() {
    assert!(
        boot()
            .mode_registry
            .load()
            .is_registered(ModeId::new("diff-mode")),
        "diff-mode must be registered at boot"
    );
}

// ── Ex-commands (the `:diff` family) ────────────────────────────────────────

#[test]
fn diff_ex_commands_resolve_at_boot() {
    let editor = boot();
    // `resolve_command_name_or_alias` mirrors the dispatcher's two-stage
    // resolution (canonical, then the alias table) — the exact path `:diff`
    // takes at the `:` line.
    for name in [
        "diff",
        "diffoff",
        "diffthis",
        "diffsplit",
        "diffget",
        "diffput",
        // CR.4 (2026-06-24): the conflict-session lifecycle ex-commands
        // also resolve at boot. Like the rest of the `:diff` family they
        // desugar to host-boundary Effects (`Effect::DiffAccept` /
        // `DiffReject`); their registration stays in the grammar
        // ex-command registry (the canonical home for every ex-command),
        // while the diff-owned LOGIC lives in lattice-diff (the `do`/`dp`
        // + conflict resolvers). `:diffget`/`:diffput` desugar straight
        // onto the mode resolvers via `diff_*_effect` (CR.1).
        "diff-accept",
        "diff-reject",
        "diff-accept-all",
        "diff-reject-all",
        "describe-diff",
    ] {
        assert!(
            excommand::resolve_command_name_or_alias(&editor.registry.load(), name).is_some(),
            ":{name} must resolve at boot"
        );
    }
}

// ── Modeline element (the `+N ~M` summary) ──────────────────────────────────

#[test]
fn diff_modeline_element_registered_at_boot() {
    let editor = boot();
    let snap = editor.modeline.snapshot();
    assert!(
        snap.registry.get(&ElementId::new(DIFF_ELEMENT)).is_some(),
        "the `+N ~M` diff modeline element must be registered at boot"
    );
}

// ── `do`/`dp` chords on the diff-mode keymap layer ──────────────────────────

/// The diff-mode minor-mode layer hit for `chords`, if the layer binds them.
fn diff_layer_hit(res: &KeymapResolution) -> Option<&LayerHit> {
    res.hits
        .iter()
        .find(|h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == DiffMode::mode_id()))
}

#[test]
fn diff_get_put_chords_bound_on_diff_mode_layer() {
    let editor = boot();
    let active = [DiffMode::mode_id()];
    // CR.6: the diff actions are registered by `lattice_diff::install()` now
    // (not host `ActionIds`), so resolve them by name like the conflict pin.
    for (second, name) in [('o', "action:diff-get"), ('p', "action:diff-put")] {
        let expected = editor
            .registry
            .load()
            .id_by_name(name)
            .unwrap_or_else(|| panic!("`{name}` must be registered at boot"));
        let chords = [KeyChord::char('d'), KeyChord::char(second)];
        let res = editor
            .keymap
            .resolve_trace(BindingMode::Normal, &chords, &active);
        let hit = diff_layer_hit(&res)
            .unwrap_or_else(|| panic!("`d{second}` must bind on the diff-mode layer at boot"));
        assert!(
            hit.active,
            "`d{second}` must be active when diff-mode is active"
        );
        assert_eq!(
            hit.command.command.command, expected,
            "`d{second}` must target the diff action"
        );
    }
}

#[test]
fn diff_chords_inactive_when_diff_mode_not_active() {
    // K.1.c gating: the diff-mode layer is pushed globally at boot, but its
    // bindings only fire on buffers where diff-mode is active. With no active
    // modes, the binding still resolves on the layer but must be INACTIVE — so
    // a non-diff buffer's `d`-operator resolution is untouched.
    let editor = boot();
    for second in ['o', 'p'] {
        let chords = [KeyChord::char('d'), KeyChord::char(second)];
        let res = editor
            .keymap
            .resolve_trace(BindingMode::Normal, &chords, &[]);
        if let Some(hit) = diff_layer_hit(&res) {
            assert!(
                !hit.active,
                "`d{second}` must be inactive when diff-mode is not active (K.1.c gating)"
            );
        }
    }
}

// ── CR.3: conflict-resolution chords on the diff-conflict-mode layer ─────────

/// The diff-conflict-mode minor-mode layer hit for `chords`, if bound.
fn conflict_layer_hit(res: &KeymapResolution) -> Option<&LayerHit> {
    res.hits.iter().find(
        |h| matches!(&h.layer, KeymapLayer::MinorMode(id) if *id == DiffConflictMode::mode_id()),
    )
}

#[test]
fn diff_conflict_chords_bound_on_conflict_mode_layer() {
    let editor = boot();
    let active = [DiffConflictMode::mode_id()];
    // `d2o`/`d3o`/`d2p`/`d3p` are 3-key; `dB` is 2-key (shift folds into
    // the case, so the chord is `d` then `B`).
    let cases: [(&[KeyChord], &str); 5] = [
        (
            &[
                KeyChord::char('d'),
                KeyChord::char('2'),
                KeyChord::char('o'),
            ],
            "action:diff-keep-ours",
        ),
        (
            &[
                KeyChord::char('d'),
                KeyChord::char('3'),
                KeyChord::char('o'),
            ],
            "action:diff-keep-theirs",
        ),
        (
            &[
                KeyChord::char('d'),
                KeyChord::char('2'),
                KeyChord::char('p'),
            ],
            "action:diff-put-ours",
        ),
        (
            &[
                KeyChord::char('d'),
                KeyChord::char('3'),
                KeyChord::char('p'),
            ],
            "action:diff-put-theirs",
        ),
        (
            &[KeyChord::char('d'), KeyChord::char('B')],
            "action:diff-keep-both",
        ),
    ];
    for (chords, name) in cases {
        let expected = editor
            .registry
            .load()
            .id_by_name(name)
            .unwrap_or_else(|| panic!("`{name}` must be registered at boot"));
        let res = editor
            .keymap
            .resolve_trace(BindingMode::Normal, chords, &active);
        let hit = conflict_layer_hit(&res)
            .unwrap_or_else(|| panic!("`{name}` chord must bind on the diff-conflict-mode layer"));
        assert!(
            hit.active,
            "`{name}` must be active when diff-conflict-mode is active"
        );
        assert_eq!(
            hit.command.command.command, expected,
            "`{name}` chord must target its action"
        );
    }
}

#[test]
fn diff_conflict_chords_inactive_when_mode_not_active() {
    // K.1.c gating: the conflict-mode layer is pushed globally at boot, but
    // its bindings only fire on buffers where diff-conflict-mode is active.
    let editor = boot();
    let chords = [KeyChord::char('d'), KeyChord::char('B')];
    let res = editor
        .keymap
        .resolve_trace(BindingMode::Normal, &chords, &[]);
    if let Some(hit) = conflict_layer_hit(&res) {
        assert!(
            !hit.active,
            "`dB` must be inactive when diff-conflict-mode is not active (K.1.c gating)"
        );
    }
}

// ── Subsystem bound to the editor's bus (the boot-wired drain) ──────────────

#[test]
fn diff_subsystem_bound_at_boot() {
    assert!(
        boot().diff_subscription_guard.is_some(),
        "DiffSubsystem must be bound to the event bus at boot (the bind guard is held)"
    );
}

#[tokio::test]
async fn boot_wired_document_closed_drains_to_subsystem() {
    let editor = boot();
    let bid = editor.document_buffer_id;
    let doc_id = editor
        .buffers
        .document_handle(bid)
        .expect("active buffer has a document")
        .id();

    // A session keyed on the active buffer.
    editor
        .diff_subsystem
        .register(bid, DiffAlgorithm::Histogram);
    assert!(
        editor.diff_subsystem.lookup(bid).is_some(),
        "session registered on the active buffer"
    );

    // Publish on the EDITOR's bus (not a fresh one): only the boot-wired
    // `bind(event_bus, BufferRegistryDocumentResolver)` can translate this
    // DocumentId → the active BufferId and drive `note_buffer_closed`.
    editor
        .event_bus
        .publish(Event::DocumentClosed { id: doc_id });

    let deadline = Instant::now() + Duration::from_secs(2);
    while editor.diff_subsystem.lookup(bid).is_some() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        editor.diff_subsystem.lookup(bid).is_none(),
        "boot-wired DocumentClosed drain must drop the session — proves bind() \
         targets the editor's bus + the real registry resolver"
    );
}
