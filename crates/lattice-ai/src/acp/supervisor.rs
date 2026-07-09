//! The AI supervisor: an idle tokio task owning the provider child process,
//! the ACP [`Connection`], and the active [`SessionId`] (AI-1b).
//!
//! Agent output flows ONLY into the [`AiLogger`]'s dedicated per-process
//! rings -- never into `*messages*`, never through `tracing::info!`, never
//! through an inbound event bus. [`AiClientHandle::spawn`] is the crate's
//! entry point: it starts this task and returns the clone-able,
//! non-blocking handle the editor thread talks to.

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{ContentBlock, SessionUpdate};
use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use lattice_agent::{AiLogLevel, AiLogSource, AiLogger, SessionKey};

use crate::Result;
use crate::acp::connection::{Connection, SessionId, SessionNotification};
use crate::acp::error::AiError;
use crate::acp::handle::{AiClientHandle, AiCmd, AiState};
use crate::acp::providers::ProviderConfig;

impl AiClientHandle {
    /// Spawn the supervisor task on `runtime` and return a handle onto it.
    ///
    /// The supervisor owns the provider child + [`Connection`] + active
    /// [`SessionId`] for as long as the task runs (until every clone of the
    /// returned handle is dropped, which closes the command channel). All
    /// protocol I/O happens on the supervisor task, never on the caller's
    /// thread.
    pub fn spawn(runtime: &tokio::runtime::Handle, logger: AiLogger) -> AiClientHandle {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<AiCmd>();
        let state = Arc::new(ArcSwap::from_pointee(AiState::default()));
        runtime.spawn(supervisor_loop(cmd_rx, state.clone(), logger));
        AiClientHandle { cmd_tx, state }
    }
}

/// Something the supervisor loop reacts to: a command from the editor, or
/// the provider child exiting without being asked to.
enum SupervisorEvent {
    Cmd(AiCmd),
    /// The provider process exited on its own (crashed, was killed out of
    /// band, or quit). It has already been reaped by the `wait` that
    /// observed it.
    ChildExited,
}

/// Await whichever comes first: the next editor command, or the provider
/// child's death.
///
/// Both branches are cancel-safe -- `mpsc::UnboundedReceiver::recv` and
/// `tokio::process::Child::wait` both document that losing a `select!` race
/// leaves their state intact -- so a command is never dropped and the
/// child's exit is never missed. Returning an owned event (rather than
/// selecting inline) keeps the `&mut child` borrow scoped to this function,
/// leaving the caller free to mutate `child` in the handler.
async fn next_event(
    cmd_rx: &mut mpsc::UnboundedReceiver<AiCmd>,
    child: &mut Option<tokio::process::Child>,
) -> Option<SupervisorEvent> {
    match child.as_mut() {
        Some(child) => {
            tokio::select! {
                cmd = cmd_rx.recv() => cmd.map(SupervisorEvent::Cmd),
                _ = child.wait() => Some(SupervisorEvent::ChildExited),
            }
        }
        None => cmd_rx.recv().await.map(SupervisorEvent::Cmd),
    }
}

/// Kill the provider child and reap it in the background.
///
/// `start_kill` only *sends* the signal; without a subsequent `wait` the
/// process lingers as a zombie. The child is moved into a detached task that
/// does that `wait`, so the supervisor never blocks on a dying process.
fn kill_child(mut child: tokio::process::Child) {
    let _ = child.start_kill();
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
}

/// The supervisor's command loop. Owns the live connection/session state
/// across iterations; each `AiCmd` is handled to completion (or, for
/// `Prompt`, fired off as its own task) before the next is pulled off the
/// channel. Between commands it also watches the provider child, so an agent
/// that dies on its own tears its session down instead of leaving a phantom
/// running `AiState` behind.
async fn supervisor_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<AiCmd>,
    state: Arc<ArcSwap<AiState>>,
    logger: AiLogger,
) {
    let mut conn: Option<Arc<Connection>> = None;
    let mut sess: Option<SessionId> = None;
    let mut active_key: Option<SessionKey> = None;
    // The provider subprocess the supervisor currently owns the lifecycle
    // of. Killed on `Stop` and on `Start` replacing an existing session --
    // otherwise it's an orphaned, potentially billed process left running
    // after the editor moves on.
    let mut child: Option<tokio::process::Child> = None;
    // Per-provider process index: first `opencode` session is index 1,
    // second is index 2, etc. -- the `*ai:opencode:1*` / `*ai:opencode:2*`
    // exit criterion.
    let mut indices: HashMap<&'static str, u32> = HashMap::new();

    while let Some(event) = next_event(&mut cmd_rx, &mut child).await {
        match event {
            // The provider died without being asked to. Nothing is left to
            // kill (the `wait` that observed the exit already reaped it), so
            // just tear the session down: the published `AiState` must stop
            // claiming a live session the moment the process backing it is
            // gone, or the modeline lies and every later `:ai-prompt` is
            // silently dropped until the user runs `:ai-stop`.
            SupervisorEvent::ChildExited => {
                logger.log(
                    active_key.as_ref(),
                    AiLogLevel::Warn,
                    AiLogSource::Lifecycle,
                    "agent exited",
                );
                child = None;
                conn = None;
                sess = None;
                active_key = None;
                state.store(Arc::new(AiState::default()));
            }
            SupervisorEvent::Cmd(AiCmd::Start(provider)) => {
                // Tear down any existing session/process before starting a
                // new one -- the supervisor owns lifecycle end-to-end, so an
                // old child must never keep running (or keep its drain task
                // writing into the old session's ring) once a new one takes
                // its place. The internal handles AND the published
                // AiState/active_key are reset together here, exactly once,
                // regardless of the new attempt's outcome: if it fails below,
                // this teardown already left everything idle, so the `Err`
                // arm needs no extra reset. If it succeeds, the `Ok` arm
                // republishes the new running state. This is a no-op on the
                // very first `Start` (no existing child, state already
                // default).
                if let Some(c) = child.take() {
                    kill_child(c);
                }
                conn = None;
                sess = None;
                active_key = None;
                state.store(Arc::new(AiState::default()));

                let idx = indices.entry(provider.display_name).or_insert(0);
                *idx += 1;
                let key = SessionKey::new(provider.display_name, *idx);

                logger.log(
                    Some(&key),
                    AiLogLevel::Info,
                    AiLogSource::Lifecycle,
                    format!("starting {}", provider.display_name),
                );

                match start_provider(&provider, key.clone(), logger.clone()).await {
                    Ok((new_conn, new_sess, new_child)) => {
                        conn = Some(new_conn);
                        sess = Some(new_sess);
                        child = Some(new_child);
                        active_key = Some(key.clone());
                        state.store(Arc::new(AiState {
                            running: true,
                            provider: Some(provider.display_name),
                            session: Some(key.clone()),
                        }));
                        logger.log(
                            Some(&key),
                            AiLogLevel::Info,
                            AiLogSource::Lifecycle,
                            "session opened",
                        );
                    }
                    Err(e) => {
                        logger.log(
                            Some(&key),
                            AiLogLevel::Error,
                            AiLogSource::Lifecycle,
                            format!("start failed: {e}"),
                        );
                    }
                }
            }
            SupervisorEvent::Cmd(AiCmd::Prompt(text)) => {
                if let (Some(c), Some(s)) = (conn.clone(), sess.clone()) {
                    let key = active_key.clone();
                    let logger = logger.clone();
                    tokio::spawn(async move {
                        if let Err(e) = crate::acp::session::prompt(&c, &s, &text).await {
                            logger.log(
                                key.as_ref(),
                                AiLogLevel::Error,
                                AiLogSource::Lifecycle,
                                format!("prompt failed: {e}"),
                            );
                        }
                    });
                } else {
                    logger.log(
                        None,
                        AiLogLevel::Warn,
                        AiLogSource::Lifecycle,
                        "prompt dropped: no active session",
                    );
                }
            }
            SupervisorEvent::Cmd(AiCmd::Stop) => {
                logger.log(
                    active_key.as_ref(),
                    AiLogLevel::Info,
                    AiLogSource::Lifecycle,
                    "stopped",
                );
                if let Some(c) = child.take() {
                    kill_child(c);
                }
                conn = None;
                sess = None;
                active_key = None;
                state.store(Arc::new(AiState::default()));
            }
        }
    }
}

/// Extract the `(source, text)` pair worth logging from one `session/update`
/// payload, or `None` for updates the AI log doesn't render as text
/// (tool calls, plans, mode changes, ...). Pure and unit-testable -- no I/O.
///
/// `SessionUpdate` and `ContentBlock` are both `#[non_exhaustive]`, so every
/// arm keeps a `_ => None` catch-all rather than an exhaustive match.
pub(crate) fn agent_log_entry(update: &SessionUpdate) -> Option<(AiLogSource, String)> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => Some((AiLogSource::AgentText, t.text.clone())),
            _ => None,
        },
        SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => Some((AiLogSource::Reasoning, t.text.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Drain `session/update` notifications into `session`'s `AiLogger` ring
/// until the sender side closes. This is the ONLY place agent output is
/// recorded -- never `*messages*`, never `tracing::info!`.
pub(crate) async fn drain_notifications(
    mut rx: mpsc::UnboundedReceiver<SessionNotification>,
    logger: AiLogger,
    session: SessionKey,
) {
    while let Some(notification) = rx.recv().await {
        if let Some((source, text)) = agent_log_entry(&notification.update) {
            logger.log(Some(&session), AiLogLevel::Info, source, text);
        }
    }
}

/// Spawn `provider` as a stdio subprocess, wire it into a [`Connection`],
/// drain its notifications into `logger`'s `session` ring, and run the ACP
/// handshake.
async fn start_provider(
    provider: &ProviderConfig,
    session: SessionKey,
    logger: AiLogger,
) -> Result<(Arc<Connection>, SessionId, tokio::process::Child)> {
    let mut child = tokio::process::Command::new(&provider.command)
        .args(&provider.args)
        .envs(provider.env.iter().cloned())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // The explicit `kill_child` calls cover `Stop` and replace-`Start`,
        // but not the supervisor task simply going away -- when the last
        // `AiClientHandle` clone drops, the command channel closes and
        // `supervisor_loop` returns, dropping this `Child`. Without this, the
        // agent (a billed process) outlives the editor that spawned it.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AiError::Process(e.to_string()))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AiError::Process("no stdin".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AiError::Process("no stdout".to_string()))?;

    let (conn, notif_rx) = Connection::spawn(stdout, stdin);
    tokio::spawn(drain_notifications(notif_rx, logger, session));

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let session_id = crate::acp::session::handshake(&conn, &cwd).await?;

    Ok((conn, session_id, child))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use agent_client_protocol::schema::v1::{
        ContentChunk, SessionId as AcpSessionId, TextContent, ToolCall,
    };
    use tokio::sync::mpsc;

    use lattice_agent::AiLogRecord;

    use super::*;

    fn text_update(text: &str, thought: bool) -> SessionUpdate {
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
        if thought {
            SessionUpdate::AgentThoughtChunk(chunk)
        } else {
            SessionUpdate::AgentMessageChunk(chunk)
        }
    }

    #[test]
    fn agent_log_entry_extracts_message_and_thought() {
        let message = text_update("hi", false);
        assert_eq!(
            agent_log_entry(&message),
            Some((AiLogSource::AgentText, "hi".to_string()))
        );

        let thought = text_update("thinking", true);
        assert_eq!(
            agent_log_entry(&thought),
            Some((AiLogSource::Reasoning, "thinking".to_string()))
        );

        let tool_call = SessionUpdate::ToolCall(ToolCall::new("tc-1", "search files"));
        assert_eq!(agent_log_entry(&tool_call), None);
    }

    #[tokio::test]
    async fn drain_logs_agent_text_into_session_ring() {
        let logger = AiLogger::with_defaults();
        let (tx, rx) = mpsc::unbounded_channel();
        let key = SessionKey::new("opencode", 1);

        let notification =
            SessionNotification::new(AcpSessionId::new("sess-1"), text_update("pong", false));
        tx.send(notification).expect("send should succeed");
        drop(tx);

        drain_notifications(rx, logger.clone(), key.clone()).await;

        let records = logger.snapshot_session(&key);
        assert!(
            records
                .iter()
                .any(|r: &AiLogRecord| r.source == AiLogSource::AgentText && r.message == "pong"),
            "expected an AgentText \"pong\" record, got {records:?}"
        );
    }

    /// Drives two consecutive `Start`s with a bogus provider command
    /// through the real supervisor loop (no real agent binary needed --
    /// `spawn()` itself fails). Each `Start` must still increment the
    /// per-provider index and log the start failure under the assigned
    /// `SessionKey`, making the `*ai:opencode:1*` / `*ai:opencode:2*`
    /// index-progression exit criterion self-verifying without a live
    /// process.
    #[tokio::test]
    async fn start_failure_still_increments_index_and_logs_per_session() {
        let logger = AiLogger::with_defaults();
        let handle = AiClientHandle::spawn(&tokio::runtime::Handle::current(), logger.clone());
        let cfg = ProviderConfig {
            command: "/nonexistent/definitely-not-a-real-binary".into(),
            args: vec![],
            env: vec![],
            display_name: "fakeprov",
        };

        handle.start(cfg.clone());
        handle.start(cfg.clone());

        let key1 = SessionKey::new("fakeprov", 1);
        let key2 = SessionKey::new("fakeprov", 2);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let s1 = logger.snapshot_session(&key1);
            let s2 = logger.snapshot_session(&key2);
            if !s1.is_empty() && !s2.is_empty() {
                assert!(
                    s1.iter().any(|r| r.message.contains("start failed")),
                    "expected a start-failure record for fakeprov:1, got {s1:?}"
                );
                assert!(
                    s2.iter().any(|r| r.message.contains("start failed")),
                    "expected a start-failure record for fakeprov:2, got {s2:?}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected both fakeprov:1 and fakeprov:2 start-failure records before the timeout"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A `Start` whose provider fails to spawn must leave the published
    /// `AiState` idle -- not the phantom-running state that results from
    /// resetting state only in the `Ok` arm of `start_provider`. Uses the
    /// same bogus-binary trick as the test above (no live agent needed).
    #[tokio::test]
    async fn start_failure_leaves_idle_state() {
        let logger = AiLogger::with_defaults();
        let handle = AiClientHandle::spawn(&tokio::runtime::Handle::current(), logger.clone());
        let cfg = ProviderConfig {
            command: "/nonexistent/definitely-not-a-real-binary".into(),
            args: vec![],
            env: vec![],
            display_name: "fakeprov",
        };

        handle.start(cfg);

        let key = SessionKey::new("fakeprov", 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let records = logger.snapshot_session(&key);
            if records.iter().any(|r| r.message.contains("start failed")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected a start-failure record for fakeprov:1 before the timeout"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(
            handle.snapshot(),
            AiState::default(),
            "state must be idle after a failed Start"
        );
    }

    /// A mock ACP agent, expressed as a `sh -c` script: it answers
    /// `initialize`, answers `session/new`, then exits -- standing in for a
    /// provider that dies on its own once a session is open. Deterministic
    /// and dependency-free (no agent binary, no network, no fixed timings).
    ///
    /// `agent-client-protocol` assigns each request a UUID *string* id
    /// (`"id":"39977f74-..."`), so `sed` lifts it back out verbatim and the
    /// canned replies quote it -- a numeric-id mock is silently ignored by
    /// the client and the handshake hangs.
    const MOCK_AGENT_EXITS_AFTER_SESSION: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":"%s","result":{"protocolVersion":1}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":"%s","result":{"sessionId":"sess-1"}}\n' "$id"
      exit 0 ;;
  esac
done
"#;

    fn mock_agent_provider() -> ProviderConfig {
        ProviderConfig {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), MOCK_AGENT_EXITS_AFTER_SESSION.into()],
            env: vec![],
            display_name: "mockprov",
        }
    }

    /// Poll until `session`'s ring holds a record containing `needle`.
    ///
    /// The mock agent exits the instant it answers `session/new`, so the
    /// running `AiState` exists for only microseconds -- polling
    /// `handle.snapshot()` for it would be inherently racy. The log ring is
    /// append-only, so a record that was ever written stays observable.
    async fn wait_for_record(logger: &AiLogger, session: &SessionKey, needle: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let records = logger.snapshot_session(session);
            if records.iter().any(|r| r.message.contains(needle)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for a {needle:?} record on {session:?}, got {records:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// An agent that exits on its own (crash, `/exit`, OOM-kill) must not
    /// leave a phantom running `AiState` behind: the supervisor watches the
    /// child and tears the session down when it dies. Without this, the
    /// modeline claims a live session forever and `:ai-prompt` is silently
    /// dropped until the user happens to run `:ai-stop`.
    #[tokio::test(flavor = "multi_thread")]
    async fn unexpected_child_exit_resets_state_and_logs() {
        let logger = AiLogger::with_defaults();
        let handle = AiClientHandle::spawn(&tokio::runtime::Handle::current(), logger.clone());
        let key = SessionKey::new("mockprov", 1);

        handle.start(mock_agent_provider());

        // The mock answers the handshake, so a session really does open ...
        wait_for_record(&logger, &key, "session opened").await;
        // ... and then the mock exits, which must tear that session down.
        wait_for_record(&logger, &key, "agent exited").await;

        assert_eq!(
            handle.snapshot(),
            AiState::default(),
            "state must be idle after the agent exits on its own"
        );
    }

    /// After the child dies, a subsequent `Start` must open a *fresh* session
    /// at the next index rather than resurrecting the dead one's key.
    #[tokio::test(flavor = "multi_thread")]
    async fn start_after_child_exit_opens_the_next_session() {
        let logger = AiLogger::with_defaults();
        let handle = AiClientHandle::spawn(&tokio::runtime::Handle::current(), logger.clone());
        let first = SessionKey::new("mockprov", 1);
        let second = SessionKey::new("mockprov", 2);

        handle.start(mock_agent_provider());
        wait_for_record(&logger, &first, "agent exited").await;

        handle.start(mock_agent_provider());
        wait_for_record(&logger, &second, "session opened").await;
        wait_for_record(&logger, &second, "agent exited").await;

        assert_eq!(handle.snapshot(), AiState::default());
    }

    /// Live end-to-end check against a real `opencode acp` subprocess,
    /// through the full supervisor + handle stack (mirrors
    /// `session::tests::opencode_end_to_end`, one layer up). Not run in CI.
    ///
    /// Run via `cargo test -p lattice-ai -- --ignored opencode_supervisor_end_to_end`.
    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn opencode_supervisor_end_to_end() {
        let logger = AiLogger::with_defaults();
        let handle = AiClientHandle::spawn(&tokio::runtime::Handle::current(), logger.clone());
        let key = SessionKey::new("opencode", 1);

        handle.start(ProviderConfig::opencode());

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if handle.snapshot().session.is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "session did not open before the timeout"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        handle.prompt("reply with the single word: pong".into());

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let has_agent_text = logger
                .snapshot_session(&key)
                .iter()
                .any(|r| r.source == AiLogSource::AgentText);
            if has_agent_text {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no AgentText record arrived before the timeout"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
