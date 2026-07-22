//! Regression: `:compile` must create the `*compilation*` buffer
//! host-side without panicking.
//!
//! The original CM.1 wiring routed buffer creation through the
//! `&self` `BufferStore::ensure_named_document`, which could only
//! *find* an existing buffer (activating a mode needs `&mut Editor`)
//! and panicked otherwise. So the very first `:compile` crashed the
//! editor. The fix made buffer creation the mode's responsibility
//! through the `&mut`-backed `ModeActivator::ensure_named_document`
//! seam: `start_compilation` calls it to create + activate
//! `*compilation*` (establishing the drain) before running the service.
//!
//! The CM.1 unit tests exercised the service in isolation over a bare
//! `EventBus` and never drove the real dispatch arm, so they missed
//! this. This test drives `apply_app_effect(CompileRun)` end-to-end.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::AppEffect;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;

#[test]
fn compile_run_creates_the_compilation_buffer_without_panicking() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    assert!(
        editor.buffers.by_name("*compilation*").is_none(),
        "no *compilation* buffer before the first :compile"
    );

    let mut out = DispatchOutcome::default();
    // `echo` is a cheap, always-available command; the assertion is
    // about buffer creation + no panic, not the streamed output.
    editor.apply_app_effect(
        AppEffect::CompileRun {
            cmdline: Some("echo hello".to_string()),
        },
        &mut out,
    );

    assert!(
        editor.buffers.by_name("*compilation*").is_some(),
        "`:compile` must create the *compilation* buffer host-side \
         (regression: previously panicked via the BufferStore stub)"
    );
}

#[test]
fn recompile_reuses_the_same_buffer() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let mut out = DispatchOutcome::default();

    editor.apply_app_effect(
        AppEffect::CompileRun {
            cmdline: Some("echo one".to_string()),
        },
        &mut out,
    );
    let first = editor.buffers.by_name("*compilation*");
    assert!(first.is_some());

    // `:recompile` (no cmdline) must not create a second buffer or panic.
    editor.apply_app_effect(AppEffect::CompileRun { cmdline: None }, &mut out);
    assert_eq!(
        editor.buffers.by_name("*compilation*"),
        first,
        "recompile reuses the existing *compilation* buffer"
    );
}
