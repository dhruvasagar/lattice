//! Shared test factories for the per-feature test modules
//! that live alongside their `impl App` blocks. The whole
//! file is gated on `cfg(test)` via the parent's `mod`
//! declaration; nothing here ships in release builds.
//!
//! Convention: every `app/<feature>.rs` test module pulls
//! these in via `use crate::app::test_helpers::*;`.

use lattice_core::Document;
use lattice_grammar::registry::MotionId;
use lattice_grammar::{CommandInvocation, ModalState};
use lattice_protocol::Event;

use crate::buffers::BufferKind;
use crate::help::HelpContent;

use super::{Action, App};

/// Build an `App` over a fresh in-memory document with the
/// requested visible viewport height. The 95%-case factory
/// for App-level tests.
pub(crate) fn app_with(text: &str, viewport: u32) -> App {
    // Hermetic boot: `App::new` → `Editor::boot` async-loads the real
    // `~/.config/lattice` (init.rs / on-disk plugins). On a developer box that
    // can enable modes (e.g. auto-pair) on the test buffer mid-run, flaking any
    // behavior assertion under load. Suppress auto-discovery process-wide before
    // the first boot spawns it (synchronous gate → set before App::new).
    lattice_host::disable_autoload();
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
    let ad = app.ad();
    let ctx = crate::input::TranslateContext {
        modal: ad.modal,
        builtins: &app.editor.builtins,
        pending_count: ad.pending_count,
        op_count: ad.op_count,
        recording_macro: ad.macro_recording,
        active_buffer: ad.buffer_kind,
        completion_open: ad.completion_open,
        chord_capture: app.chord_capture_active(),
        picker_open: ad.picker_open,
        insert_completion_open: app.completion_popup_active(),
        snippet_active: ad.snippet_active,
        terminal_insert_active: ad.terminal_insert_active,
        terminal_esc_exits: ad.terminal_esc_exits,
        terminal_app_cursor_keys: ad.terminal_app_cursor_keys,
        terminal_insert_exit_pending: ad.terminal_insert_exit_pending,
        terminal_visual_active: ad.terminal_visual_active,
        keymap: &app.editor.keymap,
        partial_chord: &app.editor.partial_chord,
        // D.5.b: in test, drive from the editor's snapshot —
        // mirrors the runtime/gpui path so tests cover the
        // per-buffer chord gating.
        active_minor_modes: &app
            .editor
            .active_modes
            .get(&app.editor.document_buffer_id)
            .map(|am| am.minors().to_vec())
            .unwrap_or_default(),
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
    let snap = a.editor.document.snapshot();
    let text = snap.buffer.as_string();
    let tv = snap.version;
    let mut s = lattice_syntax::Syntax::for_language(lang)
        .unwrap()
        .expect("syntax registered for lang");
    s.parse_at(&text, tv);
    a.editor.syntax = Some(lattice_syntax::SyntaxHandle::seeded(s));
}

/// Subscribe to every event the App publishes; the caller
/// drains via the returned receiver. Used by event-bus and
/// option-cascade tests that need to assert specific events
/// fired (e.g. `Event::DocumentChanged`,
/// `Event::OptionChanged`, `Event::ModalModeChanged`).
pub(super) fn subscribe_all_events(a: &App) -> tokio::sync::mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    a.editor.event_bus.subscribe(
        lattice_runtime::EventFilter::any(),
        lattice_runtime::SubscriptionTarget::Channel(tx),
    );
    rx
}

/// Wrap a freshly-built [`HelpContent`] as the active buffer
/// the way the App's `:describe-*` paths do. Shared across
/// every test that wants help-mode setup. Splits the content
/// into the slim buffer (popup hot-path slot) + parsed
/// metadata (seeded into `buffer_locals`).
pub(super) fn install_help(a: &mut App, h: HelpContent) {
    // PU.1a: help content is an actor-backed Document. Mirror the
    // production `open_popup` path via `register_help_document`
    // (seeds content + metadata) with the same unlisted/hidden flags.
    let id = a.editor.register_help_document(
        h,
        crate::buffers::BufferFlags {
            listed: false,
            hidden: true,
            ephemeral: false,
        },
    );
    a.editor.popup_buffer = Some(id);
    // PU-A.1a: mirror open_popup's pre-flip capture. Production captures
    // `prev_pane_for_popup` from the underlying pane BEFORE overwriting
    // cursor/scroll and flipping `active_buffer` to Help, so dismiss can
    // restore the buffer the user came from. Without it this helper left
    // `active_buffer == Help` with `prev == None` — a state production
    // never produces — which the old `dismiss` masked by hardcoding
    // Document.
    let (prev_buf, prev_id) = {
        let active = a.editor.pane_tree.active();
        (active.buffer, active.buffer_id)
    };
    a.editor.prev_pane_for_popup = Some(lattice_host::state::PrevPaneState {
        buffer: prev_buf,
        buffer_id: prev_id,
        cursor: a.editor.cursor,
        scroll: a.editor.scroll,
        modal: a.editor.modal,
    });
    a.editor.popup_scroll = 0;
    a.editor.popup_cursor = lattice_protocol::position::Position::ZERO;
    a.editor.cursor = lattice_protocol::position::Position::ZERO;
    a.editor.scroll = 0;
    a.editor.active_buffer = BufferKind::Help;
    // 2026-05-26: mirror production's `open_popup` mode activation.
    // Without this, `Editor::run_invocation`'s active-mode runner
    // lookup finds no `help-mode` minor on the popup buffer and
    // falls through to `run_document_invocation`, which routes
    // motions to the doc instead of the popup.
    let _ = a
        .editor
        .activate_major_for_buffer_kind(id, BufferKind::Help);
    // 3c.atomic.E: this helper mocks the `:describe-*` path
    // without going through dispatch, so the direct
    // `active_buffer` write isn't reflected in `render_state`
    // by default. Renderer-side reads of `ad().buffer_kind` --
    // e.g. `help_popup_inner_height` -- depend on the
    // publication, so call it explicitly here. Production code
    // reaches Help via the popup-open path inside dispatch,
    // which already publishes at the tail.
    a.editor.publish_render_state();
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
pub(super) fn app_with_path(text: &str, viewport: u32, path: std::path::PathBuf) -> App {
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
    a.editor.modal = ModalState::Command;
    a.editor.command_line = line.into();
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
    a.editor.syntax = Some(lattice_syntax::SyntaxHandle::seeded(s));
}

/// Build a fresh per-test temp workspace directory. The
/// returned path is created on disk; the caller can
/// populate it with files / subdirs before constructing an
/// App against it.
/// Build a process-unique workspace directory under
/// `lattice-config-test-<pid>-<name>`. Used by
/// `load_persistent_config` tests that need a real
/// workspace with `.lattice/config.toml` on disk.
pub(super) fn fresh_workspace(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "lattice-config-test-{}-{}-{}",
        std::process::id(),
        name,
        n,
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Write `contents` to `<workspace>/.lattice/config.toml`.
/// Pairs with `fresh_workspace`. Tests typically build
/// the workspace then drop a TOML config in via this.
pub(super) fn write_workspace_config(workspace: &std::path::Path, contents: &str) {
    let dir = workspace.join(".lattice");
    std::fs::create_dir_all(&dir).expect("create .lattice dir");
    std::fs::write(dir.join("config.toml"), contents).expect("write config.toml");
}

/// Seed the App's LSP diagnostics layer with diagnostics
/// at the given buffer lines. Each entry gets a synthetic
/// diagnostic at the line's origin column with severity
/// `Error`. Used by `:diagnostics` / `next_diagnostic` /
/// `prev_diagnostic` tests.
pub(super) fn seed_diags_at_lines(app: &mut App, lines: &[u32]) {
    use std::str::FromStr;
    let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
    app.editor
        .buffer_uris
        .insert(app.editor.document_buffer_id, uri.clone());
    let diags: Vec<lattice_lsp::Diagnostic> = lines
        .iter()
        .map(|line| lattice_lsp::Diagnostic {
            range: lattice_lsp::LspRange {
                start: lattice_lsp::LspPosition {
                    line: *line,
                    character: 0,
                },
                end: lattice_lsp::LspPosition {
                    line: *line,
                    character: 1,
                },
            },
            severity: Some(lattice_lsp::DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: None,
            message: format!("err on line {line}"),
            related_information: None,
            tags: None,
            data: None,
        })
        .collect();
    app.editor
        .lsp_diagnostics
        .apply(lattice_lsp::DiagnosticEvent {
            server_id: std::sync::Arc::from("rust"),
            uri,
            version: None,
            diagnostics: std::sync::Arc::from(diags.into_boxed_slice()),
        });
    // M.6.3: navigation (`:diag-next` / `:diag-prev`) and the
    // render gates check `lsp-diagnostics-mode`. Activate the
    // umbrella so the cascade brings every sub-mode up; tests
    // can override per-test if they want a specific sub-mode
    // off.
    if !app.lsp_mode_enabled_for(app.editor.document_buffer_id) {
        app.toggle_mode_by_name("lsp-mode");
    }
}

/// Write a file to a process-unique temp path. Used by
/// `:e` / file-open tests that need a real file on disk.
pub(super) fn write_temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("lattice-test-{}-{name}", std::process::id()));
    std::fs::write(&path, content).expect("write temp file");
    path
}

/// Pre-populate the App's insert-completion popup with a
/// single candidate carrying `top_text` against `query`.
/// Used by accept / dismiss / commit-char tests that don't
/// need to drive the full LSP / sync-source pipeline.
pub(super) fn open_popup_with_top_text(a: &mut App, query: &str, top_text: &str) {
    let mut state = lattice_completion::InsertCompletionState::open(
        lattice_completion::CompletionTrigger::Manual,
        a.editor.cursor,
        a.editor.cursor,
        query.to_string(),
    );
    let raw =
        lattice_completion::RawCandidate::plain(top_text, lattice_completion::CandidateKind::Plain);
    state.raw.push(raw.clone());
    state
        .rendered
        .push(lattice_completion::RenderedCandidate::from_scored(
            lattice_completion::ScoredCandidate {
                raw,
                score: lattice_completion::MatchScore(800),
                match_ranges: Vec::new(),
            },
        ));
    a.editor.insert_completion = Some(state);
}

/// Insert a snippet into the App's per-language snippet
/// registry. Used by snippet-popup / snippet-expand tests.
pub(super) fn install_snippet(a: &mut App, language: &str, name: &str, prefix: &str, body: &str) {
    let parsed = lattice_snippet::parse::parse(body).unwrap();
    // CSM.5: snippet_registry is `Arc<ArcSwap<...>>`. Clone the
    // current snapshot, mutate the copy, store. Cheap for test
    // scale; production reload swaps the inner the same way.
    let mut next = (**a.editor.snippet_registry.load()).clone();
    next.insert(
        language,
        lattice_snippet::Snippet {
            name: name.into(),
            prefixes: vec![prefix.into()],
            body: parsed,
            description: None,
            scope: String::new(),
        },
    );
    a.editor.snippet_registry.store(std::sync::Arc::new(next));
}

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
    let path = std::env::temp_dir().join(format!("lattice-{}-{}-{}", name, std::process::id(), n,));
    std::fs::create_dir_all(&path).unwrap();
    path
}
