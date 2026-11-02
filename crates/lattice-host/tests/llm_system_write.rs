//! LL.8: `:llm-system` edits the system prompt in an ordinary buffer. `:w`
//! applies it to the session the buffer was opened from and writes nothing to
//! disk; `:q!` discards. Drives a real `Editor::boot`, so the ex-command, the
//! synthetic-buffer open, the mode's `write_action` and the host's `:w` seam
//! are all live.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_ai::llm::options::LlmSystem;
use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

fn open_system_buffer(editor: &mut Editor) -> lattice_core::BufferId {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("llm-system", &mut out);
    let mut opened = false;
    for effect in out.effects {
        if let lattice_grammar::Effect::OpenSyntheticBuffer {
            name,
            mode_id,
            content,
            cursor,
            activate_minor,
        } = effect
        {
            editor.open_synthetic_buffer_seeded(
                &name,
                &mode_id,
                content.as_deref(),
                cursor,
                activate_minor.as_deref(),
            );
            opened = true;
        }
    }
    assert!(opened, "`:llm-system` opened no buffer");
    editor.active_pane_buffer_id()
}

fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w_applies_the_prompt_to_the_session_and_closes_the_buffer() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let origin = editor.active_pane_buffer_id();
    let system = open_system_buffer(&mut editor);
    assert_ne!(system, origin);
    assert!(
        editor
            .buffers
            .name_of(system)
            .is_some_and(|n| n.starts_with("*llm-system:")),
    );

    editor
        .apply_edit_blocking(Edit::insert(Position::new(0, 0), "You are terse."))
        .unwrap();
    ex(&mut editor, "w");

    assert_eq!(
        editor.active_pane_buffer_id(),
        origin,
        "`:w` returns to the buffer the prompt was opened over"
    );
    assert_eq!(
        editor.resolved_option::<LlmSystem>(origin).as_str(),
        "You are terse.",
        "applied to the session as a buffer-local option"
    );
    assert!(
        !editor.buffers.contains(system),
        "the form closes once it is applied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn q_bang_discards() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let origin = editor.active_pane_buffer_id();
    open_system_buffer(&mut editor);
    editor
        .apply_edit_blocking(Edit::insert(Position::new(0, 0), "never applied"))
        .unwrap();
    ex(&mut editor, "q!");
    assert_eq!(
        editor.resolved_option::<LlmSystem>(origin).as_str(),
        "",
        "a discarded edit changed nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w_with_a_path_still_writes_a_copy_and_does_not_apply() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let origin = editor.active_pane_buffer_id();
    let system = open_system_buffer(&mut editor);
    editor
        .apply_edit_blocking(Edit::insert(Position::new(0, 0), "copy me"))
        .unwrap();
    let path = std::env::temp_dir().join(format!("lattice-ll8-copy-{}.txt", std::process::id()));
    ex(&mut editor, &format!("w {}", path.display()));
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "copy me");
    let _ = std::fs::remove_file(&path);
    assert_eq!(editor.resolved_option::<LlmSystem>(origin).as_str(), "");
    assert!(editor.buffers.contains(system), "a copy is not an apply");
}
