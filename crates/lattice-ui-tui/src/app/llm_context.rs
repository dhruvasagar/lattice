//! LL.15: context (`<leader>ac`, `:llm-context-add`, `:llm-context`) and
//! `:llm-inspect`, end to end through the real keystroke path against a mock
//! model that records every request it receives.
//!
//! What a request carried is read from the recorded body, so each test checks
//! what was actually sent, not what the editor believed it sent.

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use crate::app::App;
    use crate::app::test_helpers::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lattice_ai::llm::chat::LlmChatsHandle;
    use lattice_ai::llm::config::LlmConfig;
    use lattice_ai::llm::context::shown_body;

    const TEXT: &str = "alpha beta\ngamma delta\nepsilon zeta\n";
    const SECRET: &str = "sk-ctx-secret-123";

    /// One received request: its head (request line and headers) and body.
    #[derive(Clone)]
    struct Received {
        head: String,
        body: Vec<u8>,
    }

    type Log = Arc<Mutex<Vec<Received>>>;

    fn read_request(s: &mut std::net::TcpStream) -> Option<Received> {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 65536];
        loop {
            let n = s.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + len {
                    return Some(Received {
                        head: text[..head_end].to_string(),
                        body: buf[head_end + 4..head_end + 4 + len].to_vec(),
                    });
                }
            }
        }
    }

    /// A model that answers "ok" at once and records what it was sent.
    fn serve() -> (std::net::SocketAddr, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Log = Arc::default();
        let seen = log.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let seen = seen.clone();
                std::thread::spawn(move || {
                    if let Some(r) = read_request(&mut s) {
                        seen.lock().unwrap().push(r);
                    }
                    let d = serde_json::json!({"choices":[{"index":0,"delta":{"content":"ok"}}]});
                    let _ = s.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {d}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
                        )
                        .as_bytes(),
                    );
                });
            }
        });
        (addr, log)
    }

    /// An editor over `TEXT` whose model is the mock, with a key from a
    /// command, so a test can check where the key does and does not go.
    fn app() -> (App, Log) {
        let (addr, log) = serve();
        let a = app_with(TEXT, 10);
        let toml = format!(
            "[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\napi_key_command = \"echo {SECRET}\"\n"
        );
        chats(&a).set_config(LlmConfig::from_table(&toml.parse::<toml::Table>().unwrap()));
        (a, log)
    }

    fn chats(a: &App) -> LlmChatsHandle {
        (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone()
    }

    fn key(a: &mut App, code: KeyCode) {
        press(a, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ex(a: &mut App, cmd: &str) {
        press_chars(a, &format!(":{cmd}"));
        key(a, KeyCode::Enter);
    }

    fn message(a: &App) -> String {
        a.editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    fn buffer_text(a: &App, name: &str) -> String {
        a.editor
            .buffers
            .by_name(name)
            .and_then(|b| a.editor.buffers.document_handle(b))
            .map(|h| h.text())
            .unwrap_or_default()
    }

    fn received(log: &Log) -> Vec<Received> {
        log.lock().unwrap().clone()
    }

    /// The last user message of a recorded body.
    fn last_user(r: &Received) -> String {
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// `<leader>aaw` on the caret's word, answered in the echo area.
    fn ask_word(a: &mut App) {
        press_chars(a, " aaw");
        press_chars(a, "destination=echo explain");
        key(a, KeyCode::Enter);
    }

    fn temp_file(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f
    }

    /// Attached text is read when the request goes, not when it was
    /// attached: a span follows the edit made inside it, and a file is read
    /// as it is on disk at that moment. `:llm-inspect` from the buffer then
    /// shows exactly what the model received.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attached_text_is_read_when_the_request_is_sent() {
        let (mut a, log) = app();
        // Lines 2-3, then an edit inside them.
        press_chars(&mut a, "G");
        press_chars(&mut a, " ack");
        assert!(message(&a).contains("attached"), "{}", message(&a));
        press_chars(&mut a, "A EDITED");
        key(&mut a, KeyCode::Esc);
        let file = temp_file("FILE-BEFORE\n");
        ex(
            &mut a,
            &format!("llm-context-add {}", file.path().display()),
        );
        std::fs::write(file.path(), "FILE-AFTER\n").unwrap();

        press_chars(&mut a, "gg");
        ask_word(&mut a);
        assert!(settle(&mut a, |_| received(&log).len() == 1).await);
        let sent = last_user(&received(&log)[0]);
        assert!(sent.starts_with("<context>\n"), "{sent}");
        assert!(
            sent.contains("gamma delta\nepsilon zeta EDITED\n"),
            "{sent}"
        );
        assert!(
            sent.contains("FILE-AFTER") && !sent.contains("FILE-BEFORE"),
            "{sent}"
        );
        assert!(
            sent.contains("[no name]:2-3"),
            "the span's lines are named: {sent}"
        );

        // What `:llm-inspect` shows is what the server received, byte for
        // byte once the pretty-printing is undone, and names no key.
        assert!(
            settle(&mut a, |a| chats(a)
                .last_sent(a.editor.active_pane_buffer_id())
                .is_some())
            .await
        );
        ex(&mut a, "llm-inspect");
        assert!(
            settle_woken(&mut a, |a| buffer_text(a, "*llm-inspect*")
                .starts_with("POST"))
            .await
        );
        let shown = buffer_text(&a, "*llm-inspect*");
        let value: serde_json::Value = serde_json::from_str(shown_body(&shown)).unwrap();
        assert_eq!(serde_json::to_vec(&value).unwrap(), received(&log)[0].body);
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(
            received(&log)[0].head.contains(SECRET),
            "the key did go to the server"
        );
    }

    /// In a chat, `:llm-inspect` shows what sending the typed prompt would
    /// send, before anything is sent; sending it then sends exactly that.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inspect_in_a_chat_previews_the_prompt_exactly() {
        let (mut a, log) = app();
        ex(&mut a, "llm-context-add");
        ex(&mut a, "llm");
        let chat = a.editor.active_pane_buffer_id();
        assert!(settle(&mut a, |a| chats(a).for_buffer(chat).is_some()).await);
        assert_eq!(chats(&a).context_of(chat).len(), 1, "the chat took a copy");
        press_chars(&mut a, "ihello there");
        key(&mut a, KeyCode::Esc);
        ex(&mut a, "llm-inspect");
        assert!(
            settle_woken(&mut a, |a| buffer_text(a, "*llm-inspect*")
                .starts_with("POST"))
            .await
        );
        let shown = buffer_text(&a, "*llm-inspect*");
        assert!(received(&log).is_empty(), "inspecting sent nothing");
        assert!(
            shown.contains("hello there") && shown.contains("alpha beta"),
            "{shown}"
        );
        assert!(!shown.contains(SECRET), "{shown}");

        press_chars(&mut a, "A");
        key(&mut a, KeyCode::Enter);
        assert!(settle(&mut a, |_| received(&log).len() == 1).await);
        let value: serde_json::Value = serde_json::from_str(shown_body(&shown)).unwrap();
        assert_eq!(serde_json::to_vec(&value).unwrap(), received(&log)[0].body);
    }

    /// The list names what is attached; `dd` on a row removes it, and the
    /// next request goes without it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dd_in_the_list_removes_an_item() {
        let (mut a, log) = app();
        let source = a.editor.active_pane_buffer_id();
        let file = temp_file("FROM-THE-FILE\n");
        ex(&mut a, "llm-context-add");
        ex(
            &mut a,
            &format!("llm-context-add {}", file.path().display()),
        );
        ex(&mut a, "llm-context");
        assert!(
            settle_woken(&mut a, |a| buffer_text(a, "*llm-context*").lines().count()
                == 3)
            .await,
            "{}",
            buffer_text(&a, "*llm-context*")
        );
        let list = buffer_text(&a, "*llm-context*");
        assert!(list.starts_with("Context for [no name]"), "{list}");
        assert!(
            list.contains("buffer  [no name]") && list.contains("3 lines"),
            "{list}"
        );
        press_chars(&mut a, "j");
        press_chars(&mut a, "dd");
        assert!(message(&a).contains("removed [no name]"), "{}", message(&a));
        assert!(
            settle_woken(&mut a, |a| buffer_text(a, "*llm-context*").lines().count()
                == 2)
            .await,
            "the list repaints itself: {}",
            buffer_text(&a, "*llm-context*")
        );
        assert_eq!(chats(&a).context_of(source).len(), 1);

        a.editor
            .handle_effect(lattice_grammar::Effect::FocusBuffer(source.0));
        press_chars(&mut a, "gg");
        ask_word(&mut a);
        assert!(settle(&mut a, |_| received(&log).len() == 1).await);
        let sent = last_user(&received(&log)[0]);
        assert!(sent.contains("FROM-THE-FILE"), "{sent}");
        assert!(
            !sent.contains("gamma delta"),
            "the removed buffer went: {sent}"
        );
    }

    /// A file `llm.exclude` names is refused when attached, so it never
    /// appears in the list to be sent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_excluded_file_is_refused() {
        let (mut a, _log) = app();
        ex(&mut a, "llm-context-add .env");
        assert!(
            message(&a).contains("matches llm.exclude"),
            "{}",
            message(&a)
        );
        assert!(
            chats(&a)
                .context_of(a.editor.active_pane_buffer_id())
                .is_empty()
        );
    }

    /// A chat carries a buffer that has since closed: its next request is
    /// refused with the reason, not sent without it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_closed_attached_buffer_is_reported_not_dropped() {
        let (mut a, log) = app();
        let source = a.editor.active_pane_buffer_id();
        ex(&mut a, "llm-context-add");
        ex(&mut a, "llm");
        let chat = a.editor.active_pane_buffer_id();
        assert!(settle(&mut a, |a| chats(a).for_buffer(chat).is_some()).await);
        // A second listed buffer, so the source is not the only one.
        let other = temp_file("other\n");
        ex(&mut a, &format!("e {}", other.path().display()));
        a.editor
            .handle_effect(lattice_grammar::Effect::FocusBuffer(source.0));
        assert_eq!(a.editor.active_pane_buffer_id(), source);
        ex(&mut a, "bd!");
        assert!(
            settle(&mut a, |a| a
                .editor
                .buffers
                .document_handle(source)
                .is_none())
            .await,
            "{}",
            message(&a)
        );
        // The chat's list names the closed buffer rather than dropping it.
        a.editor
            .handle_effect(lattice_grammar::Effect::FocusBuffer(chat.0));
        ex(&mut a, "llm-context");
        assert!(
            settle_woken(&mut a, |a| buffer_text(a, "*llm-context*")
                .contains("closed"))
            .await,
            "{}",
            buffer_text(&a, "*llm-context*")
        );
        a.editor
            .handle_effect(lattice_grammar::Effect::FocusBuffer(chat.0));
        press_chars(&mut a, "ihello");
        key(&mut a, KeyCode::Enter);
        assert!(
            settle_woken(&mut a, |a| buffer_text(
                a,
                &chats(a).for_buffer(chat).unwrap().name
            )
            .contains("is closed"))
            .await,
            "{}",
            buffer_text(&a, &chats(&a).for_buffer(chat).unwrap().name)
        );
        assert!(received(&log).is_empty(), "nothing was sent");
    }
}
