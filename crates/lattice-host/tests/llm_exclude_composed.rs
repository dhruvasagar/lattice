//! `llm.exclude` holds through a composed view. A search-results or other
//! multibuffer view has no file of its own, so a check on the buffer's path
//! passed it, and its lines from an excluded file went to the model. The
//! checks now look through the view to the files it shows: a view showing an
//! excluded file is refused, and one that does not is sent as before.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_ai::llm::chat::LlmChatsHandle;
use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, create_multibuffer_view};
use lattice_runtime::spawn_document;

fn chats(editor: &Editor) -> LlmChatsHandle {
    (*editor.services.get::<LlmChatsHandle>().expect("llm chats")).clone()
}

/// Run an ex line and apply what it returns, as the dispatch tail does.
fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
    for effect in std::mem::take(&mut out.effects) {
        let _ = editor.handle_effect(effect);
    }
}

fn message(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// A view of one excerpt of the file at `path`, as search results show it.
fn view_of(editor: &mut Editor, source: BufferId, path: &str, name: &str) -> BufferId {
    let registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();
    let doc = spawn_document(
        source,
        lattice_core::DocumentBuilder::default()
            .with_text("TOKEN=hunter2\nmore\n")
            .with_path(std::path::PathBuf::from(path))
            .build(),
        registry.clone(),
    );
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(source, Arc::new(doc) as Arc<dyn lattice_runtime::Document>);
    create_multibuffer_view(
        editor,
        sources,
        vec![Excerpt::new(source, 0, 1)],
        lattice_multibuffer::MultibufferViewConfig {
            name: Some(name.into()),
            flags: BufferFlags::default(),
            fold_grouping: lattice_multibuffer::FoldGrouping::SourceFile,
        },
        registry,
        None,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_showing_an_excluded_file_is_not_sent() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let secret = view_of(&mut editor, BufferId(401), "/proj/.env", "*test:secret*");
    let plain = view_of(
        &mut editor,
        BufferId(402),
        "/proj/notes.txt",
        "*test:notes*",
    );

    editor.activate_buffer(secret);
    ex(&mut editor, "llm-context-add");
    let said = message(&editor);
    assert!(
        said.contains(".env") && said.contains("llm.exclude"),
        "the view's file is named: {said}"
    );
    assert!(chats(&editor).context_of(secret).is_empty());

    ex(&mut editor, "llm-ask explain");
    let said = message(&editor);
    assert!(said.contains("nothing was sent"), "{said}");

    editor.activate_buffer(plain);
    ex(&mut editor, "llm-context-add");
    assert_eq!(
        chats(&editor).context_of(plain).len(),
        1,
        "a view of other files attaches: {}",
        message(&editor)
    );
}
