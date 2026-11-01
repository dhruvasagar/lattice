//! LL.5: `:llm` opens a chat buffer, a real (mock-server) reply streams into it
//! with no further key, and two chats sharing one mode keep their own prompts.
//!
//! Drives a real `Editor::boot`, so the whole chain is live: the ex-command,
//! the synthetic-buffer open, `llm-chat-mode`'s activation, the drain, the
//! chord handlers and the request task. The server is a thread on a std socket,
//! so the test needs no runtime features beyond the ones it already has.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use lattice_ai::llm::chat::LlmChatsHandle;
use lattice_ai::llm::config::LlmConfig;
use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_protocol::{KeyChord, parse_chord_sequence};

const REPLY: &str = concat!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"from the mock\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// A server that answers every request with [`REPLY`] after `delay`.
fn serve(delay: Duration) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                let mut seen = 0;
                // Read until the head and the (small) body are in.
                while let Ok(n) = s.read(&mut buf[seen..]) {
                    if n == 0 {
                        break;
                    }
                    seen += n;
                    if buf[..seen].windows(4).any(|w| w == b"\r\n\r\n") && seen > 200 {
                        break;
                    }
                }
                std::thread::sleep(delay);
                let _ = s.write_all(REPLY.as_bytes());
            });
        }
    });
    addr
}

fn config_for(addr: std::net::SocketAddr) -> LlmConfig {
    let text = format!(
        "[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
    );
    LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
}

fn chord(s: &str) -> KeyChord {
    parse_chord_sequence(s).unwrap().into_iter().next().unwrap()
}

fn open_chat(editor: &mut Editor) -> lattice_core::BufferId {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("llm", &mut out);
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

fn text_of(editor: &Editor, id: lattice_core::BufferId) -> String {
    let snap = editor.buffers.document_handle(id).unwrap().snapshot();
    (0..snap.buffer.rope_line_count())
        .map(|n| snap.buffer.line(n).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tick the editor the way its actor does, until `done` or a deadline. Ticks
/// only when `async_landed` fired, so a producer that wakes nothing fails here
/// instead of passing on a timer.
async fn settle_woken(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if done(editor) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let notified = editor.async_landed.clone();
        if tokio::time::timeout(left, notified.notified())
            .await
            .is_err()
        {
            return done(editor);
        }
        editor.run_tick_pending();
    }
}

/// Tick on a timer until `done`: for steps with no wake to wait on (activation).
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

fn chats(editor: &Editor) -> LlmChatsHandle {
    (*editor
        .services
        .get::<LlmChatsHandle>()
        .expect("llm chats service"))
    .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_reaches_the_buffer_without_another_key() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));

    let id = open_chat(&mut editor);
    assert_eq!(editor.buffers.name_of(id).as_deref(), Some("*llm:1*"));
    // Activation seeds the prompt marker.
    assert!(
        settle(&mut editor, |e| text_of(e, id) == "> ").await,
        "{:?}",
        text_of(&editor, id)
    );

    // Type a prompt and send it with <CR>, the way a user does.
    editor.modal = lattice_grammar::ModalState::Insert;
    editor.cursor = Position::new(0, 2);
    editor
        .apply_edit_blocking(Edit::insert(Position::new(0, 2), "say hi".to_string()))
        .expect("the prompt is editable");
    editor.cursor = Position::new(0, 8);
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(chord("<CR>"), &mut partial);

    // From here NO key is dispatched: the reply must arrive on its own.
    let arrived = settle_woken(&mut editor, |e| {
        text_of(e, id).contains("Hello from the mock")
    })
    .await;
    let text = text_of(&editor, id);
    assert!(arrived, "the reply never reached the buffer: {text:?}");
    // The transcript labels the model, the question sits above, the prompt is
    // empty and last.
    assert!(
        text.starts_with("you:\nsay hi\n\nmock-model:\nHello from the mock\n"),
        "{text:?}"
    );
    assert!(text.ends_with("\n> "), "{text:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_is_read_only_and_the_prompt_is_not() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let id = open_chat(&mut editor);
    assert!(settle(&mut editor, |e| text_of(e, id) == "> ").await);

    // Seed a transcript through the model, as a finished exchange would.
    let session = chats(&editor).for_buffer(id).unwrap();
    session.store.mutate(&session.key, |c| {
        c.push_user_text("q");
        c.extend_text(lattice_ai::conversation::Role::Assistant, "a");
    });
    assert!(
        settle_woken(&mut editor, |e| text_of(e, id).contains("mock-model:\na")).await,
        "{:?}",
        text_of(&editor, id)
    );
    let text = text_of(&editor, id);
    let prompt_line = text.lines().count() as u32 - 1;

    // Inside history: refused. In the prompt: allowed.
    let history = editor.apply_edit_blocking(Edit::insert(Position::new(1, 0), "x"));
    assert!(history.is_err(), "history accepted an edit");
    let prompt = editor.apply_edit_blocking(Edit::insert(Position::new(prompt_line, 2), "ok"));
    assert!(prompt.is_ok(), "the prompt refused an edit: {prompt:?}");
}

/// LL.2's leftover (c): one mode instance backs both buffers, and each answers
/// the editable-tail question for itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_chats_sharing_one_mode_have_their_own_editable_tails() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));

    let a = open_chat(&mut editor);
    assert!(settle(&mut editor, |e| text_of(e, a) == "> ").await);
    let b = open_chat(&mut editor);
    assert_ne!(a, b);
    assert!(settle(&mut editor, |e| text_of(e, b) == "> ").await);

    // Give B a transcript, so its prompt moves down. A stays empty.
    let session_b = chats(&editor).for_buffer(b).unwrap();
    session_b
        .store
        .mutate(&session_b.key, |c| c.push_user_text("only in b"));
    assert!(
        settle_woken(&mut editor, |e| text_of(e, b).contains("only in b")).await,
        "{:?}",
        text_of(&editor, b)
    );

    // Same position, (0, 2): the prompt of A, history of B.
    let probe = |editor: &mut Editor, id| {
        editor.do_focus_buffer(id);
        editor.apply_edit_blocking(Edit::insert(Position::new(0, 2), "x"))
    };
    assert!(probe(&mut editor, a).is_ok(), "A's prompt is at line 0");
    assert!(
        probe(&mut editor, b).is_err(),
        "B's line 0 is history, so the shared mode must answer for B, not A"
    );
}

/// Closing the buffer cancels the request in flight and forgets the chat.
///
/// KNOWN GAP: `Editor::forget_buffer` (the `:bd` / `KillBuffer` teardown) never
/// drops the buffer's `active_modes` entry, so no mode guard is dropped on
/// close and `LlmChatGuard::drop` (which releases the chat) never runs. That is
/// a generic host change outside this slice's allowed set; ignored until the
/// controller decides. Un-ignore once close deactivates modes.
#[ignore = "host does not drop mode guards when a buffer is deleted"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_chat_releases_it() {
    let addr = serve(Duration::from_secs(30)); // never answers in time
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let handle = chats(&editor);
    handle.set_config(config_for(addr));
    let id = open_chat(&mut editor);
    assert!(settle(&mut editor, |e| text_of(e, id) == "> ").await);
    let session = handle.for_buffer(id).unwrap();
    handle.submit(&session, "slow".into());
    assert!(session.is_busy());

    assert!(editor.do_buffer_delete(true), "the chat buffer closed");
    assert!(
        settle(&mut editor, |_| handle.for_buffer(id).is_none()
            && !session.is_busy())
        .await,
        "closing the buffer left the chat bound or busy"
    );
}
