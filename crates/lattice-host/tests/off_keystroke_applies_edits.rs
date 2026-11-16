//! An off-keystroke drain whose effect is an edit applies it.
//!
//! `apply_effect_host` turns `Effect::ApplyEdit` into an
//! `Action::ApplyEdit` queued on `out.next_actions`, which a keystroke's
//! dispatch runs before it returns. The off-keystroke drains (a tick
//! callback, an idle gate, a code-action fallback) went through
//! `absorb_off_keystroke_effects`, which ran the deferred effects and never
//! the deferred actions, so the edit was produced and dropped: `cri`'s
//! native inline, run as RF.3b's fallback when no server answered, did the
//! work, echoed nothing, and changed nothing.
//!
//! The test drives the drain with no key in between, which is the way it
//! fails: a test that dispatched an action first would have drained the
//! queue and passed on the broken version.

use lattice_core::Document as CoreDocument;
use lattice_grammar::Effect;
use lattice_host::editor::Editor;
use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::position::{Position, Range};

#[test]
fn a_tick_callback_editing_the_buffer_lands_without_a_key() {
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    let target = editor.active_buffer_id();
    let ticks = editor
        .services
        .get::<lattice_mode::TickCallbackRegistryHandle>()
        .expect("tick-callback registry at boot");
    let mut asked = false;
    let _registration = ticks.register(Box::new(move || {
        if std::mem::replace(&mut asked, true) {
            return Vec::new();
        }
        vec![Effect::ApplyEdit {
            target,
            edit: Edit {
                range: Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 5),
                },
                kind: EditKind::Replace {
                    text: "landed".into(),
                },
            },
            cursor: Some(Position::new(0, 3)),
        }]
    }));

    editor.run_tick_pending();

    assert_eq!(
        editor.document.snapshot().buffer.as_string(),
        "landed\n",
        "the edit a tick callback asked for is not applied"
    );
    assert_eq!(
        editor.cursor,
        Position::new(0, 3),
        "nor the cursor it asked for"
    );
}
