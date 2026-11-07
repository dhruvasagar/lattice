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
}
