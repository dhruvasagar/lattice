//! LL.13b–14: where an answer goes (`below`, `echo`, a register) and the
//! requests that carry it (`:llm-requests`, `:llm-stop`, `:llm-last`), end to
//! end through the real keystroke path against a local mock model.
//!
//! The server streams its chunks one at a time, each after a release on the
//! test's gate, so a test can act between two chunks: type above the answer,
//! type at its end, delete it, undo it, ask again. Results are awaited by
//! `settle_woken`, which waits for the editor's wake, so a chunk shown without
//! a keystroke is what each assertion checks.

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex, mpsc};

    use crate::app::App;
    use crate::app::test_helpers::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lattice_ai::llm::chat::LlmChatsHandle;
    use lattice_ai::llm::config::LlmConfig;
    use lattice_ai::llm::sink::{INTERRUPTED_MARKER, LlmInlineHandle};
    use lattice_mode::modes::ReadOnlyMode;

    const TEXT: &str = "alpha beta\ngamma delta\nepsilon zeta\n";

    /// How the mock's stream ends after its last chunk.
    #[derive(Clone, Copy)]
    enum End {
        Done,
        /// A provider error event: the reply fails partway.
        Error,
    }

    fn delta(content: &str) -> String {
        let d = serde_json::json!({"choices":[{"index":0,"delta":{"content":content}}]});
        format!("data: {d}\n\n")
    }

    fn read_request(s: &mut std::net::TcpStream) {
        let mut buf = vec![0u8; 65536];
        let mut got = 0;
        loop {
            let Ok(n) = s.read(&mut buf[got..]) else {
                return;
            };
            if n == 0 {
                return;
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
                    return;
                }
            }
        }
    }

    /// A server streaming `chunks`, the first at once and each later one (and
    /// the stream's end) after a message on the returned sender. `status`
    /// other than 200 answers with that status and no stream.
    fn serve(chunks: &[&str], end: End, status: u16) -> (std::net::SocketAddr, mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let chunks: Vec<String> = chunks.iter().map(|c| c.to_string()).collect();
        let (release, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let chunks = chunks.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    read_request(&mut s);
                    if status != 200 {
                        let body =
                            r#"{"error":{"message":"server on fire","type":"server_error"}}"#;
                        let _ = s.write_all(
                            format!(
                                "HTTP/1.1 {status} Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        );
                        return;
                    }
                    let _ = s.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                    );
                    for (i, c) in chunks.iter().enumerate() {
                        if i > 0 {
                            let _ = gate.lock().unwrap().recv();
                        }
                        let _ = s.write_all(delta(c).as_bytes());
                        let _ = s.flush();
                    }
                    let tail = match end {
                        End::Done => {
                            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                        }
                        End::Error => {
                            "data: {\"error\":{\"message\":\"overloaded\",\"type\":\"overloaded_error\"}}\n\n"
                        }
                    };
                    if !chunks.is_empty() {
                        let _ = gate.lock().unwrap().recv();
                    }
                    let _ = s.write_all(tail.as_bytes());
                });
            }
        });
        (addr, release)
    }

    fn app_streaming(chunks: &[&str], end: End, status: u16) -> (App, mpsc::Sender<()>) {
        let (addr, release) = serve(chunks, end, status);
        let a = app_with(TEXT, 10);
        let toml = format!(
            "[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
        );
        let chats: LlmChatsHandle = (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone();
        chats.set_config(LlmConfig::from_table(&toml.parse::<toml::Table>().unwrap()));
        (a, release)
    }

    fn key(a: &mut App, code: KeyCode) {
        press(a, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn inline(a: &App) -> LlmInlineHandle {
        (*a.editor.services.get::<LlmInlineHandle>().unwrap()).clone()
    }

    fn message(a: &App) -> String {
        a.editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    fn text(a: &App) -> String {
        a.editor.document.snapshot().buffer.as_string()
    }

    /// `<leader>aaj` on line 1 (the first two lines), answered below.
    fn ask_below(a: &mut App) {
        press_chars(a, " aaj");
        press_chars(a, "destination=below explain");
        key(a, KeyCode::Enter);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_answer_streams_in_under_the_span() {
        let (mut a, release) = app_streaming(&["It is ", "a list."], End::Done, 200);
        ask_below(&mut a);
        assert!(message(&a).contains("answering below"), "{}", message(&a));
        assert!(
            settle_woken(&mut a, |a| text(a).contains("It is ")).await,
            "the first chunk never showed: {:?}",
            text(&a)
        );
        assert_eq!(text(&a), "alpha beta\ngamma delta\nIt is \nepsilon zeta\n");
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);
        assert_eq!(
            text(&a),
            "alpha beta\ngamma delta\nIt is a list.\nepsilon zeta\n"
        );
        // One `u` takes the whole answer.
        press_chars(&mut a, "u");
        assert_eq!(text(&a), TEXT);
    }

    /// Typing above the answer moves its insertion point; the answer stays
    /// one piece under the span.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typing_above_moves_the_insertion_point() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        press_chars(&mut a, "ggITOP ");
        key(&mut a, KeyCode::Esc);
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);
        assert_eq!(
            text(&a),
            "TOP alpha beta\ngamma delta\none two\nepsilon zeta\n"
        );
    }

    /// Typing at the end of the answer while it streams: the answer stays
    /// contiguous and the typed text stays after it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typing_at_the_point_stays_after_the_answer() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        // To the answer's line, then append: the caret is at the insertion point.
        press_chars(&mut a, "3GA!");
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| text(a).contains("two")).await);
        press_chars(&mut a, "?");
        key(&mut a, KeyCode::Esc);
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);
        assert_eq!(
            text(&a),
            "alpha beta\ngamma delta\none two!?\nepsilon zeta\n"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deleting_the_insertion_point_stops_the_answer() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        // `gamma delta` and the answer under it, in one delete.
        press_chars(&mut a, "2Gdj");
        let _ = release.send(());
        assert!(
            settle_woken(&mut a, |a| message(a).contains("deleted")).await,
            "{}",
            message(&a)
        );
        assert_eq!(inline(&a).live_count(), 0);
        let _ = release.send(());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            text(&a),
            "alpha beta\nepsilon zeta\n",
            "nothing more written"
        );
    }

    /// `u` while it streams takes what was written and stops the rest.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn undo_while_streaming_cancels_then_undoes() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        press_chars(&mut a, "u");
        assert_eq!(text(&a), TEXT);
        let _ = release.send(());
        assert!(
            settle_woken(&mut a, |a| message(a).contains("undo")).await,
            "{}",
            message(&a)
        );
        let _ = release.send(());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(text(&a), TEXT, "the second chunk never landed");
    }

    /// A read-only source cannot take the answer: it opens in a chat beside
    /// it instead, and the echo says why.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_only_source_answers_in_a_new_buffer() {
        let (mut a, release) = app_streaming(&["answer"], End::Done, 200);
        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::activate_minor_by_id(
            &mut a.editor,
            buffer,
            ReadOnlyMode::mode_id(),
        );
        let chats: LlmChatsHandle = (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone();
        assert!(
            // Internal state with no screen of its own, so nothing wakes for
            // it: polled, not awaited on the wake.
            settle(&mut a, |_| chats.is_read_only(buffer)).await,
            "the read-only option never reached the llm layer"
        );
        ask_below(&mut a);
        assert!(message(&a).contains("read-only"), "{}", message(&a));
        assert_eq!(inline(&a).live_count(), 0);
        let _ = release.send(());
        let src = a.editor.buffers.document_handle(buffer).unwrap();
        assert_eq!(src.text(), TEXT);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_answer_at_the_same_point_is_refused() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        press_chars(&mut a, "gg");
        ask_below(&mut a);
        assert!(message(&a).contains("already streaming"), "{}", message(&a));
        assert_eq!(inline(&a).live_count(), 1);
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failure_partway_leaves_the_text_and_a_marker() {
        let (mut a, release) = app_streaming(&["half an answer"], End::Error, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("half")).await);
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);
        assert_eq!(
            text(&a),
            format!(
                "alpha beta\ngamma delta\nhalf an answer\n{INTERRUPTED_MARKER}\nepsilon zeta\n"
            )
        );
        assert!(message(&a).contains("interrupted"), "{}", message(&a));
        press_chars(&mut a, "u");
        assert_eq!(text(&a), TEXT, "the marker goes with the answer");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_immediate_failure_leaves_nothing_behind() {
        let (mut a, _release) = app_streaming(&[], End::Done, 500);
        ask_below(&mut a);
        assert!(
            settle_woken(&mut a, |a| inline(a).live_count() == 0
                && !message(a).contains("answering"))
            .await,
            "{}",
            message(&a)
        );
        assert!(message(&a).starts_with("llm:"), "{}", message(&a));
        assert_eq!(text(&a), TEXT);
        press_chars(&mut a, "u");
        assert_eq!(text(&a), TEXT, "no edit to undo");
    }

    fn screen(a: &App) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let snap = a.ad().snapshot.clone();
        t.draw(|f| {
            let _ = crate::render::draw_frame(f, a, &snap);
        })
        .unwrap();
        let buf = t.backend().buffer().clone();
        (0..12u16)
            .map(|y| {
                (0..40u16)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    /// The keystroke contract for a stream: rows above the answer do not
    /// change while it streams, and the caret stays on its text.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_streamed_rows_change_and_the_caret_stays_put() {
        let (mut a, release) = app_streaming(&["first", " second"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("first")).await);
        let cursor = a.editor.cursor;
        let before = screen(&a);
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| text(a).contains("second")).await);
        let after = screen(&a);
        assert_eq!(a.editor.cursor, cursor, "streaming never moves the caret");
        let alpha = after.iter().position(|r| r.contains("alpha")).unwrap();
        // `alpha beta` and `gamma delta`, the rows above the answer.
        assert_eq!(
            before[..alpha + 2],
            after[..alpha + 2],
            "rows above unchanged"
        );
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);
    }

    /// The view follows the answer only when the caret is at its end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_view_follows_only_a_caret_at_the_end() {
        let long = "\nx".repeat(30);
        let (mut a, release) = app_streaming(&["start", long.as_str()], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("start")).await);
        press_chars(&mut a, "3GA");
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| text(a).matches("\nx").count() >= 30).await);
        assert!(
            a.editor.scroll > 0,
            "the caret at the end took the view with it"
        );
        key(&mut a, KeyCode::Esc);
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| inline(a).live_count() == 0).await);

        let (mut b, release) = app_streaming(&["start", long.as_str()], End::Done, 200);
        ask_below(&mut b);
        assert!(settle_woken(&mut b, |b| text(b).contains("start")).await);
        press_chars(&mut b, "gg");
        release.send(()).unwrap();
        assert!(settle_woken(&mut b, |b| text(b).matches("\nx").count() >= 30).await);
        assert_eq!(
            b.editor.scroll, 0,
            "a caret above stays, and so does the view"
        );
        release.send(()).unwrap();
        assert!(settle_woken(&mut b, |b| inline(b).live_count() == 0).await);
    }

    /// `<leader>aaj` on line 1 with `destination=<kind>`, after `prefix` (a
    /// register, `"a`).
    fn ask_to(a: &mut App, prefix: &str, kind: &str) {
        press_chars(a, &format!("{prefix} aaj"));
        press_chars(a, &format!("destination={kind} explain"));
        key(a, KeyCode::Enter);
    }

    /// LL.14a: the echo area takes the reply's first line and says how much
    /// more `:llm-last` holds; the buffer and the layout do not change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_echo_answer_shows_its_first_line() {
        let (mut a, release) =
            app_streaming(&["It is a list.\n", "Of greek\nletters."], End::Done, 200);
        ask_to(&mut a, "", "echo");
        assert!(message(&a).contains("will show here"), "{}", message(&a));
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(
            settle_woken(&mut a, |a| message(a).starts_with("It is a list.")).await,
            "{}",
            message(&a)
        );
        assert_eq!(message(&a), "It is a list. (+2 lines; :llm-last shows all)");
        assert_eq!(text(&a), TEXT);
        assert_eq!(a.editor.pane_tree.leaves().len(), 1);

        // `:llm-last` opens the hidden session with the whole reply.
        press_chars(&mut a, ":llm-last");
        key(&mut a, KeyCode::Enter);
        assert_eq!(a.editor.pane_tree.leaves().len(), 2);
        let chats: LlmChatsHandle = (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone();
        let name = chats.last_ask().unwrap();
        assert!(
            settle_woken(&mut a, |a| {
                a.editor
                    .buffers
                    .by_name(&name)
                    .and_then(|b| a.editor.buffers.document_handle(b))
                    .is_some_and(|h| h.text().contains("letters."))
            })
            .await,
            "the chat shows the full reply"
        );
    }

    /// `"a` before the operator names the register; a multi-line reply puts
    /// as lines.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_register_answer_lands_in_the_typed_register() {
        let (mut a, release) = app_streaming(&["fn x() {}\n", "fn y() {}"], End::Done, 200);
        ask_to(&mut a, "\"a", "register");
        assert!(message(&a).contains("register \"a"), "{}", message(&a));
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(
            settle_woken(&mut a, |a| message(a).contains("is in register")).await,
            "{}",
            message(&a)
        );
        let reg = a
            .editor
            .registers
            .get(&lattice_grammar::Register::Named('a'))
            .unwrap();
        assert_eq!(reg.content, "fn x() {}\nfn y() {}\n");
        assert_eq!(text(&a), TEXT, "nothing written into the buffer");
        press_chars(&mut a, "gg\"ap");
        assert_eq!(
            text(&a),
            "alpha beta\nfn x() {}\nfn y() {}\ngamma delta\nepsilon zeta\n"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_register_answer_leaves_the_register_alone() {
        let (mut a, _release) = app_streaming(&[], End::Done, 500);
        ask_to(&mut a, "\"b", "register");
        assert!(
            settle_woken(&mut a, |a| message(a).starts_with("llm:")
                && !message(a).contains("asking"))
            .await,
            "{}",
            message(&a)
        );
        assert!(
            !a.editor
                .registers
                .contains_key(&lattice_grammar::Register::Named('b'))
        );
    }

    /// A request that may use tools needs a chat to show and approve them in:
    /// aimed at the buffer, it opens one instead and says so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tools_request_aimed_in_place_opens_a_chat_instead() {
        let (mut a, release) = app_streaming(&["answer"], End::Done, 200);
        press_chars(&mut a, ":setlocal llm.tools=lattice.read");
        key(&mut a, KeyCode::Enter);
        let chats: LlmChatsHandle = (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone();
        let buffer = a.editor.document_buffer_id;
        assert!(
            settle(&mut a, |_| chats
                .resolve_for(buffer)
                .is_ok_and(|r| !r.values.tools.is_empty()))
            .await,
            "the tools setting never reached the llm layer"
        );
        for kind in ["below", "echo", "register"] {
            ask_to(&mut a, "", kind);
            assert!(
                message(&a).contains("need a chat"),
                "{kind}: {}",
                message(&a)
            );
            assert_eq!(inline(&a).live_count(), 0);
            press_chars(&mut a, ":only");
            key(&mut a, KeyCode::Enter);
        }
        let _ = release.send(());
        assert_eq!(text(&a), TEXT);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn llm_last_with_nothing_asked_says_so() {
        let (mut a, _release) = app_streaming(&[], End::Done, 200);
        press_chars(&mut a, ":llm-last");
        key(&mut a, KeyCode::Enter);
        assert!(message(&a).contains("no answer to show"), "{}", message(&a));
        assert_eq!(a.editor.pane_tree.leaves().len(), 1);
    }

    fn list_text(a: &App) -> String {
        a.editor
            .buffers
            .by_name("*llm-requests*")
            .and_then(|b| a.editor.buffers.document_handle(b))
            .map(|h| h.text())
            .unwrap_or_default()
    }

    fn ex(a: &mut App, cmd: &str) {
        press_chars(a, &format!(":{cmd}"));
        key(a, KeyCode::Enter);
    }

    /// LL.14b: the list shows a running request with where it is going, and
    /// keeps itself current: its end shows without a keystroke.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_requests_list_follows_a_request_to_its_end() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        ex(&mut a, "llm-requests");
        assert!(
            settle_woken(&mut a, |a| list_text(a).contains("streaming")).await,
            "{:?}",
            list_text(&a)
        );
        let list = list_text(&a);
        assert!(list.contains("mock-model"), "{list}");
        assert!(list.contains("below "), "names the destination: {list}");
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(
            settle_woken(&mut a, |a| list_text(a).contains("done")).await,
            "{:?}",
            list_text(&a)
        );
        assert!(list_text(&a).contains("(none)"), "{}", list_text(&a));
        // Read-only: the list is not text to edit.
        let before = list_text(&a);
        press_chars(&mut a, "ggx");
        assert_eq!(list_text(&a), before);
    }

    /// `dd` on a row stops that request through its session: the inline
    /// answer gets its marker, and the row says it stopped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dd_in_the_list_stops_the_request() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        let source = a.editor.document_buffer_id;
        ex(&mut a, "llm-requests");
        assert!(settle_woken(&mut a, |a| list_text(a).contains("streaming")).await);
        press_chars(&mut a, "ggjdd");
        // The stop can land, and its echo replace this one, within the key.
        assert!(
            message(&a).contains("stopping request #")
                || message(&a).contains("answer was stopped"),
            "{}",
            message(&a)
        );
        assert!(
            settle_woken(&mut a, |a| list_text(a).contains("stopped")).await,
            "{:?}",
            list_text(&a)
        );
        let src = a.editor.buffers.document_handle(source).unwrap();
        assert!(
            settle_woken(&mut a, |_| src.text().contains(INTERRUPTED_MARKER)).await,
            "{:?}",
            src.text()
        );
        assert_eq!(
            src.text(),
            format!("alpha beta\ngamma delta\none \n{INTERRUPTED_MARKER}\nepsilon zeta\n")
        );
        let _ = release.send(());
        // `dd` on a finished row says so rather than pretending.
        press_chars(&mut a, "ggjdd");
        let _ = release.send(());
        assert!(
            message(&a).contains("no request on this line")
                || message(&a).contains("already finished"),
            "{}",
            message(&a)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn llm_stop_stops_this_buffers_one_request() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ex(&mut a, "llm-stop");
        assert_eq!(message(&a), "llm: no request is running");
        ask_below(&mut a);
        assert!(settle_woken(&mut a, |a| text(a).contains("one ")).await);
        ex(&mut a, "llm-stop");
        assert!(
            message(&a).contains("stopping request") || message(&a).contains("answer was stopped"),
            "{}",
            message(&a)
        );
        assert!(settle_woken(&mut a, |a| text(a).contains(INTERRUPTED_MARKER)).await);
        assert_eq!(a.editor.pane_tree.leaves().len(), 1, "no list opened");
        let _ = release.send(());
    }

    /// Two running from here: stopping one would be a guess, so the list
    /// opens instead and nothing is stopped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn llm_stop_with_several_running_opens_the_list() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_to(&mut a, "", "echo");
        ask_to(&mut a, "", "echo");
        let chats: LlmChatsHandle = (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone();
        let live = |c: &LlmChatsHandle| {
            c.requests()
                .snapshot()
                .iter()
                .filter(|r| !r.state.is_terminal())
                .count()
        };
        // The registry is not on screen; polled (see the read-only test).
        assert!(settle(&mut a, |_| live(&chats) == 2).await);
        ex(&mut a, "llm-stop");
        assert!(
            message(&a).contains("2 requests are running"),
            "{}",
            message(&a)
        );
        assert!(
            settle_woken(&mut a, |a| list_text(a).matches("streaming").count()
                + list_text(a).matches("starting").count()
                == 2)
            .await,
            "{:?}",
            list_text(&a)
        );
        assert_eq!(live(&chats), 2, "nothing was stopped");
        for _ in 0..4 {
            let _ = release.send(());
        }
    }

    fn llm_badge(a: &App) -> String {
        a.editor
            .modeline
            .snapshot()
            .content_for(
                lattice_mode::ModelineKey::Global,
                &lattice_mode::ElementId::new("llm"),
            )
            .map(|c| c.plain())
            .unwrap_or_default()
    }

    /// The painted modeline, after the publish the editor actor's wake arm
    /// runs between its tick and the paint.
    fn modeline_row(a: &mut App) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        a.editor.publish_render_state();
        let a = &*a;
        let mut t = Terminal::new(TestBackend::new(100, 12)).unwrap();
        let snap = a.ad().snapshot.clone();
        t.draw(|f| {
            let _ = crate::render::draw_frame(f, a, &snap);
        })
        .unwrap();
        let buf = t.backend().buffer().clone();
        // The active pane's modeline: the row that names the buffer.
        (0..12u16)
            .map(|y| {
                (0..100u16)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .rfind(|r| r.contains("[no name]"))
            .unwrap_or_default()
    }

    /// LL.14c: the modeline says a request is running, and stops saying so
    /// when it ends, both without a keystroke.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_modeline_shows_a_running_request_until_it_ends() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        assert_eq!(llm_badge(&a), "");
        ask_below(&mut a);
        assert!(
            settle_woken(&mut a, |a| llm_badge(a) == "llm").await,
            "{:?}",
            llm_badge(&a)
        );
        assert!(
            modeline_row(&mut a).contains(" llm "),
            "painted: {:?}",
            modeline_row(&mut a)
        );
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(
            settle_woken(&mut a, |a| llm_badge(a).is_empty()).await,
            "{:?}",
            llm_badge(&a)
        );
        assert!(
            !modeline_row(&mut a).contains(" llm "),
            "{:?}",
            modeline_row(&mut a)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_modeline_counts_several_running_requests() {
        let (mut a, release) = app_streaming(&["one ", "two"], End::Done, 200);
        ask_to(&mut a, "", "echo");
        ask_to(&mut a, "", "echo");
        assert!(
            settle_woken(&mut a, |a| llm_badge(a) == "llm 2").await,
            "{:?}",
            llm_badge(&a)
        );
        for _ in 0..4 {
            let _ = release.send(());
        }
        assert!(settle_woken(&mut a, |a| llm_badge(a).is_empty()).await);
    }
}
