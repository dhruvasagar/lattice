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
            target: lattice_grammar::RunTarget::Compilation,
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
            target: lattice_grammar::RunTarget::Compilation,
        },
        &mut out,
    );
    let first = editor.buffers.by_name("*compilation*");
    assert!(first.is_some());

    // `:recompile` (no cmdline) must not create a second buffer or panic.
    editor.apply_app_effect(
        AppEffect::CompileRun {
            cmdline: None,
            target: lattice_grammar::RunTarget::Compilation,
        },
        &mut out,
    );
    assert_eq!(
        editor.buffers.by_name("*compilation*"),
        first,
        "recompile reuses the existing *compilation* buffer"
    );
}

/// The cues a pipe strips from a diagnostic have to come back *on screen*,
/// which is a longer road than the classifier: reader thread → drain task →
/// the highlight store → the editor's tick → the buffer's highlight local.
/// The unit tests on each stage passed while the road was cut in two places
/// — the drain asked the service registry for the wrong type, and the store
/// kept only the latest of several splices — so this drives a real boot and
/// reads the far end.
///
/// The row matters as much as the style: a span one line out is the failure
/// a streamed log produces, so each styled row is checked against the text
/// of the line it sits on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_diagnostic_reaches_the_buffer_styled_on_its_own_line() {
    use lattice_cells::Style;

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let mut out = DispatchOutcome::default();
    // Unstyled lines on both sides of each diagnostic, several of them, so
    // the publishes are spread over more than one flush.
    editor.apply_app_effect(
        AppEffect::CompileRun {
            cmdline: Some(
                "printf 'building\\nwarning: unused\\nstill building\\n\
                 error[E0308]: mismatched types\\n --> src/main.rs:3:17\\ndone\\n' 1>&2"
                    .to_string(),
            ),
            target: lattice_grammar::RunTarget::Compilation,
        },
        &mut out,
    );
    let id = editor.buffers.by_name("*compilation*").unwrap();

    let text_of = |e: &Editor| {
        e.buffers
            .document_handle(id)
            .unwrap()
            .snapshot()
            .buffer
            .as_string()
    };
    let spans_of = |e: &Editor| {
        e.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<lattice_host::modes::ExtraHighlights>())
            .map(|h| h.0.clone())
            .unwrap_or_default()
    };
    let label = |spans: &[Vec<lattice_cells::StyledSpan>], line: usize| {
        spans
            .get(line)
            .and_then(|l| l.first())
            .map(|s| (s.start, s.end, s.style))
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        editor.run_tick_pending();
        let text = text_of(&editor);
        let spans = spans_of(&editor);
        let lines: Vec<&str> = text.lines().collect();
        let row = |needle: &str| lines.iter().position(|l| l.starts_with(needle));
        if text.contains("Compilation")
            && let (Some(warning), Some(error), Some(arrow)) =
                (row("warning:"), row("error[E0308]"), row(" -->"))
            && label(&spans, error).is_some()
        {
            assert_eq!(
                label(&spans, warning),
                Some((0, "warning".len(), Style::DiagnosticWarning))
            );
            assert_eq!(
                label(&spans, error),
                Some((0, "error[E0308]".len(), Style::DiagnosticError))
            );
            assert_eq!(label(&spans, arrow), Some((1, 4, Style::Comment)));
            // The jumpable row and how its text reads are two axes with
            // two carriers: the row index says *which* line `<CR>` jumps
            // from (the renderers tint it), the span says where the
            // location is. The index used to carry the path's byte range
            // as well, and only the TUI painted it.
            let jumpable = editor
                .compilation_location_lines
                .get(&id)
                .map(|l| l.to_vec())
                .unwrap_or_default();
            if jumpable.is_empty() {
                // The index rides its own bus; it may land a tick later.
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                continue;
            }
            // Row 0 is the echoed command, which quotes the location.
            assert_eq!(jumpable, vec![0, arrow as u32]);
            let link = spans[arrow].iter().find(|s| s.style == Style::Link);
            assert_eq!(
                link.map(|s| &lines[arrow][s.start..s.end]),
                Some("src/main.rs:3:17")
            );
            for (i, line) in lines.iter().enumerate() {
                if !line.starts_with(['w', 'e', ' ']) {
                    assert!(
                        spans.get(i).is_none_or(|l| l.is_empty()),
                        "line {i} ({line:?}) has nothing to style, got {:?}",
                        spans[i]
                    );
                }
            }
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the diagnostic never arrived styled.\ntext: {text:?}\nspans: {spans:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Run the tick until `buffer`'s text satisfies `done`, dispatching nothing.
async fn settle_text(
    editor: &mut Editor,
    buffer: lattice_core::BufferId,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        editor.run_tick_pending();
        let text = editor
            .buffers
            .document_handle(buffer)
            .unwrap()
            .snapshot()
            .buffer
            .as_string();
        if done(&text) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the output never settled: {text:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `:!cmd` and `:compile` are the same machinery put to two uses, and the
/// uses must not leak. A one-off command in between a build and its
/// `:recompile` must not become the command that is recompiled, must not
/// overwrite the build's output, and must not replace the error list the
/// build filled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_command_does_not_disturb_the_build() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let mut out = DispatchOutcome::default();

    editor.execute_ex_line("compile printf 'main.c:3:5: error: bad\\n' 1>&2", &mut out);
    let build = editor.buffers.by_name("*compilation*").unwrap();
    let built = settle_text(&mut editor, build, |t| t.contains("Compilation")).await;
    assert!(built.contains("main.c:3:5: error: bad"));
    let errors_after_build = editor.error_list().len();
    assert_eq!(
        errors_after_build, 1,
        "the build's diagnostic is the error list"
    );

    editor.execute_ex_line("!printf 'other.c:9:1: error: not a build\\n'", &mut out);
    let shell = editor.buffers.by_name("*shell-command*").unwrap();
    assert_ne!(shell, build, "its own buffer");
    let shown = settle_text(&mut editor, shell, |t| t.contains("Command finished")).await;
    assert!(shown.contains("other.c:9:1: error: not a build"));

    // The build's buffer and error list are as the build left them.
    assert_eq!(settle_text(&mut editor, build, |_| true).await, built);
    assert_eq!(editor.error_list().len(), errors_after_build);

    // Nothing in the shell buffer is parsed: no styled spans, no jumpable
    // rows, no gutter marks — though the line would earn all three in a
    // build.
    let spans = editor
        .buffer_locals
        .get(&shell)
        .and_then(|l| l.get::<lattice_host::modes::ExtraHighlights>())
        .map(|h| h.0.iter().flatten().count())
        .unwrap_or(0);
    assert_eq!(spans, 0);
    assert!(!editor.compilation_location_lines.contains_key(&shell));

    // And `:recompile` still means the build.
    editor.execute_ex_line("recompile", &mut out);
    let rebuilt = settle_text(&mut editor, build, |t| t.contains("Compilation")).await;
    assert!(rebuilt.starts_with("$ printf 'main.c:3:5"), "{rebuilt:?}");
}
