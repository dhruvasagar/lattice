//! The llm layer, through each peer's real key path: a chat reply, an
//! answer written into the buffer, and a tool question answered in its
//! popup.
//!
//! The llm tests in the TUI drive its `App`; none ran in the GUI peer, whose
//! dispatch entry and idle wake are its own. Each of these needs a reply to
//! land with no key pressed, which is exactly what a peer gets wrong when it
//! skips a tail step or the off-keystroke drain.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use lattice_ai::llm::chat::LlmChatsHandle;
use lattice_ai::llm::config::LlmConfig;
use serde_json::{Value, json};

use super::completion::{settle, temp_dir};
use super::{PeerUnderTest, boot, press, text};

/// Every request body the mock model received, in order.
type Log = Arc<Mutex<Vec<Value>>>;

fn body_of(s: &mut std::net::TcpStream) -> Option<Value> {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let head = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = head.find("\r\n\r\n") {
            let len = head[..end]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                return serde_json::from_slice(&buf[end + 4..end + 4 + len]).ok();
            }
        }
    }
}

fn sse(events: &[Value]) -> String {
    let mut out = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for e in events {
        out.push_str(&format!("data: {e}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

/// An OpenAI-compatible model on 127.0.0.1. Its first reply makes `calls`
/// (lattice tool name, arguments) when there are any; every other reply
/// says "done".
fn serve(calls: Vec<(&'static str, Value)>) -> (std::net::SocketAddr, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let log: Log = Arc::default();
    let seen = log.clone();
    let calls: Vec<Value> = calls
        .into_iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({"index": i, "id": format!("call_{i}"), "type": "function",
                   "function": {"name": format!("lattice__{name}"), "arguments": args.to_string()}})
        })
        .collect();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let (seen, calls) = (seen.clone(), calls.clone());
            std::thread::spawn(move || {
                let n = {
                    let mut g = seen.lock().unwrap();
                    g.push(body_of(&mut s).unwrap_or_default());
                    g.len()
                };
                let reply = if n == 1 && !calls.is_empty() {
                    sse(&[
                        json!({"choices":[{"index":0,"delta":{"tool_calls":calls}}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                    ])
                } else {
                    sse(&[
                        json!({"choices":[{"index":0,"delta":{"content":"done"}}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    ])
                };
                let _ = s.write_all(reply.as_bytes());
            });
        }
    });
    (addr, log)
}

fn chats<P: PeerUnderTest>(peer: &P) -> LlmChatsHandle {
    (*peer
        .editor()
        .services
        .get::<LlmChatsHandle>()
        .expect("the llm chats"))
    .clone()
}

/// A running peer's ambient runtime: the chat mode activates, and requests
/// run, on tasks spawned there. Entered for as long as this is held.
struct Runtime {
    _guard: tokio::runtime::EnterGuard<'static>,
}

fn runtime() -> Runtime {
    // Leaked: one small runtime per scenario, alive for the test process,
    // so the guard can borrow it for `'static`.
    let rt: &'static tokio::runtime::Runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime"),
    ));
    Runtime { _guard: rt.enter() }
}

/// A peer over `text` whose model is the mock, with `tools` enabled.
fn with_model<P: PeerUnderTest>(
    text: &str,
    calls: Vec<(&'static str, Value)>,
    tools: &str,
) -> (P, Log, Runtime) {
    let rt = runtime();
    let (addr, log) = serve(calls);
    let peer = boot::<P>(text);
    let toml = format!(
        "[llm]\ntools = \"{tools}\"\n[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
    );
    chats(&peer).set_config(LlmConfig::from_table(
        &toml.parse::<toml::Table>().expect("toml"),
    ));
    (peer, log, rt)
}

fn buffer_text<P: PeerUnderTest>(peer: &P, id: lattice_core::BufferId) -> String {
    peer.editor()
        .buffers
        .document_handle(id)
        .map(|d| d.snapshot().buffer.as_string())
        .unwrap_or_default()
}

/// `:llm`, and wait for the chat to be the active buffer with its session.
fn open_chat<P: PeerUnderTest>(peer: &mut P) -> lattice_core::BufferId {
    press(peer, ":llm<CR>");
    let opened = settle(peer, |p| {
        let id = p.editor().active_pane_buffer_id();
        chats(p).for_buffer(id).is_some()
    });
    let id = peer.editor().active_pane_buffer_id();
    assert!(
        opened,
        "no chat is active: {:?} ({})",
        peer.editor().buffers.name_of(id),
        peer.editor()
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    );
    id
}

/// `:llm`, a prompt typed and sent with `<CR>`: the reply streams into the
/// transcript with no further key, through the peer's off-keystroke wake.
pub fn a_chat_reply_lands_without_a_keypress<P: PeerUnderTest>() {
    let (mut peer, log, _rt) = with_model::<P>("scratch\n", Vec::new(), "");
    let chat = open_chat(&mut peer);

    press(&mut peer, "isay hi<CR>");
    assert!(
        settle(&mut peer, |p| buffer_text(p, chat).contains("done")),
        "the reply never showed: {:?}",
        buffer_text(&peer, chat)
    );
    let sent = log.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "one request");
    assert!(
        sent[0].to_string().contains("say hi"),
        "the prompt was sent"
    );
}

/// `<leader>aaj`, a question with `destination=below`: the answer is
/// written under the two lines it was asked about.
pub fn an_ask_writes_its_answer_below<P: PeerUnderTest>() {
    let (mut peer, _log, _rt) =
        with_model::<P>("alpha beta\ngamma delta\nepsilon zeta\n", Vec::new(), "");
    let keys = (*peer
        .editor()
        .services
        .get::<lattice_ai::llm::modes::llm_mode::LlmKeysHandle>()
        .expect("the llm keys"))
    .clone();
    // `llm-mode` binds its chords as it activates. The GUI peer's
    // constructor leaves that activation to the actor's first
    // `async_landed` wake, which fires right after boot; `settle` is that
    // wake. Without it the first keys would arrive before any running peer
    // could take them.
    assert!(
        settle(&mut peer, |_| keys.bound().is_some()),
        "the llm chords were never bound"
    );
    press(&mut peer, "<Space>aaj");
    press(&mut peer, "destination=below explain<CR>");
    assert!(
        settle(&mut peer, |p| text(p).contains("done")),
        "the answer never showed: {:?}",
        text(&peer)
    );
    assert_eq!(text(&peer), "alpha beta\ngamma delta\ndone\nepsilon zeta\n");
}

/// A command the model wants to run asks first: the question opens in its
/// popup on its own, and picking "allow once" with the cursor and `<CR>`
/// runs it and sends the model its output.
pub fn a_tool_question_is_answered_in_its_popup<P: PeerUnderTest>() {
    let dir = temp_dir("llm-exec");
    let (mut peer, log, _rt) = with_model::<P>(
        "scratch\n",
        vec![("run_command", json!({"command": "echo peer-ok"}))],
        "lattice.exec",
    );
    chats(&peer).set_project_root(dir.clone());
    open_chat(&mut peer);
    press(&mut peer, "irun it<CR>");

    let popup = |p: &P| {
        p.editor()
            .popup_buffer
            .map(|b| buffer_text(p, b))
            .unwrap_or_default()
    };
    assert!(
        settle(&mut peer, |p| popup(p).contains("Allow")),
        "the question never opened: {:?}",
        popup(&peer)
    );
    assert!(popup(&peer).contains("$ echo peer-ok"), "{}", popup(&peer));

    // The first option is "allow once".
    press(&mut peer, "jj<CR>");
    assert!(
        settle(&mut peer, |_| log.lock().unwrap().len() >= 2),
        "the result was never sent: {:?}",
        popup(&peer)
    );
    let second = log.lock().unwrap()[1].to_string();
    assert!(
        second.contains("peer-ok"),
        "the model got the output: {second}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
