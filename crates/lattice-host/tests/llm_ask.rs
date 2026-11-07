//! LL.10b: `:llm-ask` sends a range to a new dedicated buffer that opens
//! beside the source, which keeps the focus; the answer streams in with no
//! further key; the new chat starts from a copy of the source's settings and
//! stays unlinked from it.
//!
//! The operator front-end (`<leader>aa{motion}`) shares the same tail and is
//! driven through the real keystroke path in `lattice-ui-tui`
//! (`app/llm_ask.rs`), because `Editor::dispatch_chord` does not compose an
//! operator with a motion.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_ai::llm::chat::LlmChatsHandle;
use lattice_ai::llm::config::LlmConfig;
use lattice_ai::llm::settings::SettingKey;
use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

const REPLY: &str = concat!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"It defines b.\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// A server that records each request body and answers with [`REPLY`].
fn serve() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                let mut got = 0;
                loop {
                    let Ok(n) = s.read(&mut buf[got..]) else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let len = text[..head_end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if got >= head_end + 4 + len {
                            seen.lock().unwrap().push(text[head_end + 4..].to_string());
                            break;
                        }
                    }
                }
                let _ = s.write_all(REPLY.as_bytes());
            });
        }
    });
    (addr, bodies)
}

fn config_for(addr: std::net::SocketAddr) -> LlmConfig {
    let text = format!(
        "[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
    );
    LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
}

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

fn text_of(editor: &Editor, id: lattice_core::BufferId) -> String {
    let snap = editor.buffers.document_handle(id).unwrap().snapshot();
    snap.text().to_string()
}

/// The chat buffer the ask opened: the one pane not showing the source.
fn chat_buffer(editor: &Editor) -> Option<lattice_core::BufferId> {
    let active = editor.pane_tree.active_index();
    editor
        .pane_tree
        .leaves()
        .iter()
        .enumerate()
        .find(|(i, _)| *i != active)
        .map(|(_, p)| p.buffer_id)
}

/// Tick on a timer until `done`: for activation, which has no wake to wait on.
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

/// Tick only when `async_landed` fires, so a reply that wakes nothing fails.
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

const SOURCE: &str = "fn a() {}\nfn b() {}\nfn c() {}\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ask_opens_beside_the_source_and_the_answer_arrives_with_focus_unmoved() {
    let (addr, bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    editor.set_cursor_clamped(Position::new(2, 3));
    let source = editor.active_buffer_id();

    ex(&mut editor, "1,2llm-ask what does this do");

    assert_eq!(editor.pane_tree.leaves().len(), 2, "the answer gets a pane");
    assert_eq!(
        editor.active_buffer_id(),
        source,
        "the source keeps the focus"
    );
    assert_eq!(editor.cursor, Position::new(2, 3), "and the caret");
    let chat = chat_buffer(&editor).expect("a chat pane");
    let name = editor.buffers.name_of(chat).unwrap();
    assert!(name.starts_with("*llm:") && name.ends_with(":1*"), "{name}");

    // From here no key is dispatched: the answer must arrive on its own.
    let arrived = settle_woken(&mut editor, |e| text_of(e, chat).contains("It defines b.")).await;
    let text = text_of(&editor, chat);
    assert!(arrived, "the answer never arrived: {text:?}");
    assert!(
        text.contains(":1-2\n```text\nfn a() {}\nfn b() {}\n```\nwhat does this do\n"),
        "the sent text is the first turn: {text:?}"
    );
    assert_eq!(editor.active_buffer_id(), source, "still the source");

    let body = bodies.lock().unwrap().first().cloned().unwrap_or_default();
    assert!(body.contains("fn a() {}\\nfn b() {}"), "{body}");
    assert!(!body.contains("fn c()"), "only the range is sent: {body}");
    assert!(body.contains("what does this do"), "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_range_the_current_line_is_asked_about() {
    let (addr, bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    editor.set_cursor_clamped(Position::new(2, 0));
    ex(&mut editor, "llm-ask why");
    let got = settle(&mut editor, |_| !bodies.lock().unwrap().is_empty()).await;
    assert!(got, "nothing was sent");
    let body = bodies.lock().unwrap()[0].clone();
    assert!(
        body.contains("fn c() {}") && !body.contains("fn b()"),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_ask_replaces_the_first_answer_in_its_pane() {
    let (addr, _bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    let source = editor.active_buffer_id();
    ex(&mut editor, "1llm-ask one");
    let first = chat_buffer(&editor).unwrap();
    ex(&mut editor, "2llm-ask two");
    let second = chat_buffer(&editor).unwrap();
    assert_eq!(editor.pane_tree.leaves().len(), 2, "no split per request");
    assert_ne!(first, second, "a new buffer per request");
    assert!(editor.buffers.name_of(first).is_some(), "the first is kept");
    assert_eq!(editor.active_buffer_id(), source);
}

/// A bad request argument is named, and nothing is opened or sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_argument_is_named_and_nothing_opens() {
    let (addr, bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    ex(&mut editor, "1llm-ask temperature=hot explain");
    assert_eq!(editor.pane_tree.leaves().len(), 1);
    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(msg.contains("temperature"), "{msg}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(bodies.lock().unwrap().is_empty());
}

/// Design §4: settings are copied when the ask's buffer is made, and nothing
/// stays linked, in either direction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_starts_from_a_copy_of_the_source_settings_and_stays_unlinked() {
    let (addr, _bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    let source = editor.active_buffer_id();
    ex(&mut editor, "setlocal llm.temperature=0.3");
    assert!(
        settle(&mut editor, |e| {
            chats(e)
                .session_overrides(source)
                .get(&SettingKey::Temperature)
                .map(String::as_str)
                == Some("0.3")
        })
        .await,
        "the source's override never reached the driver"
    );

    ex(&mut editor, "1llm-ask hi");
    let chat = chat_buffer(&editor).unwrap();
    assert!(settle(&mut editor, |e| chats(e).for_buffer(chat).is_some()).await);
    let temperature = |e: &Editor, b| {
        chats(e)
            .resolve_for(b)
            .ok()
            .and_then(|r| r.values.temperature)
    };
    assert_eq!(temperature(&editor, chat), Some(0.3), "copied at creation");

    // The source changes: the answer's session does not follow.
    ex(&mut editor, "setlocal llm.temperature=0.9");
    assert!(settle(&mut editor, |e| temperature(e, source) == Some(0.9)).await);
    assert_eq!(
        temperature(&editor, chat),
        Some(0.3),
        "not linked to the source"
    );

    // The answer's session changes: the source does not follow.
    editor.do_focus_buffer(chat);
    ex(&mut editor, "setlocal llm.temperature=0.1");
    assert!(settle(&mut editor, |e| temperature(e, chat) == Some(0.1)).await);
    assert_eq!(
        temperature(&editor, source),
        Some(0.9),
        "not linked back either"
    );
}

// ── LL.10c ──────────────────────────────────────────────────────────────────

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lattice-llm-ask-{name}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An editor over `text`, as the file `path` (no disk read).
fn boot_at(path: &std::path::Path, text: &str) -> Editor {
    let doc = lattice_core::DocumentBuilder::default()
        .with_text(text)
        .with_path(path.to_path_buf())
        .build();
    Editor::boot(doc)
}

fn message(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// Design §5: `llm.exclude` refuses `.env` by default, by name, and nothing
/// is opened or sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_excluded_file_is_refused_by_name_and_nothing_is_sent() {
    let (addr, bodies) = serve();
    let dir = scratch_dir("env");
    let mut editor = boot_at(&dir.join(".env"), "API_TOKEN=hunter2\n");
    chats(&editor).set_config(config_for(addr));

    ex(&mut editor, "llm-ask what is this");
    let msg = message(&editor);
    assert!(msg.contains(".env") && msg.contains("llm.exclude"), "{msg}");
    assert_eq!(editor.pane_tree.leaves().len(), 1, "nothing opened");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(bodies.lock().unwrap().is_empty(), "nothing sent");
}

/// The value replaces the default list, and a bad glob is refused by `:set`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_exclude_replaces_the_default_and_refuses_a_bad_glob() {
    let (addr, bodies) = serve();
    let dir = scratch_dir("custom");

    let mut notes = boot_at(&dir.join("notes.txt"), "private notes\n");
    chats(&notes).set_config(config_for(addr));
    ex(&mut notes, "set llm.exclude=[");
    assert!(
        message(&notes).contains("not a glob"),
        "{}",
        message(&notes)
    );
    ex(&mut notes, "set llm.exclude=*.txt");
    ex(&mut notes, "llm-ask hi");
    assert!(message(&notes).contains("notes.txt"), "{}", message(&notes));

    let mut env = boot_at(&dir.join(".env"), "EDITOR=lattice\n");
    chats(&env).set_config(config_for(addr));
    ex(&mut env, "set llm.exclude=*.txt");
    ex(&mut env, "llm-ask hi");
    let sent = settle(&mut env, |_| !bodies.lock().unwrap().is_empty()).await;
    assert!(sent, "`.env` is not excluded once the list is replaced");
    assert!(!bodies.lock().unwrap()[0].contains("private notes"));
}

/// Design §5 edge cases: an oversized source asks first, and nothing is sent
/// before the answer. The yes half is the TUI test (the dialog is the
/// renderer's).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_source_asks_before_anything_is_sent() {
    let (addr, bodies) = serve();
    let big: String = (0..2000)
        .map(|i| format!("let line_{i} = {i};\n"))
        .collect();
    assert!(big.len() > lattice_ai::llm::operators::ASK_CONFIRM_BYTES);
    let mut editor = Editor::boot(CoreDocument::from_text(&big));
    chats(&editor).set_config(config_for(addr));

    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("%llm-ask summarise", &mut out);
    let confirm = out.effects.iter().find_map(|e| match e {
        lattice_grammar::effect::Effect::Confirm {
            prompt, yes_action, ..
        } => Some((prompt.clone(), yes_action.clone())),
        _ => None,
    });
    let (prompt, yes) = confirm.expect("a confirm, not a send");
    assert!(
        prompt.contains("KB") && prompt.contains("tokens"),
        "{prompt}"
    );
    assert_eq!(yes, lattice_ai::llm::operators::ACTION_ASK_CONFIRMED);
    assert_eq!(editor.pane_tree.leaves().len(), 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(bodies.lock().unwrap().is_empty(), "nothing before the yes");
}

/// Design §3: `destination=<buffer>` continues that chat instead of opening a
/// new one, and the model sees the earlier exchange.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destination_continues_an_open_chat() {
    let (addr, bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    ex(&mut editor, "1llm-ask one");
    let first = chat_buffer(&editor).unwrap();
    let name = editor.buffers.name_of(first).unwrap();
    assert!(settle_woken(&mut editor, |e| text_of(e, first).contains("It defines b.")).await);

    ex(&mut editor, &format!("2llm-ask destination={name} two"));
    assert_eq!(
        chat_buffer(&editor),
        Some(first),
        "the same chat, in its pane"
    );
    assert_eq!(editor.pane_tree.leaves().len(), 2);
    let got = settle(&mut editor, |_| bodies.lock().unwrap().len() >= 2).await;
    assert!(got, "the second ask was never sent");
    let second = bodies.lock().unwrap()[1].clone();
    assert!(
        second.contains("fn a() {}") && second.contains("fn b() {}"),
        "{second}"
    );
    assert!(
        second.contains("It defines b."),
        "the earlier answer is history: {second}"
    );
    let names: Vec<String> = editor
        .buffers
        .sorted_ids()
        .into_iter()
        .filter_map(|id| editor.buffers.name_of(id))
        .filter(|n| n.starts_with("*llm:"))
        .collect();
    assert_eq!(names.len(), 1, "no second chat: {names:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_destination_naming_no_chat_is_refused() {
    let (addr, bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    ex(&mut editor, "1llm-ask destination=*llm:nope* hi");
    assert!(
        message(&editor).contains("not an open chat"),
        "{}",
        message(&editor)
    );
    assert_eq!(editor.pane_tree.leaves().len(), 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(bodies.lock().unwrap().is_empty());
}

/// A destination kind not built yet still answers, in a new buffer, and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_destination_not_built_yet_opens_a_new_buffer_and_says_so() {
    let (addr, _bodies) = serve();
    let mut editor = Editor::boot(CoreDocument::from_text(SOURCE));
    chats(&editor).set_config(config_for(addr));
    ex(&mut editor, "1llm-ask destination=review hi");
    assert_eq!(editor.pane_tree.leaves().len(), 2);
    assert!(
        message(&editor).contains("not built yet"),
        "{}",
        message(&editor)
    );
}
