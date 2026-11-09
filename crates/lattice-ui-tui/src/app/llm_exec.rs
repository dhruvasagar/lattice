//! LL.18: `run_command`, end to end through the real editor, the real
//! `install` wiring and the shared `*ai-permission*` popup, against the mock
//! model. The popup opens on its own (no keystroke) and is answered with the
//! keys a user presses; what the model was told is read from the request that
//! followed.

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use crate::app::App;
    use crate::app::llm_mock::{Log, offered, results, serve};
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

    fn ex(a: &mut App, cmd: &str) {
        press_chars(a, &format!(":{cmd}"));
        key(a, KeyCode::Enter);
    }

    fn states(a: &App) -> Vec<RequestState> {
        chats(a)
            .requests()
            .snapshot()
            .iter()
            .map(|r| r.state.clone())
            .collect()
    }

    /// The permission popup's text, if it is open.
    fn popup(a: &App) -> Option<String> {
        let id = a.editor.popup_buffer?;
        Some(a.editor.buffers.document_handle(id)?.text())
    }

    fn run(command: &str) -> (&'static str, Value) {
        ("run_command", json!({ "command": command }))
    }

    /// An editor over a temp project whose model is the mock and whose
    /// requests may use `tools`.
    fn app(calls: Vec<(&'static str, Value)>, tools: &str) -> (App, Log, tempfile::TempDir) {
        let (addr, log) = serve(calls);
        let dir = tempfile::tempdir().unwrap();
        let a = app_with("scratch\n", 10);
        let toml = format!(
            "[llm]\ntools = \"{tools}\"\n[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
        );
        chats(&a).set_config(LlmConfig::from_table(&toml.parse::<toml::Table>().unwrap()));
        chats(&a).set_project_root(dir.path().to_path_buf());
        (a, log, dir)
    }

    async fn send(a: &mut App, prompt: &str) -> lattice_core::BufferId {
        ex(a, "llm");
        let chat = a.editor.active_pane_buffer_id();
        assert!(settle(a, |a| chats(a).for_buffer(chat).is_some()).await);
        let session = chats(a).for_buffer(chat).unwrap();
        chats(a).submit(&session, prompt.to_string());
        chat
    }

    /// Wait for the popup to open on its own, then pick option `n` (1-based)
    /// with the cursor and `<CR>`, as a user does.
    async fn answer(a: &mut App, n: usize) {
        assert!(
            settle_woken(a, |a| popup(a).is_some_and(|t| t.contains("Allow"))).await,
            "the question never opened: {:?}",
            popup(a)
        );
        press_chars(a, &"j".repeat(n + 1));
        key(a, KeyCode::Enter);
    }

    async fn second_request(a: &mut App, log: &Log) {
        assert!(
            settle_woken(a, |_| log.lock().unwrap().len() >= 2).await,
            "the loop never sent the results: {:?} / {:?}",
            states(a),
            popup(a)
        );
    }

    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn pids(file: &Path) -> Vec<String> {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The command waits for the user: the popup names the tool and shows
    /// the command; allowed once, it runs and the model gets its output.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_command_asks_first_and_runs_once_allowed() {
        let (mut a, log, _dir) = app(vec![run("echo hi")], "lattice.exec");
        send(&mut a, "say hi").await;
        assert!(
            settle_woken(&mut a, |a| popup(a).is_some_and(|t| t.contains("Allow"))).await,
            "no question"
        );
        let shown = popup(&a).unwrap();
        assert!(shown.contains("Allow lattice.run_command?"), "{shown}");
        assert!(shown.contains("$ echo hi"), "{shown}");
        assert_eq!(states(&a), [RequestState::AwaitingReview]);
        assert_eq!(log.lock().unwrap().len(), 1, "nothing ran yet");
        answer(&mut a, 1).await;
        second_request(&mut a, &log).await;
        assert_eq!(results(&log), ["exit status 0\n--- stdout ---\nhi\n"]);
    }

    /// A denial is a result the model can act on, and nothing runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_denial_reaches_the_model_and_nothing_runs() {
        let (mut a, log, dir) = app(vec![run("touch ran")], "lattice.exec");
        send(&mut a, "touch it").await;
        answer(&mut a, 3).await;
        second_request(&mut a, &log).await;
        assert!(results(&log)[0].contains("denied"), "{:?}", results(&log));
        assert!(!dir.path().join("ran").exists());
    }

    /// `Esc` decides later: the popup closes, the question stays pending and
    /// the request keeps waiting; `:ai-permission` brings it back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deferring_leaves_the_question_pending() {
        let (mut a, log, _dir) = app(vec![run("echo later")], "lattice.exec");
        send(&mut a, "later").await;
        assert!(settle_woken(&mut a, |a| popup(a).is_some_and(|t| t.contains("Allow"))).await);
        key(&mut a, KeyCode::Esc);
        assert!(settle(&mut a, |a| popup(a).is_none()).await, "closed");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(settle(&mut a, |a| popup(a).is_none()).await, "not reopened");
        assert_eq!(states(&a), [RequestState::AwaitingReview]);
        assert_eq!(log.lock().unwrap().len(), 1);
        ex(&mut a, "ai-permission");
        answer(&mut a, 1).await;
        second_request(&mut a, &log).await;
        assert!(results(&log)[0].contains("later"), "{:?}", results(&log));
    }

    /// After one question is answered, the next opens on its own: answering
    /// closes the menu for the auto-open, not only on screen.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_next_question_opens_on_its_own_after_an_answer() {
        let (mut a, log, _dir) = app(vec![run("echo one"), run("echo two")], "lattice.exec");
        send(&mut a, "both").await;
        answer(&mut a, 1).await;
        assert!(
            settle_woken(&mut a, |a| popup(a).is_some_and(|t| t.contains("echo two"))).await,
            "the second question never opened: {:?}",
            popup(&a)
        );
        answer(&mut a, 1).await;
        second_request(&mut a, &log).await;
        let said = results(&log);
        assert!(
            said[0].contains("one") && said[1].contains("two"),
            "{said:?}"
        );
    }

    /// "Allow this command for this chat" covers that command's later runs
    /// in it, and nothing else: a different command asks again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn allowing_a_command_for_the_chat_asks_once_for_that_command() {
        let (mut a, log, _dir) = app(
            vec![run("echo one"), run("echo one"), run("echo two")],
            "lattice.exec",
        );
        send(&mut a, "three").await;
        assert!(
            settle_woken(&mut a, |a| popup(a)
                .is_some_and(|t| t.contains("Allow this command for this chat")))
            .await,
            "{:?}",
            popup(&a)
        );
        answer(&mut a, 2).await;
        assert!(
            settle_woken(&mut a, |a| popup(a).is_some_and(|t| t.contains("echo two"))).await,
            "the other command never asked: {:?}",
            popup(&a)
        );
        answer(&mut a, 1).await;
        second_request(&mut a, &log).await;
        let said = results(&log);
        assert!(
            said[0].contains("one") && said[1].contains("one") && said[2].contains("two"),
            "{said:?}"
        );
    }

    /// A trusted edit drops the chat's allowed commands. `echo one` was
    /// allowed for the chat, then an edit applied without review: the same
    /// command asks again, because what it runs may no longer be what was
    /// allowed (an unseen `build.rs` changes what `cargo test` does).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trusted_edit_drops_the_commands_allowed_for_the_chat() {
        let (mut a, log, dir) = app(
            vec![
                run("echo one"),
                (
                    "write_file",
                    json!({"path": "build.rs", "content": "fn main() {}\n"}),
                ),
                run("echo one"),
            ],
            "lattice.exec,lattice.edit",
        );
        ex(&mut a, "llm");
        let chat = a.editor.active_pane_buffer_id();
        assert!(settle(&mut a, |a| chats(a).for_buffer(chat).is_some()).await);
        let session = chats(&a).for_buffer(chat).unwrap();
        assert!(session.toggle_trust(), "edits trusted");
        chats(&a).submit(&session, "go".to_string());

        answer(&mut a, 2).await;
        assert!(
            settle_woken(&mut a, |a| popup(a).is_some_and(|t| t.contains("echo one"))).await,
            "the allowed command did not ask again after a trusted edit: {:?}",
            popup(&a)
        );
        assert!(
            dir.path().join("build.rs").exists(),
            "the trusted edit applied without a review"
        );
        answer(&mut a, 1).await;
        second_request(&mut a, &log).await;
        let said = results(&log);
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(said[2].contains("one"), "{said:?}");
    }

    /// Stopping the request kills what the command started, children
    /// included, not only the shell.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_the_request_kills_the_process_tree() {
        let (mut a, log, dir) = app(
            vec![run("echo $$ > pids; sleep 30 & echo $! >> pids; wait")],
            "lattice.exec",
        );
        let chat = send(&mut a, "wait").await;
        answer(&mut a, 1).await;
        let file = dir.path().join("pids");
        assert!(
            settle(&mut a, |_| pids(&file).len() == 2).await,
            "never started: {:?}",
            pids(&file)
        );
        assert!(pids(&file).iter().all(|p| alive(p)));
        let session = chats(&a).for_buffer(chat).unwrap();
        chats(&a).interrupt(&session);
        assert!(
            settle(&mut a, |_| pids(&file).iter().all(|p| !alive(p))).await,
            "still running: {:?}",
            pids(&file)
        );
        assert!(settle(&mut a, |a| states(a) == [RequestState::Cancelled]).await);
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    /// A command that writes without end is cut at the cap, and the model is
    /// told it was cut.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_is_capped() {
        let (mut a, log, _dir) = app(
            vec![run("head -c 200000 /dev/zero | tr '\\0' a")],
            "lattice.exec",
        );
        send(&mut a, "lots").await;
        answer(&mut a, 1).await;
        second_request(&mut a, &log).await;
        let said = &results(&log)[0];
        assert!(
            said.contains("[stdout cut at 30000 bytes]"),
            "{}",
            &said[..80]
        );
        assert!(said.len() < 31_000, "{}", said.len());
    }

    /// A project's `.lattice/llm.toml` cannot switch exec on: the user's
    /// config allows reads, so the request offers reads and no command.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_cannot_enable_exec() {
        let (mut a, log, dir) = app(vec![("read_file", json!({"path": "x"}))], "lattice.read");
        std::fs::create_dir_all(dir.path().join(".lattice")).unwrap();
        std::fs::write(
            dir.path().join(".lattice/llm.toml"),
            "tools = [\"lattice.exec\"]\n",
        )
        .unwrap();
        send(&mut a, "anything").await;
        assert!(settle_woken(&mut a, |_| !log.lock().unwrap().is_empty()).await);
        let tools = offered(&log);
        assert!(
            !tools.iter().any(|t| t == "lattice__run_command"),
            "{tools:?}"
        );
        assert!(tools.iter().any(|t| t == "lattice__read_file"), "{tools:?}");
    }
}
