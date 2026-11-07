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
}
