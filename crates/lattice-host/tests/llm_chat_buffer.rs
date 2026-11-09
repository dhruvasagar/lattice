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
use lattice_grammar::{CommandInvocation, Target, args::Args, range::Range};
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
        text.starts_with("you:\n\nsay hi\n\nmock-model:\n\nHello from the mock\n"),
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
        settle_woken(&mut editor, |e| text_of(e, id).contains("mock-model:\n\na")).await,
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

/// Closing the buffer (`:bd`) drops its mode guards, which releases the chat and
/// cancels the request in flight: the turn ends (it was blocked on a server that
/// never answers, so only a cancellation can end it).
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

/// Seed a finished exchange and return the chat's buffer, session and prompt line.
async fn chat_with_history(
    editor: &mut Editor,
) -> (
    lattice_core::BufferId,
    std::sync::Arc<lattice_ai::llm::chat::ChatSession>,
    u32,
) {
    let id = open_chat(editor);
    assert!(settle(editor, |e| text_of(e, id) == "> ").await);
    let session = chats(editor).for_buffer(id).unwrap();
    session.store.mutate(&session.key, |c| {
        c.push_user_text("q");
        c.extend_text(lattice_ai::conversation::Role::Assistant, "a");
    });
    assert!(settle_woken(editor, |e| text_of(e, id).contains("mock-model:\n\na")).await);
    let prompt_line = text_of(editor, id).lines().count() as u32 - 1;
    (id, session, prompt_line)
}

/// The operators as the grammar runs them (`dispatch_invocation`), not the
/// `apply_edit_blocking` char path, which skips the invocation runner.
fn run_x(editor: &mut Editor, at: Position) {
    editor.modal = lattice_grammar::ModalState::Normal;
    editor.cursor = at;
    let x = CommandInvocation::of(editor.builtins.delete.0)
        .with_target(Target::Motion(editor.builtins.char_right, Args::None));
    editor.dispatch_invocation(x, &mut DispatchOutcome::default());
}

fn run_dd(editor: &mut Editor, at: Position) {
    editor.modal = lattice_grammar::ModalState::Normal;
    editor.cursor = at;
    let dd = CommandInvocation::of(editor.builtins.delete.0)
        .with_target(Target::Range(Range::CurrentLine));
    editor.dispatch_invocation(dd, &mut DispatchOutcome::default());
}

/// `p` is an Action: it takes the keymap path, which routes the invocation
/// through the mode's runner like any other.
fn run_p(editor: &mut Editor, at: Position) {
    editor.modal = lattice_grammar::ModalState::Normal;
    editor.cursor = at;
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(chord("p"), &mut partial);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn x_dd_and_p_are_refused_in_history_and_allowed_in_the_prompt() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let (id, _session, prompt_line) = chat_with_history(&mut editor).await;
    editor
        .apply_edit_blocking(Edit::insert(Position::new(prompt_line, 2), "abc"))
        .expect("the prompt is editable");
    let before = text_of(&editor, id);

    // History: each operator is refused and the buffer is untouched.
    run_x(&mut editor, Position::new(1, 0));
    assert_eq!(text_of(&editor, id), before, "x edited history");
    run_dd(&mut editor, Position::new(1, 0));
    assert_eq!(text_of(&editor, id), before, "dd edited history");

    // The prompt: x edits and fills the unnamed register ...
    run_x(&mut editor, Position::new(prompt_line, 2));
    let after_x = text_of(&editor, id);
    assert!(after_x.ends_with("> bc"), "x in the prompt: {after_x:?}");

    // ... which `p` in history must not paste, and in the prompt may.
    run_p(&mut editor, Position::new(1, 0));
    assert_eq!(text_of(&editor, id), after_x, "p edited history");
    run_p(&mut editor, Position::new(prompt_line, 2));
    let after_p = text_of(&editor, id);
    assert!(
        after_p.starts_with(before.split("> abc").next().unwrap()) && after_p != after_x,
        "p in the prompt did nothing: {after_p:?}"
    );

    // dd in the prompt line is allowed too.
    run_dd(&mut editor, Position::new(prompt_line, 2));
    assert_ne!(
        text_of(&editor, id),
        after_p,
        "dd in the prompt did nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_caret_stays_in_the_prompt_while_text_arrives_above() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let (id, session, prompt_line) = chat_with_history(&mut editor).await;
    // Type "abc" into the prompt the way a user does, so the document's own
    // caret is there too.
    editor.modal = lattice_grammar::ModalState::Insert;
    editor.cursor = Position::new(prompt_line, 2);
    let mut partial: Vec<KeyChord> = Vec::new();
    for key in ["a", "b", "c"] {
        editor.dispatch_chord(chord(key), &mut partial);
    }
    assert!(text_of(&editor, id).ends_with("> abc"));

    // A reply grows the transcript above the prompt, with no key dispatched.
    // Only the reply grows: a new user turn would be a submission, and the
    // drain clears the prompt for those.
    session.store.mutate(&session.key, |c| {
        c.extend_text(
            lattice_ai::conversation::Role::Assistant,
            "\nstreamed in\nmore",
        );
    });
    assert!(settle_woken(&mut editor, |e| text_of(e, id).contains("streamed in")).await);

    let text = text_of(&editor, id);
    assert!(
        text.lines().count() as u32 - 1 > prompt_line,
        "the prompt moved"
    );
    assert!(text.ends_with("> abc"), "the typed text survived: {text:?}");

    // The next key lands in the prompt, not in the history the caret was
    // above when the text arrived.
    // `dispatch_fused` is the production entry, and the one that adopts the
    // document's caret (an owner write moved it) before the key runs.
    editor.dispatch_fused(
        lattice_host::action::Action::Insert("d".to_string()),
        lattice_core::BufferKind::Document,
        false,
        false,
    );
    let text = text_of(&editor, id);
    assert!(
        text.ends_with("> abcd"),
        "the caret left the prompt: {text:?}"
    );
    assert!(text.contains("streamed in\nmore\n"), "{text:?}");
}

/// An open chat, even one mid-request with an unsent prompt typed, is never
/// "unsaved changes" to `:qa`. The request in flight is what holds it up
/// (LL.14d's quit guard, with its own message), and once that stops, `:qa`
/// quits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chat_buffer_does_not_block_quit_all() {
    let addr = serve(Duration::from_secs(30));
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let handle = chats(&editor);
    handle.set_config(config_for(addr));
    let id = open_chat(&mut editor);
    assert!(settle(&mut editor, |e| text_of(e, id) == "> ").await);
    let session = handle.for_buffer(id).unwrap();
    handle.submit(&session, "slow".into());
    editor
        .apply_edit_blocking(Edit::insert(
            Position::new(text_of(&editor, id).lines().count() as u32 - 1, 2),
            "unsent",
        ))
        .expect("the prompt is editable");

    // What `:qa` runs (`Effect::QuitEditor` -> `do_quit`), dirty guard included.
    editor.do_quit(false, lattice_grammar::QuitScope::All);
    let said = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(!editor.should_quit, "a running request holds up `:qa`");
    assert!(said.contains("request is running"), "{said}");
    assert!(
        !said.contains("no write since"),
        "the chat is not unsaved: {said}"
    );

    // Stopped, and the turn over: what the guard reads (internal state, so
    // polled).
    handle.interrupt(&session);
    assert!(settle(&mut editor, |_| handle.in_flight() == 0).await);
    editor.do_quit(false, lattice_grammar::QuitScope::All);
    assert!(
        editor.should_quit,
        "`:qa` was blocked by the chat buffer: {:?}",
        editor.last_message.as_ref().map(|m| &m.text)
    );
}

/// LL.14d: the host records which buffers the current tab shows, on every
/// publish, and the store answers from that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_store_knows_which_buffers_are_displayed() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let source = editor.active_pane_buffer_id();
    let store = editor
        .services
        .get::<lattice_mode::BufferStoreHandle>()
        .unwrap();
    editor.publish_render_state();
    assert!(store.is_displayed(source));
    let chat = open_chat(&mut editor);
    editor.publish_render_state();
    assert!(
        store.is_displayed(chat),
        "the chat replaced the source in the pane"
    );
    assert!(
        !store.is_displayed(source),
        "and the source is no longer shown"
    );
    editor.do_split_pane(lattice_core::ui::pane::SplitOrientation::Vertical);
    editor.publish_render_state();
    editor.handle_effect(lattice_grammar::Effect::FocusBuffer(source.0));
    editor.publish_render_state();
    assert!(
        store.is_displayed(source) && store.is_displayed(chat),
        "one in each pane"
    );
    editor.do_only_pane();
    editor.publish_render_state();
    assert!(store.is_displayed(source));
    assert!(!store.is_displayed(chat), "closing its window hides it");
}

// ---------------------------------------------------------------------------
// LL.8: the menu, driven by keys
// ---------------------------------------------------------------------------

/// Apply the effects a peer applies after a chord: the ones the host leaves to
/// the renderer, here for a menu and a prompt. Everything else was applied host
/// side already.
fn apply_peer_effects(editor: &mut Editor, out: DispatchOutcome) {
    for effect in out.effects {
        match effect {
            lattice_grammar::Effect::OpenTransient { source, args } => {
                let _ = editor.open_named_transient(source, args);
            }
            lattice_grammar::Effect::OpenPrompt {
                prompt,
                initial,
                on_submit_action,
                buffer_name,
            } => {
                let _ = editor.open_prompt_line(prompt, initial, on_submit_action, buffer_name);
            }
            _ => {}
        }
    }
}

fn press_applying(editor: &mut Editor, keys: &str) {
    for c in parse_chord_sequence(keys).unwrap() {
        let mut partial: Vec<KeyChord> = Vec::new();
        let (_, out) = editor.dispatch_chord_with_outcome(c, &mut partial);
        apply_peer_effects(editor, out);
    }
}

/// `<M-m>` opens the menu, `m` asks for a model, a typed value and `<CR>` set
/// it for the session: the whole chain a user takes, through `dispatch_chord`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_menu_sets_the_model_from_the_keyboard() {
    use lattice_ai::llm::options::LlmModel;
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let id = open_chat(&mut editor);
    assert!(settle(&mut editor, |e| text_of(e, id) == "> ").await);
    assert_eq!(editor.resolved_option::<LlmModel>(id).as_str(), "");

    press_applying(&mut editor, "<M-m>");
    assert!(
        editor
            .picker
            .as_ref()
            .is_some_and(|p| p.transient.is_some()),
        "`<M-m>` opens the LLM menu"
    );
    press_applying(&mut editor, "m");
    assert!(
        matches!(editor.modal, lattice_grammar::ModalState::Prompt),
        "the model row asks for a value"
    );
    press_applying(&mut editor, "mock/other");
    press_applying(&mut editor, "<CR>");

    assert_eq!(
        editor.resolved_option::<LlmModel>(id).as_str(),
        "mock/other",
        "the typed value became the session's model"
    );
}

/// LL.19: `gb` on a past question opens a new chat holding the history before
/// it, with the question already in the prompt, and leaves this chat alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gb_forks_a_chat_with_the_question_in_its_prompt() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let (id, _session, _) = chat_with_history(&mut editor).await;
    let before = text_of(&editor, id);
    let q_line = before.lines().position(|l| l == "q").unwrap() as u32;

    editor.modal = lattice_grammar::ModalState::Normal;
    editor.cursor = Position::new(q_line, 0);
    let mut partial: Vec<KeyChord> = Vec::new();
    for c in parse_chord_sequence("gb").unwrap() {
        editor.dispatch_chord(c, &mut partial);
    }
    let fork = editor.active_pane_buffer_id();
    assert_ne!(fork, id, "the fork opens in its own buffer");
    // Forked at the first question: no history, the question to edit.
    assert!(
        settle(&mut editor, |e| text_of(e, fork) == "> q").await,
        "{:?}",
        text_of(&editor, fork)
    );
    assert_eq!(text_of(&editor, id), before, "the original is untouched");
    assert!(
        chats(&editor).for_buffer(fork).is_some(),
        "a chat, bound to its buffer"
    );
}

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

/// LL.20: `:llm-save`, `:llm-export` and `:llm-resume` through real files.
/// Each result arrives off the editor thread; the test waits on the wake, so
/// a result that needed a keystroke to show fails here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_export_and_resume_go_through_real_files() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let (id, _session, _) = chat_with_history(&mut editor).await;
    let dir = std::env::temp_dir().join(format!(
        "lattice-ll20-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (json, md, org) = (
        dir.join("chat.json"),
        dir.join("chat.md"),
        dir.join("chat.org"),
    );

    ex(&mut editor, &format!("llm-save {}", json.display()));
    assert!(
        settle_woken(&mut editor, |e| message(e).contains("saved")).await,
        "{}",
        message(&editor)
    );
    assert!(
        std::fs::read_to_string(&json)
            .unwrap()
            .contains("lattice-llm-session")
    );
    // An existing file is kept without `!`, and replaced with it.
    ex(&mut editor, &format!("llm-save {}", json.display()));
    assert!(
        settle_woken(&mut editor, |e| message(e)
            .contains("exists (add ! to overwrite)"))
        .await
    );
    ex(&mut editor, &format!("llm-save! {}", json.display()));
    assert!(
        settle_woken(&mut editor, |e| message(e).contains("saved")).await,
        "{}",
        message(&editor)
    );

    ex(&mut editor, &format!("llm-export {}", md.display()));
    assert!(
        settle_woken(&mut editor, |e| message(e).contains("chat.md")).await,
        "{}",
        message(&editor)
    );
    let written = std::fs::read_to_string(&md).unwrap();
    assert!(
        written.starts_with("you:\n\nq\n") && written.contains("mock-model:\n\na"),
        "{written}"
    );
    ex(&mut editor, &format!("llm-export {}", org.display()));
    assert!(
        settle_woken(&mut editor, |e| message(e).contains("chat.org")).await,
        "{}",
        message(&editor)
    );
    let written = std::fs::read_to_string(&org).unwrap();
    assert!(
        written.starts_with("#+title: *llm:") && written.contains("\n* you\nq\n"),
        "{written}"
    );

    ex(&mut editor, &format!("llm-resume {}", json.display()));
    assert!(
        settle_woken(&mut editor, |e| e.active_pane_buffer_id() != id).await,
        "{}",
        message(&editor)
    );
    let resumed = editor.active_pane_buffer_id();
    let original = text_of(&editor, id);
    assert!(
        settle(&mut editor, |e| text_of(e, resumed) == original).await,
        "{:?}",
        text_of(&editor, resumed)
    );
    assert!(message(&editor).contains("resumed"), "{}", message(&editor));
    assert!(
        chats(&editor).for_buffer(resumed).is_some(),
        "a chat, bound to its buffer"
    );

    // A file that is not a session is refused, and nothing opens.
    ex(&mut editor, &format!("llm-resume {}", md.display()));
    assert!(
        settle_woken(&mut editor, |e| message(e)
            .contains("not a lattice llm session file"))
        .await
    );
    assert_eq!(editor.active_pane_buffer_id(), resumed);
    let _ = std::fs::remove_dir_all(&dir);
}
