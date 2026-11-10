//! LL.11b: the rewrite operator through the real keystroke path, end to end.
//!
//! `<leader>ar{motion}` captures a span and opens an instruction prompt; the
//! answer comes from a local mock model server and opens as a diff against the
//! span's text; `:diff-accept` applies it to the buffer as one edit. Driven
//! through `press_chars`, because `Editor::dispatch_chord` does not compose an
//! operator with a motion.

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
    use lattice_ai::llm::rewrite::LlmRewritesHandle;
    use lattice_mode::modes::ReadOnlyMode;

    const TEXT: &str = "alpha beta\ngamma delta\nepsilon zeta\n";

    /// An OpenAI-compatible stream answering `content`.
    fn sse(content: &str) -> String {
        let delta = serde_json::json!({"choices":[{"index":0,"delta":{"content":content}}]});
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
             data: {delta}\n\n\
             data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
             data: [DONE]\n\n"
        )
    }

    /// A server answering every request with `content`. With `hold`, each
    /// answer waits for a message on the returned sender.
    fn serve(content: &str, hold: bool) -> (std::net::SocketAddr, mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let reply = sse(content);
        let (release, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let reply = reply.clone();
                let gate = gate.clone();
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
                                break;
                            }
                        }
                    }
                    if hold {
                        let _ = gate.lock().unwrap().recv();
                    }
                    let _ = s.write_all(reply.as_bytes());
                });
            }
        });
        (addr, release)
    }

    fn app_answering(content: &str, hold: bool) -> (App, mpsc::Sender<()>) {
        let (addr, release) = serve(content, hold);
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

    fn chats(a: &App) -> LlmChatsHandle {
        (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone()
    }

    fn rewrites(a: &App) -> LlmRewritesHandle {
        (*a.editor.services.get::<LlmRewritesHandle>().unwrap()).clone()
    }

    fn message(a: &App) -> String {
        a.editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    fn source_text(a: &App) -> String {
        a.editor.document.snapshot().buffer.as_string()
    }

    /// A buffer's text by id: the focus moves to a diff when one opens, so
    /// the source is not `editor.document` then.
    fn text_of(a: &App, id: lattice_core::BufferId) -> String {
        a.editor.buffers.document_handle(id).unwrap().text()
    }

    fn diffs_open(a: &App) -> usize {
        a.editor.programmatic_diff_accept_paths.len()
    }

    /// `<leader>arj` on line 1, instruction typed and sent.
    fn rewrite_first_two_lines(a: &mut App, instruction: &str) {
        press_chars(a, " arj");
        assert_eq!(rewrites(a).pending_count(), 1, "{}", message(a));
        press_chars(a, instruction);
        key(a, KeyCode::Enter);
    }

    #[test]
    fn the_operator_opens_an_instruction_prompt_over_its_span() {
        let mut a = app_with(TEXT, 10);
        press_chars(&mut a, " arj");
        assert!(a.editor.buffers.by_name("*llm-rewrite:1*").is_some());
        assert_eq!(rewrites(&a).pending_count(), 1);
    }

    /// `<leader>aR` opens the menu first, holding the rewrite; its `a` row
    /// moves it on to the instruction prompt, and the rewrite then runs to
    /// its diff exactly as `<leader>ar` does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_menu_first_rewrite_opens_the_menu_then_the_prompt() {
        let (mut a, _release) = app_answering("ALPHA BETA\nGAMMA DELTA", false);
        let src = a.editor.document_buffer_id;
        press_chars(&mut a, " aRj");
        let title = a
            .editor
            .picker
            .as_ref()
            .map(|p| p.title.clone())
            .unwrap_or_default();
        assert!(
            title.contains("LLM settings"),
            "the menu is open: {title:?}"
        );
        let held = chats(&a).menu_ask(src).expect("holding the rewrite");
        assert_eq!(held.kind, lattice_ai::llm::chat::MenuHold::Rewrite);
        assert_eq!(rewrites(&a).pending_count(), 1);
        press_chars(&mut a, "a");
        assert!(
            a.editor.buffers.by_name("*llm-rewrite:1*").is_some(),
            "the rewrite prompt opened: {}",
            message(&a)
        );
        assert!(chats(&a).menu_ask(src).is_none(), "moved on");
        press_chars(&mut a, "uppercase it");
        key(&mut a, KeyCode::Enter);
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "the diff never opened: {}",
            message(&a)
        );
    }

    /// `:1,2llm-rewrite` is the operator from the command line: the lines go
    /// to the model with the instruction, the answer opens as a diff, and
    /// accepting replaces them as one undoable edit. The span is tracked
    /// through the document's handle here, not by the operator, so this is
    /// also what proves it lands where the lines are.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_command_rewrites_the_range_through_review() {
        let (mut a, _release) = app_answering("ALPHA BETA\nGAMMA DELTA", false);
        let src = a.editor.document_buffer_id;
        submit_ex(&mut a, "1,2llm-rewrite uppercase it");
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "the diff never opened: {}",
            message(&a)
        );
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| text_of(a, src).starts_with("ALPHA")).await,
            "not applied: {:?} / {}",
            text_of(&a, src),
            message(&a)
        );
        assert_eq!(text_of(&a, src), "ALPHA BETA\nGAMMA DELTA\nepsilon zeta\n");
        press_chars(&mut a, "u");
        assert_eq!(text_of(&a, src), TEXT, "one `u` undoes the whole rewrite");
    }

    /// With no instruction the command opens the prompt over its lines, as
    /// the operator does, rather than sending an empty rewrite.
    #[test]
    fn the_command_without_an_instruction_opens_the_prompt() {
        let mut a = app_with(TEXT, 10);
        submit_ex(&mut a, "llm-rewrite");
        assert_eq!(rewrites(&a).pending_count(), 1, "{}", message(&a));
        assert!(a.editor.buffers.by_name("*llm-rewrite:1*").is_some());
    }

    /// The host's read-only gate stops operators, not ex-commands, so the
    /// command refuses a read-only buffer itself: accepting its diff would
    /// write the buffer through its handle.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_command_refuses_a_read_only_buffer() {
        let (mut a, _release) = app_answering("NOPE", false);
        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::activate_minor_by_id(
            &mut a.editor,
            buffer,
            ReadOnlyMode::mode_id(),
        );
        let llm = chats(&a);
        assert!(
            settle(&mut a, |_| llm.is_read_only(buffer)).await,
            "the read-only option never reached the llm layer"
        );
        submit_ex(&mut a, "1,2llm-rewrite change it");
        assert!(message(&a).contains("read-only"), "{}", message(&a));
        assert_eq!(rewrites(&a).pending_count(), 0, "nothing held");
        assert_eq!(diffs_open(&a), 0);
        assert_eq!(source_text(&a), TEXT);
    }

    /// The operator changes text, so a read-only buffer refuses it.
    #[test]
    fn a_read_only_buffer_refuses_a_rewrite() {
        let mut a = app_with(TEXT, 10);
        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::activate_minor_by_id(
            &mut a.editor,
            buffer,
            ReadOnlyMode::mode_id(),
        );
        press_chars(&mut a, " arj");
        assert_eq!(rewrites(&a).pending_count(), 0, "nothing held");
        assert_eq!(source_text(&a), TEXT);
    }

    /// A block is a column of pieces; one replacement cannot stand for it.
    #[test]
    fn a_blockwise_selection_is_refused_up_front() {
        let mut a = app_with(TEXT, 10);
        press(
            &mut a,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        );
        press_chars(&mut a, "jl ar");
        assert_eq!(rewrites(&a).pending_count(), 0, "nothing held");
        assert!(message(&a).contains("block"), "{}", message(&a));
    }

    #[test]
    fn an_excluded_file_is_refused_before_the_prompt() {
        let dir = unique_tempdir();
        let path = dir.join("server.pem");
        std::fs::write(&path, "KEY\n").unwrap();
        let mut a = app_with_path("KEY\n", 10, path);
        press_chars(&mut a, " arr");
        assert_eq!(rewrites(&a).pending_count(), 0);
        assert!(message(&a).contains("llm.exclude"), "{}", message(&a));
    }

    #[test]
    fn an_empty_instruction_sends_nothing() {
        let (mut a, _release) = app_answering("unused", false);
        rewrite_first_two_lines(&mut a, "");
        assert_eq!(rewrites(&a).pending_count(), 0, "taken");
        assert!(message(&a).contains("instruction"), "{}", message(&a));
        assert_eq!(source_text(&a), TEXT);
    }

    /// The whole path: the answer, fenced as models do, opens as a diff
    /// without a keystroke; accepting replaces the span as one undoable edit
    /// and writes nothing to disk (the buffer has no file).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_accepted_rewrite_replaces_the_span_as_one_edit() {
        let (mut a, _release) = app_answering("```text\nALPHA BETA\nGAMMA DELTA\n```", false);
        let src = a.editor.document_buffer_id;
        rewrite_first_two_lines(&mut a, "uppercase it");
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "the diff never opened: {}",
            message(&a)
        );
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| text_of(a, src).starts_with("ALPHA")).await,
            "not applied: {:?} / {}",
            text_of(&a, src),
            message(&a)
        );
        assert_eq!(text_of(&a, src), "ALPHA BETA\nGAMMA DELTA\nepsilon zeta\n");
        assert_eq!(a.editor.active_buffer_id(), src, "back in the source");
        press_chars(&mut a, "u");
        assert_eq!(text_of(&a, src), TEXT, "one `u` undoes the whole rewrite");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rejected_rewrite_leaves_the_buffer_untouched() {
        let (mut a, _release) = app_answering("NOPE", false);
        let src = a.editor.document_buffer_id;
        rewrite_first_two_lines(&mut a, "change it");
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        submit_ex(&mut a, "diff-reject");
        assert!(settle_woken(&mut a, |a| message(a).contains("rejected")).await);
        assert_eq!(text_of(&a, src), TEXT);
    }

    /// The span changed while the review was open: accepting does not apply,
    /// it reopens the diff against the text as it is now; accepting that does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_changed_span_is_reviewed_again_before_it_is_replaced() {
        let (mut a, _release) = app_answering("NEW", false);
        let src = a.editor.document_buffer_id;
        rewrite_first_two_lines(&mut a, "shorten");
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        // An edit inside the span, made behind the review's back. (One at
        // its start would land before it: the span does not grow.)
        let handle = a.editor.buffers.document_handle(src).unwrap();
        let _ = handle
            .apply_edit(lattice_protocol::edit::Edit::insert(
                lattice_protocol::position::Position::new(0, 2),
                "X".to_string(),
            ))
            .await;
        submit_ex(&mut a, "diff-accept");
        let again = settle_woken(&mut a, |a| {
            diffs_open(a) == 1 && message(a).contains("review it again")
        })
        .await;
        assert!(again, "not reviewed again: {}", message(&a));
        assert!(
            text_of(&a, src).starts_with("alXpha"),
            "nothing applied yet"
        );
        let left = a.editor.pane_tree.leaves()[1].buffer_id;
        assert!(
            text_of(&a, left).starts_with("alXpha"),
            "the second review's left side is the text as it is now: {:?}",
            text_of(&a, left)
        );
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| text_of(a, src) == "NEW\nepsilon zeta\n").await,
            "{:?} / {}",
            text_of(&a, src),
            message(&a)
        );
    }

    /// Two rewrites: the second's diff waits for the first review to close.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn diffs_open_one_at_a_time() {
        // Both answers are held until both rewrites are sent, so the first
        // diff cannot take the focus while the second is being typed.
        let (mut a, release) = app_answering("DONE", true);
        let src = a.editor.document_buffer_id;
        press_chars(&mut a, " arr");
        press_chars(&mut a, "first");
        key(&mut a, KeyCode::Enter);
        press_chars(&mut a, "jj");
        press_chars(&mut a, " arr");
        assert_eq!(rewrites(&a).pending_count(), 1, "{}", message(&a));
        press_chars(&mut a, "second");
        key(&mut a, KeyCode::Enter);
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        // Give the second answer time to arrive; its diff must still wait.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        a.editor.run_tick_pending();
        assert_eq!(diffs_open(&a), 1, "one at a time");
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1
                && text_of(a, src).contains("DONE"))
            .await,
            "the second never opened: {}",
            message(&a)
        );
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| text_of(a, src).matches("DONE").count() == 2).await,
            "{:?} / {} / diffs {}",
            text_of(&a, src),
            message(&a),
            diffs_open(&a)
        );
    }

    /// A macro replays a rewrite, instruction included, and its diff queues
    /// behind the one before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_macro_replays_a_rewrite() {
        let (mut a, _release) = app_answering("DONE", false);
        let src = a.editor.document_buffer_id;
        press_chars(&mut a, "qa arrfirst");
        key(&mut a, KeyCode::Enter);
        press_chars(&mut a, "q");
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        submit_ex(&mut a, "diff-accept");
        assert!(settle_woken(&mut a, |a| text_of(a, src).starts_with("DONE\n")).await);
        press_chars(&mut a, "j@a");
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "the replay sent nothing: {}",
            message(&a)
        );
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| text_of(a, src) == "DONE\nDONE\nepsilon zeta\n").await,
            "{:?}",
            text_of(&a, src)
        );
    }

    /// The text in the open prompt.
    fn prompt_text(a: &App) -> String {
        a.editor.document.snapshot().buffer.as_string()
    }

    /// LL.11c: `.` repeats a rewrite over the same motion at the caret, its
    /// prompt pre-filled with the last instruction; `<CR>` sends it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dot_repeats_a_rewrite_with_the_last_instruction() {
        let (mut a, _release) = app_answering("DONE", false);
        let src = a.editor.document_buffer_id;
        press_chars(&mut a, " arrshout");
        key(&mut a, KeyCode::Enter);
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        submit_ex(&mut a, "diff-accept");
        assert!(settle_woken(&mut a, |a| text_of(a, src).starts_with("DONE\n")).await);

        press_chars(&mut a, "j.");
        assert_eq!(
            rewrites(&a).pending_count(),
            1,
            "`.` held a rewrite: {}",
            message(&a)
        );
        assert_eq!(prompt_text(&a).trim_end(), "shout", "pre-filled");
        key(&mut a, KeyCode::Enter);
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "{}",
            message(&a)
        );
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| text_of(a, src) == "DONE\nDONE\nepsilon zeta\n").await,
            "{:?}",
            text_of(&a, src)
        );
    }

    /// The pre-fill is `.`'s: a rewrite started afresh opens an empty prompt.
    #[test]
    fn a_fresh_rewrite_opens_an_empty_prompt() {
        let (mut a, _release) = app_answering("unused", true);
        press_chars(&mut a, " arrshout");
        key(&mut a, KeyCode::Enter);
        press_chars(&mut a, "j arr");
        assert_eq!(rewrites(&a).pending_count(), 1);
        assert_eq!(prompt_text(&a).trim_end(), "", "empty");
    }

    /// `<Esc>` in the prompt `.` opened sends nothing, and the buffer is
    /// unchanged.
    #[test]
    fn escape_from_a_repeated_prompt_sends_nothing() {
        let (mut a, _release) = app_answering("unused", true);
        press_chars(&mut a, " arrshout");
        key(&mut a, KeyCode::Enter);
        press_chars(&mut a, "j.");
        assert_eq!(prompt_text(&a).trim_end(), "shout");
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);
        assert_eq!(rewrites(&a).pending_count(), 1, "held, never sent");
        assert_eq!(source_text(&a), TEXT);
    }

    /// A span whose prompt was dismissed is let go by the next rewrite: its
    /// tracked range is untracked rather than following every later edit.
    #[test]
    fn a_dismissed_prompt_releases_its_span() {
        let mut a = app_with(TEXT, 10);
        // By id: the focus moves to the prompt, so the source is not
        // `editor.document` while one is open.
        let source = a.editor.active_buffer_id();
        let tracked = move |a: &App| {
            a.editor
                .buffers
                .document_handle(source)
                .unwrap()
                .snapshot()
                .tracked_ranges
                .len()
        };
        let before = tracked(&a);
        press_chars(&mut a, " arj");
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);
        assert_eq!(tracked(&a), before + 1, "held while nothing replaced it");
        press_chars(&mut a, " arj");
        assert_eq!(rewrites(&a).pending_count(), 1, "the dismissed one went");
        // Untracking goes to the document's actor; give it its turn.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while tracked(&a) != before + 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(tracked(&a), before + 1, "only the live span is tracked");
    }

    /// `repeatable` now means something, so yank stopped claiming it: `.`
    /// after `yy` still repeats the change before it.
    #[test]
    fn dot_after_a_yank_repeats_the_change_before_it() {
        let mut a = app_with(TEXT, 10);
        press_chars(&mut a, "x");
        press_chars(&mut a, "yy");
        press_chars(&mut a, ".");
        assert_eq!(source_text(&a), "pha beta\ngamma delta\nepsilon zeta\n");
    }

    /// The span deleted before the answer is reviewed: nothing is applied and
    /// the answer is shown in its session buffer instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deleted_span_moves_the_answer_to_its_session() {
        let (mut a, release) = app_answering("REPLACED", true);
        let src = a.editor.document_buffer_id;
        rewrite_first_two_lines(&mut a, "rewrite");
        press_chars(&mut a, "ggdj");
        assert_eq!(text_of(&a, src), "epsilon zeta\n");
        release.send(()).unwrap();
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        submit_ex(&mut a, "diff-accept");
        assert!(
            settle_woken(&mut a, |a| message(a).contains("deleted")).await,
            "{}",
            message(&a)
        );
        assert_eq!(text_of(&a, src), "epsilon zeta\n", "nothing applied");
        let shown = a.editor.pane_tree.leaves().iter().any(|p| {
            a.editor
                .buffers
                .name_of(p.buffer_id)
                .is_some_and(|n| n.starts_with("*llm:"))
        });
        assert!(shown, "the session is shown");
    }
}
