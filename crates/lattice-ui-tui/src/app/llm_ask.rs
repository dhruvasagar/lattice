//! LL.10b: the ask operator through the real keystroke path.
//!
//! `<Space>` is the default leader, so `<leader>aa{motion}` is typed `" aa…"`.
//! Driven through `press_chars`, because `Editor::dispatch_chord` does not
//! compose an operator with a motion (see `operator_seam.rs`). The request
//! itself, its answer and its settings are `lattice-host/tests/llm_ask.rs`;
//! here only what the keys decide: the span captured, the prompt, the pane,
//! the focus, read-only sources, `<Esc>`, blockwise and `.`.

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::app::test_helpers::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lattice_ai::llm::operators::LlmAsksHandle;
    use lattice_mode::modes::ReadOnlyMode;

    const TEXT: &str = "alpha beta\ngamma delta\nepsilon zeta\n";

    fn key(a: &mut App, code: KeyCode) {
        press(a, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn asks(a: &App) -> LlmAsksHandle {
        (*a.editor
            .services
            .get::<LlmAsksHandle>()
            .expect("the ask registry"))
        .clone()
    }

    fn active_name(a: &App) -> String {
        a.editor
            .buffers
            .name_of(a.editor.active_buffer_id())
            .unwrap_or_default()
    }

    /// The prompt the operator opened is up, carrying its ask's token.
    fn prompt_is_open(a: &App) -> bool {
        a.editor
            .buffers
            .by_name("*llm-ask:1*")
            .is_some_and(|_| asks(a).pending_count() == 1)
    }

    fn chat_panes(a: &App) -> usize {
        a.editor
            .pane_tree
            .leaves()
            .iter()
            .filter(|p| {
                a.editor
                    .buffers
                    .name_of(p.buffer_id)
                    .unwrap_or_default()
                    .starts_with("*llm:")
            })
            .count()
    }

    #[test]
    fn the_operator_opens_an_instruction_prompt_over_its_span() {
        let mut a = app_with(TEXT, 10);
        press_chars(&mut a, " aaj");
        assert!(prompt_is_open(&a), "active: {}", active_name(&a));
        assert_eq!(
            asks(&a).take(1).map(|s| s.text),
            Some("alpha beta\ngamma delta".into())
        );
    }

    #[test]
    fn submitting_the_prompt_opens_the_answer_beside_the_source() {
        let mut a = app_with(TEXT, 10);
        let source = a.editor.document_buffer_id;
        press_chars(&mut a, " aaj");
        assert!(prompt_is_open(&a));
        press_chars(&mut a, "why");
        key(&mut a, KeyCode::Enter);

        assert_eq!(asks(&a).pending_count(), 0, "the ask was taken");
        assert_eq!(chat_panes(&a), 1, "the answer has a pane");
        assert_eq!(
            a.editor.active_buffer_id(),
            source,
            "the source keeps focus"
        );
    }

    /// An editor whose model is the mock, and what the mock was sent.
    fn app_with_model(text: &str) -> (App, crate::app::llm_mock::Log) {
        let (addr, log) = crate::app::llm_mock::serve(Vec::new());
        let a = app_with(text, 10);
        let toml = format!(
            "[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
        );
        chats(&a).set_config(lattice_ai::llm::config::LlmConfig::from_table(
            &toml.parse::<toml::Table>().unwrap(),
        ));
        (a, log)
    }

    /// `<leader>as`: the buffer from its first line to the cursor's is the
    /// prompt, sent with no instruction prompt; what is below the cursor is
    /// not sent, and the answer opens beside the buffer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_sends_the_buffer_up_to_the_cursor() {
        let (mut a, log) = app_with_model("first\nsecond\nthird\n");
        let source = a.editor.document_buffer_id;
        press_chars(&mut a, "j as");
        assert!(!prompt_is_open(&a), "no instruction prompt");
        assert!(
            settle_woken(&mut a, |_| log.lock().unwrap().len() == 1).await,
            "nothing was sent: {}",
            message(&a)
        );
        let sent = log.lock().unwrap()[0].to_string();
        assert!(sent.contains("first\\nsecond"), "{sent}");
        assert!(!sent.contains("third"), "below the cursor stays: {sent}");
        assert_eq!(chat_panes(&a), 1, "the answer has a pane");
        assert_eq!(a.editor.active_buffer_id(), source, "focus stays");
    }

    /// `:llm-send` in an empty buffer sends nothing and says why: no request
    /// for an empty question, and no chat pane opened for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_in_an_empty_buffer_sends_nothing() {
        let (mut a, log) = app_with_model("");
        submit_ex(&mut a, "llm-send");
        // The refusal is the proof: it is synchronous, and the path that says
        // it returns before any request is built. Asserted first, so a send
        // cannot pass by being slower than the checks after it.
        let said = message(&a);
        assert!(said.contains("nothing"), "{said}");
        assert_eq!(chat_panes(&a), 0, "no pane for nothing");
        assert!(log.lock().unwrap().is_empty(), "a request went out");
    }

    /// The transcript drain runs on the layer's runtime, not the editor's.
    /// The editor here runs on this test's `current_thread` runtime, as the
    /// real editor actor does; a drain spawned there would stall while the
    /// thread is busy. Blocking it without yielding, the reply still lands.
    #[tokio::test(flavor = "current_thread")]
    async fn a_chat_reply_lands_while_the_editor_thread_is_busy() {
        let (mut a, log) = app_with_model("scratch\n");
        submit_ex(&mut a, "llm");
        let chat = a.editor.active_buffer_id();
        assert!(settle(&mut a, |a| chats(a).for_buffer(chat).is_some()).await);
        let session = chats(&a).for_buffer(chat).unwrap();
        chats(&a).submit(&session, "hi".to_string());

        let text = |a: &App| {
            a.editor
                .buffers
                .document_handle(chat)
                .map(|h| h.text())
                .unwrap_or_default()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !text(&a).contains("done") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            text(&a).contains("done"),
            "the reply waited for the editor thread ({} sent): {}",
            log.lock().unwrap().len(),
            text(&a)
        );
    }

    /// A reply streaming faster than the screen repaints is written at most
    /// once a frame (design §9): 60 deltas a millisecond apart become a
    /// handful of writes, not 60 re-projections, and the text is all there.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fast_stream_is_written_at_most_once_a_frame() {
        use lattice_ai::conversation::Role;
        use lattice_ai::conversation::transcript::FRAME;
        let (mut a, _log) = app_with_model("scratch\n");
        submit_ex(&mut a, "llm");
        let chat = a.editor.active_buffer_id();
        assert!(settle(&mut a, |a| chats(a).for_buffer(chat).is_some()).await);
        let session = chats(&a).for_buffer(chat).unwrap();
        let doc = a.editor.buffers.document_handle(chat).unwrap();
        // Let activation's own writes land before counting.
        assert!(settle(&mut a, |_| doc.text().contains("> ")).await);
        tokio::time::sleep(FRAME * 2).await;
        let before = doc.snapshot().text_version;

        let started = std::time::Instant::now();
        for i in 0..60 {
            session.store.mutate(&session.key, |c| {
                c.extend_text(Role::Assistant, &format!("t{i} "))
            });
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let streamed = started.elapsed();
        assert!(
            settle(&mut a, |_| doc.text().contains("t59 ")).await,
            "the last delta never landed: {}",
            doc.text()
        );
        let writes = doc.snapshot().text_version - before;
        let allowed = (streamed.as_millis() / FRAME.as_millis()) as u64 + 3;
        assert!(
            writes <= allowed,
            "{writes} writes in {streamed:?}; at most one a frame allows {allowed}"
        );
        assert!(doc.text().contains("t0 t1 t2 "), "{}", doc.text());
    }

    /// In a chat, `:llm-send` sends its prompt, as `<CR>` does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_in_a_chat_sends_its_prompt() {
        let (mut a, log) = app_with_model("scratch\n");
        submit_ex(&mut a, "llm");
        let chat = a.editor.active_buffer_id();
        assert!(settle(&mut a, |a| chats(a).for_buffer(chat).is_some()).await);
        press_chars(&mut a, "ihello there");
        key(&mut a, KeyCode::Esc);
        submit_ex(&mut a, "llm-send");
        assert!(
            settle_woken(&mut a, |_| log.lock().unwrap().len() == 1).await,
            "nothing was sent: {}",
            message(&a)
        );
        assert!(log.lock().unwrap()[0].to_string().contains("hello there"));
    }

    #[test]
    fn escape_from_the_prompt_sends_nothing() {
        let mut a = app_with(TEXT, 10);
        press_chars(&mut a, " aaj");
        assert!(prompt_is_open(&a));
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);
        assert_eq!(chat_panes(&a), 0, "no answer pane");
    }

    /// An ask reads; a read-only buffer (`*compilation*`, help) runs it.
    #[test]
    fn a_read_only_buffer_can_be_asked_about() {
        let mut a = app_with(TEXT, 10);
        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::activate_minor_by_id(
            &mut a.editor,
            buffer,
            ReadOnlyMode::mode_id(),
        );
        press_chars(&mut a, " aaj");
        assert!(prompt_is_open(&a), "active: {}", active_name(&a));
    }

    /// A help page is not the main document: the ask must read the page the
    /// caret is on, not the file underneath it.
    #[tokio::test]
    async fn a_help_page_is_asked_about_not_the_file_under_it() {
        let mut a = app_with(TEXT, 10);
        let lines: Vec<String> = vec!["help one".into(), "help two".into(), "help three".into()];
        install_help(
            &mut a,
            crate::help::HelpContent::from_lines("ask-help", lines),
        );
        let help = a.editor.active_buffer_id();
        let llm = lattice_mode::ModeId::new("llm-mode");
        let active = settle(&mut a, |a| {
            a.editor
                .active_modes
                .get(&help)
                .is_some_and(|m| m.is_active(llm))
        })
        .await;
        assert!(active, "llm-mode is on in the help page");
        press_chars(&mut a, " aaj");
        assert_eq!(asks(&a).pending_count(), 1, "active: {}", active_name(&a));
        let held = asks(&a).take(1).expect("held");
        assert!(held.text.starts_with("help one"), "{held:?}");
        assert!(!held.text.contains("alpha"), "not the file: {held:?}");
    }

    /// A block selection is one span: the ask is about the text, not per row.
    #[test]
    fn a_blockwise_selection_is_asked_about_as_one_span() {
        let mut a = app_with(TEXT, 10);
        press(
            &mut a,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        );
        press_chars(&mut a, "jl aa");
        assert_eq!(asks(&a).pending_count(), 1, "one ask, not one per row");
        let held = asks(&a).take(1).expect("held");
        assert!(held.text.starts_with("al"), "{held:?}");
        assert!(held.text.contains("ga"), "{held:?}");
    }

    /// `.` repeats changes; an ask is not one, so `.` neither re-opens the
    /// prompt nor re-sends, and still repeats the change before it.
    #[test]
    fn dot_does_not_repeat_an_ask() {
        let mut a = app_with(TEXT, 10);
        press_chars(&mut a, "x");
        let after_x = a.editor.document.snapshot().buffer.as_string();
        press_chars(&mut a, " aaj");
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);
        let before = asks(&a).pending_count();
        press_chars(&mut a, ".");
        assert_eq!(asks(&a).pending_count(), before, "no new ask");
        assert_ne!(
            a.editor.document.snapshot().buffer.as_string(),
            after_x,
            "`.` repeated the last change, `x`"
        );
    }

    // ── LL.10c ──────────────────────────────────────────────────────────────

    fn chats(a: &App) -> lattice_ai::llm::chat::LlmChatsHandle {
        (*a.editor
            .services
            .get::<lattice_ai::llm::chat::LlmChatsHandle>()
            .expect("llm chats"))
        .clone()
    }

    fn picker_title(a: &App) -> Option<String> {
        a.editor.picker.as_ref().map(|p| p.title.clone())
    }

    fn message(a: &App) -> String {
        a.editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    /// `<leader>aA` opens the menu first; its `a` row moves the held ask on
    /// to the instruction prompt.
    #[test]
    fn the_menu_first_ask_opens_the_menu_then_the_prompt() {
        let mut a = app_with(TEXT, 10);
        let source = a.editor.document_buffer_id;
        press_chars(&mut a, " aAj");
        let title = picker_title(&a).unwrap_or_default();
        assert!(
            title.contains("LLM settings"),
            "the menu is open: {title:?}"
        );
        assert!(chats(&a).menu_ask(source).is_some(), "holding the ask");
        press_chars(&mut a, "a");
        assert!(prompt_is_open(&a), "active: {}", active_name(&a));
        assert!(chats(&a).menu_ask(source).is_none(), "moved on");
        assert_eq!(
            asks(&a).take(1).map(|s| s.text),
            Some("alpha beta\ngamma delta".into())
        );
    }

    /// `:1,2llm-menu` is `<leader>aA` from the command line: the menu opens
    /// holding an ask over the range's lines, and `a` moves it on.
    #[test]
    fn the_menu_command_with_a_range_holds_an_ask() {
        let mut a = app_with(TEXT, 10);
        let source = a.editor.document_buffer_id;
        submit_ex(&mut a, "1,2llm-menu");
        let title = picker_title(&a).unwrap_or_default();
        assert!(
            title.contains("LLM settings"),
            "the menu is open: {title:?}"
        );
        let held = chats(&a).menu_ask(source).expect("holding the ask");
        assert_eq!(held.kind, lattice_ai::llm::chat::MenuHold::Ask);
        press_chars(&mut a, "a");
        assert!(prompt_is_open(&a), "active: {}", active_name(&a));
        assert_eq!(
            asks(&a).take(1).map(|s| s.text),
            Some("alpha beta\ngamma delta".into())
        );
    }

    /// `:llm-menu ask` without a range holds the caret's line; a word it does
    /// not know is refused and opens nothing.
    #[test]
    fn the_menu_command_names_what_it_holds() {
        let mut a = app_with(TEXT, 10);
        let source = a.editor.document_buffer_id;
        submit_ex(&mut a, "llm-menu ask");
        assert!(chats(&a).menu_ask(source).is_some(), "{}", message(&a));
        press_chars(&mut a, "a");
        assert_eq!(asks(&a).take(1).map(|s| s.text), Some("alpha beta".into()));
        key(&mut a, KeyCode::Esc);
        submit_ex(&mut a, "llm-menu explain");
        assert!(
            message(&a).contains("`ask` or `rewrite`"),
            "{}",
            message(&a)
        );
        assert!(chats(&a).menu_ask(source).is_none());
    }

    /// A menu opened by hand after a dismissed `<leader>aA` holds no ask.
    #[test]
    fn a_menu_opened_by_hand_holds_no_stale_ask() {
        let mut a = app_with(TEXT, 10);
        let source = a.editor.document_buffer_id;
        press_chars(&mut a, " aAj");
        assert!(chats(&a).menu_ask(source).is_some());
        key(&mut a, KeyCode::Esc);
        submit_ex(&mut a, "llm-menu");
        assert!(picker_title(&a).is_some(), "the menu is open");
        assert!(chats(&a).menu_ask(source).is_none(), "and holds no ask");
    }

    /// An oversized source asks first; `y` sends it.
    #[test]
    fn an_oversized_ask_is_confirmed_then_sent() {
        let big: String = (0..2000)
            .map(|i| format!("let line_{i} = {i};\n"))
            .collect();
        let mut a = app_with(&big, 10);
        press_chars(&mut a, "VG aa");
        assert_eq!(asks(&a).pending_count(), 1, "active: {}", active_name(&a));
        press_chars(&mut a, "sum");
        key(&mut a, KeyCode::Enter);
        let title = picker_title(&a).unwrap_or_default();
        assert!(title.contains("KB"), "a confirm, not a send: {title:?}");
        assert_eq!(chat_panes(&a), 0, "nothing sent before the yes");
        press_chars(&mut a, "y");
        assert_eq!(chat_panes(&a), 1, "sent on the yes: {}", message(&a));
        assert_eq!(asks(&a).pending_count(), 0);
    }

    /// `llm.exclude` refuses at the operator, before an instruction is typed.
    #[test]
    fn an_excluded_file_is_refused_before_the_prompt() {
        let dir = unique_tempdir();
        let path = dir.join(".env");
        std::fs::write(&path, "API_TOKEN=hunter2\n").unwrap();
        let mut a = app_with_path("API_TOKEN=hunter2\n", 10, path);
        press_chars(&mut a, " aaa");
        assert_eq!(asks(&a).pending_count(), 0, "nothing held");
        let msg = message(&a);
        assert!(msg.contains(".env") && msg.contains("llm.exclude"), "{msg}");
    }

    // -----------------------------------------------------------------
    // LL.15b: `llm.prefix`
    // -----------------------------------------------------------------

    fn keys(a: &App) -> lattice_ai::llm::modes::llm_mode::LlmKeysHandle {
        (*a.editor
            .services
            .get::<lattice_ai::llm::modes::llm_mode::LlmKeysHandle>()
            .expect("the llm keys"))
        .clone()
    }

    fn ctrl(a: &mut App, c: char) {
        press(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    /// The default prefix is bound once `llm-mode` activates.
    #[test]
    fn the_chords_start_with_leader_a_by_default() {
        let a = app_with("alpha\nbeta\n", 10);
        assert_eq!(keys(&a).bound().as_deref(), Some("<leader>a"));
    }

    /// `:set llm.prefix=<C-c>.` moves every chord: the operators and the
    /// commands answer under the new prefix and no longer under the old.
    /// Held alone, the prefix lists them all, which is what which-key shows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn setting_the_prefix_moves_every_chord() {
        let mut a = app_with("alpha\nbeta\n", 10);
        press_chars(&mut a, ":set llm.prefix=<C-c>.");
        key(&mut a, KeyCode::Enter);
        // Rebound off the editor thread, on the option's change event.
        assert!(
            settle(&mut a, |a| keys(a).bound().as_deref() == Some("<C-c>.")).await,
            "{}",
            message(&a)
        );

        let prefix = lattice_protocol::parse_chord_sequence("<C-c>.").unwrap();
        let held = a
            .editor
            .keymap
            .continuations_with_context(
                lattice_keymap::BindingMode::Normal,
                &prefix,
                &[lattice_ai::llm::modes::llm_mode::LlmMode::mode_id()],
            )
            .expect("the prefix is a node");
        let mut next: Vec<String> = held.children.iter().map(|c| c.chord.to_string()).collect();
        next.sort();
        for k in ["A", "C", "L", "R", "a", "c", "i", "k", "l", "m", "r"] {
            assert!(
                next.iter().any(|n| n == k),
                "{k} missing under the prefix: {next:?}"
            );
        }

        ctrl(&mut a, 'c');
        press_chars(&mut a, ".aj");
        assert!(prompt_is_open(&a), "the ask answers under the new prefix");
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);

        let held = asks(&a).pending_count();
        press_chars(&mut a, " aaj");
        assert_eq!(
            asks(&a).pending_count(),
            held,
            "and no longer under the old one"
        );
        assert!(a.editor.buffers.by_name("*llm-ask:2*").is_none());
        key(&mut a, KeyCode::Esc);

        ctrl(&mut a, 'c');
        press_chars(&mut a, ".q");
        assert_eq!(
            active_name(&a),
            "*llm-requests*",
            "a command chord moved too"
        );
    }

    /// A value that is not keys is refused by `:set`, and the chords stay.
    #[test]
    fn a_prefix_that_is_not_keys_is_refused() {
        let mut a = app_with("alpha\nbeta\n", 10);
        press_chars(&mut a, ":set llm.prefix=<Nope");
        key(&mut a, KeyCode::Enter);
        assert!(
            message(&a).contains("not a key sequence"),
            "{}",
            message(&a)
        );
        assert_eq!(keys(&a).bound().as_deref(), Some("<leader>a"));
    }
}
