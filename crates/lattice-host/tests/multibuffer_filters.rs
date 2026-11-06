//! MF.1 — `:multibuffer-keep` / `:multibuffer-reject` /
//! `:multibuffer-clear-filters`, driven through a booted `Editor`.
//!
//! The substrate behaviour (streaming, refresh, sliding) is pinned in
//! `lattice-multibuffer`'s own tests. What only a real boot proves is the
//! wiring: the `:` line parses the pattern, `Effect::InvokeCommand` reaches
//! the action, the host's boot harvest registered `MultibufferMode`'s
//! global handler for it, and the handler finds the active view through
//! the service registry. Any one of those missing is a command that
//! silently does nothing — so each test asserts on the VIEW, not on the
//! command returning.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, create_multibuffer_view};
use lattice_runtime::spawn_document;

/// A view over two sources, one excerpt per line — the shape `:search`
/// produces with no context lines.
fn boot_with_results() -> (Editor, BufferId, Arc<dyn lattice_runtime::Document>) {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let registry = editor.registry.clone();
    let src = |id: u32, text: &str| -> Arc<dyn lattice_runtime::Document> {
        Arc::new(spawn_document(
            BufferId(id),
            CoreDocument::from_text(text),
            registry.clone(),
        ))
    };
    let parser = src(201, "fn parse() {} // TODO\nlet x = 1;\nTODO: errors\n");
    let tests = src(202, "// TODO: more tests\nTODO cover parse\n");

    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(BufferId(201), parser.clone());
    sources.insert(BufferId(202), tests);
    let excerpts = vec![
        Excerpt::new(BufferId(201), 0, 0),
        Excerpt::new(BufferId(201), 2, 2),
        Excerpt::new(BufferId(202), 0, 0),
        Excerpt::new(BufferId(202), 1, 1),
    ];
    let registry_for_view = editor.registry.clone();
    let view = create_multibuffer_view(
        &mut editor,
        sources,
        excerpts,
        Some("*search:TODO*".into()),
        BufferFlags::default(),
        registry_for_view,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    );
    editor.activate_document(view);
    (editor, view, parser)
}

fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
}

fn echoed(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

fn shown(editor: &Editor, view: BufferId) -> String {
    editor
        .buffers
        .document_handle(view)
        .expect("buffer is registered")
        .snapshot()
        .buffer
        .as_string()
}

#[test]
fn keep_shows_only_the_matching_results() {
    let (mut editor, view, _) = boot_with_results();
    ex(&mut editor, "multibuffer-keep /parse/");
    assert_eq!(
        shown(&editor, view),
        "fn parse() {} // TODO\nTODO cover parse\n"
    );
    assert_eq!(echoed(&editor), "2 of 4 shown");
}

#[test]
fn reject_hides_the_matching_results_and_filters_stack() {
    let (mut editor, view, _) = boot_with_results();
    ex(&mut editor, r"multibuffer-reject /^\/\//");
    assert_eq!(
        shown(&editor, view),
        "fn parse() {} // TODO\nTODO: errors\nTODO cover parse\n"
    );
    ex(&mut editor, "multibuffer-keep errors");
    assert_eq!(shown(&editor, view), "TODO: errors\n");
    assert_eq!(echoed(&editor), "1 of 4 shown");
}

#[test]
fn clearing_brings_every_result_back() {
    let (mut editor, view, _) = boot_with_results();
    let before = shown(&editor, view);
    ex(&mut editor, "multibuffer-keep /nothing-matches-this/");
    assert_eq!(shown(&editor, view), "");
    ex(&mut editor, "multibuffer-clear-filters");
    assert_eq!(shown(&editor, view), before);
    assert_eq!(echoed(&editor), "all 4 shown");
}

/// The reason this is not `:v/pat/d`: that, in an editable view, deletes
/// the lines from the files. A filter must leave every source as it was.
#[test]
fn a_filter_never_edits_a_source() {
    let (mut editor, _, parser) = boot_with_results();
    let before = parser.snapshot().buffer.as_string();
    ex(&mut editor, "multibuffer-reject TODO");
    assert_eq!(parser.snapshot().buffer.as_string(), before);
    assert!(!parser.snapshot().dirty, "the source must not be dirtied");
}

#[test]
fn a_bad_pattern_is_refused_and_changes_nothing() {
    let (mut editor, view, _) = boot_with_results();
    let before = shown(&editor, view);
    ex(&mut editor, "multibuffer-keep /(/");
    assert_eq!(shown(&editor, view), before);
    let msg = echoed(&editor);
    assert!(msg.contains("bad pattern"), "got {msg:?}");
}

#[test]
fn outside_a_multibuffer_the_command_says_so() {
    let mut editor = Editor::boot(CoreDocument::from_text("plain\n"));
    let active = editor.active_buffer_id();
    ex(&mut editor, "multibuffer-keep plain");
    let msg = echoed(&editor);
    assert!(msg.contains("not a multibuffer view"), "got {msg:?}");
    assert_eq!(
        shown(&editor, active),
        "plain\n",
        "a filter command must not touch an ordinary buffer"
    );
}
