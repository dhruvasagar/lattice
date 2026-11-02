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
    open_system_line(editor, "llm-system")
}

fn open_system_line(editor: &mut Editor, line: &str) -> lattice_core::BufferId {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
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

/// `:wq` / `:x` are `Many[SaveBuffer, QuitEditor{Pane}]`. The write action
/// closes the form, so a quit that followed would be spent on the user's own
/// buffer. The write ends the sequence: the quit is spent on the form.
async fn write_and_quit_keeps_the_target(line: &str) {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let origin = editor.active_pane_buffer_id();
    let system = open_system_buffer(&mut editor);
    editor
        .apply_edit_blocking(Edit::insert(Position::new(0, 0), "Be brief."))
        .unwrap();
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
    assert!(
        !out.effects
            .iter()
            .any(|e| matches!(e, lattice_grammar::Effect::QuitEditor { .. })),
        "`:{line}` must not quit once the form has closed itself"
    );
    assert_eq!(editor.active_pane_buffer_id(), origin);
    assert!(!editor.buffers.contains(system));
    assert_eq!(
        editor.resolved_option::<LlmSystem>(origin).as_str(),
        "Be brief."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wq_applies_and_keeps_the_target_pane() {
    write_and_quit_keeps_the_target("wq").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn x_applies_and_keeps_the_target_pane() {
    write_and_quit_keeps_the_target("x").await;
}

/// `:'<,'>llm-system` from an actual Visual selection seeds the form with the
/// selected lines. The host resolves `Range::Selection` to lines from the last
/// Visual selection, as it does for `:'<,'>narrow`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_visual_selection_seeds_the_system_prompt() {
    use lattice_host::chord::KeyChord;
    let mut editor = Editor::boot(CoreDocument::from_text("zero\none\ntwo\nthree\n"));
    let mut partial = Vec::new();
    for c in "jVj".chars() {
        let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
    let _ = editor.dispatch_chord(
        KeyChord::special(lattice_protocol::chord::SpecialKey::Esc),
        &mut partial,
    );
    let system = open_system_line(&mut editor, "'<,'>llm-system");
    assert_eq!(
        editor
            .buffers
            .name_of(system)
            .map(|n| n.starts_with("*llm-system:")),
        Some(true),
        "the selection opened the form instead of erroring"
    );
    let text = editor.document.snapshot().buffer.as_string();
    assert_eq!(text.trim_end(), "one\ntwo");
}

/// A buffer in `llm-system-mode` whose name does not say what it edits has
/// nowhere to apply to. `:w` says so, rather than doing nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w_in_a_form_with_an_unparsable_name_says_why() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.open_synthetic_buffer_seeded("*not-a-form*", "llm-system-mode", Some("x"), None, None);
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("w", &mut out);
    let msg = editor.last_message.clone().expect("`:w` said something");
    assert_eq!(format!("{:?}", msg.level), "Error", "{}", msg.text);
    assert!(msg.text.contains("llm"), "{}", msg.text);
}
