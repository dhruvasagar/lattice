//! Shared test factories for the per-feature test modules
//! that live alongside their `impl App` blocks. The whole
//! file is gated on `cfg(test)` via the parent's `mod`
//! declaration; nothing here ships in release builds.
//!
//! Convention: every `app/<feature>.rs` test module pulls
//! these in via `use crate::app::test_helpers::*;`.

use lattice_core::Document;
use lattice_grammar::CommandInvocation;
use lattice_grammar::registry::MotionId;

use super::{Action, App};

/// Build an `App` over a fresh in-memory document with the
/// requested visible viewport height. The 95%-case factory
/// for App-level tests.
pub(super) fn app_with(text: &str, viewport: u32) -> App {
    let mut a = App::new(Document::from_text(text));
    a.set_viewport_height(viewport);
    a
}

/// End-to-end key-event harness. Drives a single
/// [`crossterm::event::KeyEvent`] through
/// [`crate::input::translate`] + [`App::apply`] -- the same
/// path the real input loop in `runtime.rs` walks. Catches
/// bugs that live in the seam between translate and apply
/// (count flow through `attach_count` plus dispatcher
/// multiplication, partial_chord state machine across multiple
/// keystrokes, etc.). The translate-layer tests in
/// `input::tests` only check the returned `Action`, and the
/// App-layer tests hand-construct `Action::Invoke(...)`;
/// neither exercises this seam.
pub(super) fn press(app: &mut App, event: crossterm::event::KeyEvent) {
    let ctx = crate::input::TranslateContext {
        modal: app.modal,
        builtins: &app.builtins,
        pending_count: app.pending_count,
        op_count: app.op_count,
        recording_macro: app.macro_recording.is_some(),
        active_buffer: app.active_buffer,
        completion_open: app.completion_state.is_some(),
        chord_capture: app.chord_capture_active(),
        picker_open: app.picker.is_some(),
        insert_completion_open: app.insert_completion.is_some(),
        snippet_active: app.active_snippet.is_some(),
        keymap: &app.keymap,
        partial_chord: &app.partial_chord,
    };
    let action = crate::input::translate(ctx, event);
    app.apply(action);
}

/// Construct an `Action::Invoke` carrying a bare motion
/// (no operator, no count). The 95%-case shorthand for
/// motion tests.
pub(super) fn invoke_motion(id: MotionId) -> Action {
    Action::Invoke(CommandInvocation::of(id.0))
}

/// Convenience: drive a sequence of bare-char keystrokes
/// through [`press`]. Each char becomes a
/// `KeyCode::Char(c)` event with no modifiers -- handy for
/// vim-style chord sequences (`"2dd"`, `"d2w"`, `">>"`).
/// For modifiers or special keys, build a `KeyEvent` and
/// call [`press`] directly.
pub(super) fn press_chars(app: &mut App, keys: &str) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for c in keys.chars() {
        press(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}
