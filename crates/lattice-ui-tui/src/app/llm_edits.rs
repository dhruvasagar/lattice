//! LL.17: lattice's edit tools, end to end through the real editor, the real
//! `install` wiring and the real diff subsystem, against a mock model whose
//! first reply makes the calls given and whose later replies say "done".
//! What a tool returned is read from the request that followed it, so each
//! test checks what the model was actually told.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::app::App;
    use crate::app::llm_mock::{Log, results, serve};
    use crate::app::test_helpers::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lattice_ai::llm::RequestState;
    use lattice_ai::llm::chat::LlmChatsHandle;
    use lattice_ai::llm::config::LlmConfig;
    use serde_json::{Value, json};

    fn chats(a: &App) -> LlmChatsHandle {
        (*a.editor.services.get::<LlmChatsHandle>().unwrap()).clone()
    }

    fn key(a: &mut App, code: KeyCode) {
        press(a, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ctrl(a: &mut App, c: char) {
        press(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn ex(a: &mut App, cmd: &str) {
        press_chars(a, &format!(":{cmd}"));
        key(a, KeyCode::Enter);
    }

    fn diffs_open(a: &App) -> usize {
        a.editor.programmatic_diff_accept_paths.len()
    }

    fn text_of(a: &App, id: lattice_core::BufferId) -> String {
        a.editor.buffers.document_handle(id).unwrap().text()
    }

    fn states(a: &App) -> Vec<RequestState> {
        chats(a)
            .requests()
            .snapshot()
            .iter()
            .map(|r| r.state.clone())
            .collect()
    }

    struct Project {
        dir: tempfile::TempDir,
    }

    impl Project {
        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.path(name)).unwrap_or_default()
        }
    }

    /// An editor in a project of one saved file, `notes.txt` ("one\ntwo\n"),
    /// whose model is the mock and whose requests may use all of lattice's
    /// tools.
    fn app(calls: Vec<(&'static str, Value)>) -> (App, Log, Project) {
        let (addr, log) = serve(calls);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "one\ntwo\n").unwrap();
        let a = app_with("scratch\n", 10);
        let toml = format!(
            "[llm]\ntools = \"lattice.*\"\n[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
        );
        chats(&a).set_config(LlmConfig::from_table(&toml.parse::<toml::Table>().unwrap()));
        chats(&a).set_project_root(dir.path().to_path_buf());
        (a, log, Project { dir })
    }

    fn edit(old: &str, new: &str) -> (&'static str, Value) {
        (
            "edit_file",
            json!({"path": "notes.txt", "old_string": old, "new_string": new}),
        )
    }

    /// Open a new chat; returns its buffer.
    async fn open_chat(a: &mut App) -> lattice_core::BufferId {
        ex(a, "llm");
        let chat = a.editor.active_pane_buffer_id();
        assert!(settle(a, |a| chats(a).for_buffer(chat).is_some()).await);
        chat
    }

    /// Send `prompt` from a new chat; returns the chat's buffer.
    async fn send(a: &mut App, prompt: &str) -> lattice_core::BufferId {
        let chat = open_chat(a).await;
        let session = chats(a).for_buffer(chat).unwrap();
        chats(a).submit(&session, prompt.to_string());
        chat
    }

    async fn second_request(a: &mut App, log: &Log) {
        assert!(
            settle_woken(a, |_| log.lock().unwrap().len() >= 2).await,
            "the loop never sent the results: {:?}",
            log.lock().unwrap()
        );
    }

    /// The edit opens a diff without a keystroke; accepting writes the file
    /// and tells the model so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_accepted_edit_is_written_and_the_model_is_told() {
        let (mut a, log, p) = app(vec![edit("two", "TWO")]);
        send(&mut a, "shout line two").await;
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "no diff"
        );
        assert_eq!(
            p.read("notes.txt"),
            "one\ntwo\n",
            "nothing before the verdict"
        );
        submit_ex(&mut a, "diff-accept");
        second_request(&mut a, &log).await;
        assert_eq!(p.read("notes.txt"), "one\nTWO\n");
        assert_eq!(results(&log), ["wrote `notes.txt`"]);
    }

    /// An open file is edited in its buffer, unsaved, as one undoable change;
    /// the disk keeps what the user last saved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_file_is_edited_in_its_buffer_and_not_saved() {
        let (mut a, log, p) = app(vec![edit("two", "TWO")]);
        ex(&mut a, &format!("e {}", p.path("notes.txt").display()));
        let file = a.editor.active_pane_buffer_id();
        send(&mut a, "shout line two").await;
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1).await,
            "no diff"
        );
        submit_ex(&mut a, "diff-accept");
        second_request(&mut a, &log).await;
        assert_eq!(text_of(&a, file), "one\nTWO\n");
        assert_eq!(p.read("notes.txt"), "one\ntwo\n", "not written");
        assert!(
            results(&log)[0].contains("in the editor"),
            "{:?}",
            results(&log)
        );
    }

    /// A reject changes nothing, and the model gets a refusal it can act on
    /// rather than a failed request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rejected_edit_reaches_the_model_as_a_refusal() {
        let (mut a, log, p) = app(vec![edit("two", "TWO")]);
        send(&mut a, "shout").await;
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        submit_ex(&mut a, "diff-reject");
        second_request(&mut a, &log).await;
        assert_eq!(p.read("notes.txt"), "one\ntwo\n");
        let said = &results(&log)[0];
        assert!(said.contains("rejected"), "{said}");
        assert!(
            settle_woken(&mut a, |a| states(a) == [RequestState::Done]).await,
            "the loop went on to its answer: {:?}",
            states(&a)
        );
    }

    /// While the diff is open the request says it waits on the user.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_review_shows_as_awaiting_review() {
        let (mut a, log, _p) = app(vec![edit("two", "TWO")]);
        send(&mut a, "shout").await;
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        assert_eq!(states(&a), [RequestState::AwaitingReview]);
        submit_ex(&mut a, "diff-accept");
        second_request(&mut a, &log).await;
        assert!(settle_woken(&mut a, |a| states(a) == [RequestState::Done]).await);
    }

    /// `<C-t>` in the chat: its edits apply with no diff, the headerline says
    /// so, and a new chat starts untrusted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trusted_chat_applies_without_review_and_trust_is_per_chat() {
        let (mut a, log, p) = app(vec![edit("two", "TWO")]);
        let chat = open_chat(&mut a).await;
        ctrl(&mut a, 't');
        let session = chats(&a).for_buffer(chat).unwrap();
        assert!(session.trusted(), "{:?}", a.editor.last_message);
        chats(&a).submit(&session, "shout".to_string());
        second_request(&mut a, &log).await;
        assert_eq!(diffs_open(&a), 0, "no review");
        assert_eq!(p.read("notes.txt"), "one\nTWO\n");

        let other = open_chat(&mut a).await;
        assert!(!chats(&a).for_buffer(other).unwrap().trusted());
        ctrl(&mut a, 't');
        ctrl(&mut a, 't');
        assert!(
            !chats(&a).for_buffer(other).unwrap().trusted(),
            "toggles off"
        );
    }

    /// The file changed on disk while the diff was open: accepting would
    /// overwrite that change, so nothing is written and the model is told to
    /// read again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_target_changed_during_review_is_not_overwritten() {
        let (mut a, log, p) = app(vec![edit("two", "TWO")]);
        send(&mut a, "shout").await;
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        std::fs::write(p.path("notes.txt"), "one\ntwo\nthree\n").unwrap();
        submit_ex(&mut a, "diff-accept");
        second_request(&mut a, &log).await;
        assert_eq!(p.read("notes.txt"), "one\ntwo\nthree\n");
        let said = &results(&log)[0];
        assert!(said.contains("changed while"), "{said}");
    }

    /// Two edits in one turn open one diff at a time, and each verdict is its
    /// own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn several_edits_queue_one_diff_at_a_time() {
        let (mut a, log, p) = app(vec![
            edit("one", "ONE"),
            (
                "write_file",
                json!({"path": "new.txt", "content": "fresh\n"}),
            ),
        ]);
        send(&mut a, "two changes").await;
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        let first: Vec<_> = a
            .editor
            .programmatic_diff_accept_paths
            .keys()
            .copied()
            .collect();
        submit_ex(&mut a, "diff-reject");
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 1
                && a.editor
                    .programmatic_diff_accept_paths
                    .keys()
                    .all(|k| !first.contains(k)))
            .await,
            "the second diff never opened"
        );
        submit_ex(&mut a, "diff-accept");
        second_request(&mut a, &log).await;
        assert_eq!(p.read("notes.txt"), "one\ntwo\n");
        assert_eq!(p.read("new.txt"), "fresh\n");
        let said = results(&log);
        assert!(said[0].contains("rejected"), "{said:?}");
        assert_eq!(said[1], "created `new.txt`");
    }

    /// Closing the chat while its diff is open closes the diff: no verdict
    /// would reach anyone, so none is asked for. Nothing is written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_the_chat_closes_its_diffs() {
        let (mut a, log, p) = app(vec![edit("two", "TWO")]);
        let chat = send(&mut a, "shout").await;
        assert!(settle_woken(&mut a, |a| diffs_open(a) == 1).await);
        a.activate_buffer(chat);
        assert_eq!(
            a.editor.active_buffer_id(),
            chat,
            "{:?}",
            a.editor.last_message
        );
        ex(&mut a, "bd!");
        assert!(
            settle_woken(&mut a, |a| diffs_open(a) == 0).await,
            "the diff stayed open: {:?}",
            a.editor.last_message
        );
        assert!(settle_woken(&mut a, |a| states(a) == [RequestState::Cancelled]).await);
        assert_eq!(p.read("notes.txt"), "one\ntwo\n");
        assert_eq!(log.lock().unwrap().len(), 1, "nothing more was sent");
    }
}
