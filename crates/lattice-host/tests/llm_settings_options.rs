//! LL.7: the `llm.*` options are ordinary typed options, driven the way a user
//! drives them. `:setlocal` writes the buffer-local layer on the editor thread;
//! the chat driver reads it off-thread through the bus event the host publishes.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use lattice_ai::llm::chat::LlmChatsHandle;
use lattice_ai::llm::config::LlmConfig;
use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;

fn config() -> LlmConfig {
    let text = r#"
        [llm.providers.mock]
        kind = "openai-compatible"
        base_url = "http://127.0.0.1:1/v1"
        model = "provider-model"
        [llm.profiles.writer]
        model = "mock/writer-model"
        temperature = 0.2
    "#;
    LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
}

fn chats(editor: &Editor) -> LlmChatsHandle {
    (*editor.services.get::<LlmChatsHandle>().expect("llm chats")).clone()
}

fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
}

fn message(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

fn open_chat(editor: &mut Editor, line: &str) -> lattice_core::BufferId {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
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
        }
    }
    editor.active_pane_buffer_id()
}

async fn settle(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if done(editor) {
            return true;
        }
        if Instant::now() >= deadline {
            return done(editor);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        editor.run_tick_pending();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setlocal_reaches_the_chat_driver_and_set_query_shows_the_origin() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config());
    let id = open_chat(&mut editor, "llm");
    assert!(settle(&mut editor, |e| chats(e).for_buffer(id).is_some()).await);

    // The option exists and queries like any other.
    ex(&mut editor, "set llm.model?");
    assert!(
        message(&editor).starts_with("llm.model="),
        "{}",
        message(&editor)
    );

    ex(&mut editor, "setlocal llm.model=mock/typed-model");
    // `:set name?` echoes the global value; the buffer-local one, with its
    // origin, is `:setlocal name?`.
    ex(&mut editor, "setlocal llm.model?");
    let q = message(&editor);
    assert!(
        q.contains("typed-model") && q.contains("buffer-local"),
        "{q}"
    );

    // And the off-thread reader sees it, with the layer it came from.
    let seen = settle(&mut editor, |e| {
        chats(e)
            .resolve_for(id)
            .map(|r| r.values.model.as_deref() == Some("mock/typed-model"))
            .unwrap_or(false)
    })
    .await;
    assert!(seen, "the buffer-local value never reached the chat driver");
    let r = chats(&editor).resolve_for(id).unwrap();
    assert_eq!(
        r.origins[&lattice_ai::llm::settings::SettingKey::Model],
        lattice_config::OptionOrigin::BufferLocal
    );

    // A bad value is refused by the shared validator with its message.
    ex(&mut editor, "setlocal llm.temperature=9");
    assert!(message(&editor).contains("0 to 2"), "{}", message(&editor));

    // `name&` clears even an option whose validator rejects a dummy value.
    ex(&mut editor, "setlocal llm.fold.tools=hidden");
    ex(&mut editor, "setlocal llm.fold.tools&");
    let cleared = settle(&mut editor, |e| {
        chats(e).projection_options_for(id).tools == lattice_ai::conversation::FoldDisplay::Closed
    })
    .await;
    assert!(cleared);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_with_a_profile_and_the_profile_switch() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config());

    // An unknown profile is a named error and opens nothing.
    let before = editor.active_pane_buffer_id();
    let opened = open_chat(&mut editor, "llm ghost");
    assert_eq!(opened, before);
    assert!(
        message(&editor).contains("ghost") && message(&editor).contains("writer"),
        "{}",
        message(&editor)
    );

    // A known one starts the chat from it.
    let id = open_chat(&mut editor, "llm writer");
    assert!(settle(&mut editor, |e| chats(e).for_buffer(id).is_some()).await);
    let r = chats(&editor).resolve_for(id).unwrap();
    assert_eq!(r.values.model.as_deref(), Some("mock/writer-model"));
    assert_eq!(r.values.temperature, Some(0.2));

    // Overrides, then the switch clears them and says which.
    ex(&mut editor, "setlocal llm.temperature=0.9");
    ex(&mut editor, "llm-profile writer");
    let m = message(&editor);
    assert!(
        m.contains("cleared") && m.contains("llm.temperature"),
        "{m}"
    );
    let reverted = settle(&mut editor, |e| {
        chats(e)
            .resolve_for(id)
            .map(|r| r.values.temperature == Some(0.2))
            .unwrap_or(false)
    })
    .await;
    assert!(reverted, "the override should be gone");
}
