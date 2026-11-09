//! LL.16: lattice's read tools, end to end through the real editor and the
//! real `install` wiring, against a mock model that asks for a read and then
//! answers. What a tool returned is read from the second recorded request,
//! so each test checks what the model actually received.

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

    type Log = Arc<Mutex<Vec<serde_json::Value>>>;

    fn body_of(s: &mut std::net::TcpStream) -> Option<serde_json::Value> {
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
                    return serde_json::from_slice(&buf[head_end + 4..head_end + 4 + len]).ok();
                }
            }
        }
    }

    fn sse(events: &[serde_json::Value]) -> String {
        let mut out = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        );
        for e in events {
            out.push_str(&format!("data: {e}\n\n"));
        }
        out.push_str("data: [DONE]\n\n");
        out
    }

    /// A model whose first reply calls `read_file` with `args`, and whose
    /// later replies say "done".
    fn serve(args: serde_json::Value) -> (std::net::SocketAddr, Log) {
        use serde_json::json;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Log = Arc::default();
        let seen = log.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let (seen, args) = (seen.clone(), args.to_string());
                std::thread::spawn(move || {
                    let n = {
                        let mut g = seen.lock().unwrap();
                        g.push(body_of(&mut s).unwrap_or_default());
                        g.len()
                    };
                    let reply = if n == 1 {
                        sse(&[
                            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_r","type":"function","function":{"name":"lattice__read_file","arguments":args}}]}}]}),
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

    /// An editor in a project of one saved file, `notes.txt`, whose model is
    /// the mock and whose requests may use lattice's read tools.
    fn app(args: serde_json::Value) -> (App, Log, tempfile::TempDir) {
        let (addr, log) = serve(args);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "saved\n").unwrap();
        let a = app_with("scratch\n", 10);
        let toml = format!(
            "[llm]\ntools = \"lattice.read\"\n[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
        );
        chats(&a).set_config(LlmConfig::from_table(&toml.parse::<toml::Table>().unwrap()));
        chats(&a).set_project_root(dir.path().to_path_buf());
        (a, log, dir)
    }

    /// What the tool returned, as the second request carried it.
    fn tool_result(log: &Log) -> String {
        let sent = log.lock().unwrap().clone();
        sent[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// Send `prompt` from a new chat and wait for the loop's second request.
    async fn ask(a: &mut App, log: &Log, prompt: &str) {
        ex(a, "llm");
        let chat = a.editor.active_pane_buffer_id();
        assert!(settle(a, |a| chats(a).for_buffer(chat).is_some()).await);
        let session = chats(a).for_buffer(chat).unwrap();
        chats(a).submit(&session, prompt.to_string());
        assert!(
            settle(a, |_| log.lock().unwrap().len() == 2).await,
            "the loop never sent the result: {:?}",
            log.lock().unwrap()
        );
    }

    /// The file open with an unsaved edit is read as the user sees it, not
    /// as it is on disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_returns_the_buffers_unsaved_text() {
        let (mut a, log, dir) = app(serde_json::json!({"path": "notes.txt"}));
        ex(
            &mut a,
            &format!("e {}", dir.path().join("notes.txt").display()),
        );
        press_chars(&mut a, "ggOunsaved");
        key(&mut a, KeyCode::Esc);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "saved\n",
            "the edit is not written"
        );

        ask(&mut a, &log, "read notes.txt").await;
        assert_eq!(tool_result(&log), "unsaved\nsaved\n");
        let sent = log.lock().unwrap().clone();
        let tools: Vec<&str> = sent[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert_eq!(
            tools,
            [
                "lattice__read_file",
                "lattice__list_directory",
                "lattice__search"
            ]
        );
    }

    /// A read reaching outside the project is refused by name, and the model
    /// is told why.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_outside_the_project_is_refused() {
        let (mut a, log, _dir) = app(serde_json::json!({"path": "../../etc/hostname"}));
        ask(&mut a, &log, "read it").await;
        let said = tool_result(&log);
        assert!(said.contains("outside the project"), "{said}");
    }
}
