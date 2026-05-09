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
use lattice_protocol::Event;

use crate::buffers::BufferKind;
use crate::help::HelpBuffer;

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

/// Subscribe to every event the App publishes; the caller
/// drains via the returned receiver. Used by event-bus and
/// option-cascade tests that need to assert specific events
/// fired (e.g. `Event::DocumentChanged`,
/// `Event::OptionChanged`, `Event::ModalModeChanged`).
pub(super) fn subscribe_all_events(
    a: &App,
) -> tokio::sync::mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    a.event_bus.subscribe(
        lattice_runtime::EventFilter::any(),
        lattice_runtime::SubscriptionTarget::Channel(tx),
    );
    rx
}

/// Wrap a freshly-built [`HelpBuffer`] as the active buffer
/// the way the App's `:describe-*` paths do. Shared across
/// every test that wants help-mode setup.
pub(super) fn install_help(a: &mut App, h: HelpBuffer) {
    a.help_buffer = Some(h);
    a.active_buffer = BufferKind::Help;
}

/// Drive a complete ex-command line: enter cmdline, type
/// the contents, submit. The 95%-case shorthand for tests
/// that exercise an ex command end-to-end (substitute,
/// `:noh`, `:g/v`, `:reg`, `:marks`, etc.).
pub(super) fn submit_ex(a: &mut App, line: &str) {
    a.apply(Action::EnterCommandLine);
    for c in line.chars() {
        a.apply(Action::CommandLineAppend(c));
    }
    a.apply(Action::CommandLineSubmit);
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
