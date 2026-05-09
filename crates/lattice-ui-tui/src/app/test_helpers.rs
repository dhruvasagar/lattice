//! Shared test factories for the per-feature test modules
//! that live alongside their `impl App` blocks. The whole
//! file is gated on `cfg(test)` via the parent's `mod`
//! declaration; nothing here ships in release builds.
//!
//! Convention: every `app/<feature>.rs` test module pulls
//! these in via `use crate::app::test_helpers::*;`.

use lattice_core::Document;
use lattice_grammar::{CommandInvocation, ModalState};
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

/// Build + attach a tree-sitter syntax handle synchronously,
/// matching the slice 3 production seam (one synchronous
/// parse at attach time, then the worker takes over). Sync
/// test paths call this instead of building the handle by
/// hand.
pub(super) fn attach_test_syntax(a: &mut App, lang: lattice_syntax::Lang) {
    let snap = a.document.snapshot();
    let text = snap.buffer.as_string();
    let tv = snap.version;
    let mut s = lattice_syntax::Syntax::for_language(lang)
        .unwrap()
        .expect("syntax registered for lang");
    s.parse_at(&text, tv);
    a.syntax = Some(lattice_syntax::SyntaxHandle::seeded(s));
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

/// Build an `App` over a path-bearing document. Used by
/// path-completion + LSP-attach tests that need a real
/// path to drive the language detection / attach flow.
pub(super) fn app_with_path(
    text: &str,
    viewport: u32,
    path: std::path::PathBuf,
) -> App {
    let doc = lattice_core::DocumentBuilder::default()
        .with_text(text)
        .with_path(path)
        .build();
    let mut a = App::new(doc);
    a.set_viewport_height(viewport);
    a
}

/// Construct an App pre-staged into Command modal with
/// `line` already in the cmdline buffer. Used by every
/// `:`-line completion / dispatch test that needs the
/// cmdline pre-populated.
pub(super) fn app_in_command_mode(line: &str) -> App {
    let mut a = app_with("xx", 10);
    a.modal = ModalState::Command;
    a.command_line = line.into();
    a
}

/// Attach a freshly-built tree-sitter Rust syntax handle
/// over `source`. Convenience for tree-sitter-driven tests
/// that don't care about the language detection path.
pub(super) fn set_rust_syntax(a: &mut App, source: &str) {
    let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .expect("rust syntax");
    s.parse_at(source, 0);
    a.syntax = Some(lattice_syntax::SyntaxHandle::seeded(s));
}

/// Build a fresh per-test temp workspace directory. The
/// returned path is created on disk; the caller can
/// populate it with files / subdirs before constructing an
/// App against it.
/// Build a unique temp directory for tests that touch the
/// filesystem. The path is created on disk; the caller can
/// drop files into it. Caller is responsible for cleanup
/// (or lets the OS reap the temp dir on shutdown -- v1
/// tests don't bother).
pub(super) fn unique_tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = base.join(format!("lattice-tui-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn fresh_path_workspace(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "lattice-{}-{}-{}",
        name,
        std::process::id(),
        n,
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
