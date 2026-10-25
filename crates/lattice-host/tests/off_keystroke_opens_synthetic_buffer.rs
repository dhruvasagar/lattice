//! An off-keystroke drain that asks for a synthetic buffer opens it.
//!
//! `apply_effect_host` routes `Effect::OpenSyntheticBuffer` to the
//! renderer tail, which a peer drains only after a dispatch. A tick
//! callback (and so every `boot.inbound` handler) runs with no dispatch
//! behind it, so the request was produced and dropped: the subsystem ran
//! and nothing appeared. `absorb_off_keystroke_effects` now opens it
//! host-side, as it already did for popups (WK.10).
//!
//! The test drives the drain with no key in between, which is the way it
//! fails: a test that dispatched an action first would have had a peer
//! tail to drain and passed on the broken version.

use lattice_core::Document as CoreDocument;
use lattice_grammar::Effect;
use lattice_host::editor::Editor;

#[test]
fn a_tick_callback_opening_a_synthetic_buffer_puts_it_in_front() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let ticks = editor
        .services
        .get::<lattice_mode::TickCallbackRegistryHandle>()
        .expect("tick-callback registry at boot");
    let mut asked = false;
    let _registration = ticks.register(Box::new(move || {
        if std::mem::replace(&mut asked, true) {
            return Vec::new();
        }
        vec![Effect::OpenSyntheticBuffer {
            name: "*off-keystroke*".into(),
            mode_id: "plugin-output-mode".into(),
            content: Some("landed\n".into()),
            cursor: None,
            activate_minor: None,
        }]
    }));

    editor.run_tick_pending();

    let front = editor.active_pane_buffer_id();
    assert_eq!(
        editor.buffers.name_of(front).as_deref(),
        Some("*off-keystroke*"),
        "the buffer a tick callback asked for is not in front"
    );
    let text = editor
        .buffers
        .document_handle(front)
        .expect("a document")
        .snapshot()
        .buffer
        .line(0)
        .unwrap_or_default();
    assert_eq!(text, "landed", "seeded with the content it was asked for");
}
