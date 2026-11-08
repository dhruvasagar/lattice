//! LL.19: fork a chat from a past question, and `:llm-retry` its last one.
//! The buffer half (the forked chat opens with its prompt filled) is tested in
//! `lattice-host/tests/llm_chat_buffer.rs`.

#![cfg(feature = "llm")]
#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::any::TypeId;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_ai::conversation::{Block, OpaqueReasoning, Role, ToolCallData, ToolKind, ToolStatus};
use lattice_ai::llm::chat::{ChatSession, LlmChats, LlmChatsHandle, spawn_turn_listener};
use lattice_ai::llm::config::LlmConfig;
use lattice_ai::llm::modes::chat::fork_effect;
use lattice_ai::llm::options::LlmModel;
use lattice_ai::llm::{RequestRegistry, RequestRegistryHandle};
use lattice_config::{OptionOrigin, OptionOverride, OptionOverrideSet, ResolvedOptions, Resolver};
use lattice_core::BufferId;
use lattice_grammar::effect::Effect;
use llm_support::*;
use tokio::io::AsyncWriteExt;

const ANTHROPIC_BODY: &str = include_str!("fixtures/llm/anthropic_text_thinking.sse");
/// The signature in [`ANTHROPIC_BODY`].
const SIGNATURE: &str = "EqQBCgIYAhIM";

fn sse(body: &str) -> Vec<u8> {
    format!("{SSE_HEAD}{body}").into_bytes()
}

fn chats_with(text: &str) -> LlmChatsHandle {
    let config = LlmConfig::from_table(&text.parse::<toml::Table>().unwrap());
    assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let handle = tokio::runtime::Handle::current();
    let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
        handle.clone(),
        Some(bus.clone()),
        None,
    ));
    let chats: LlmChatsHandle = Arc::new(LlmChats::new(handle, requests, bus.clone(), config));
    spawn_turn_listener(chats.clone(), &bus);
    chats
}

fn mock_provider(addr: std::net::SocketAddr) -> String {
    format!(
        r#"
        [llm.providers.mock]
        kind = "openai-compatible"
        base_url = "http://{addr}/v1"
        model = "test-model"
        "#
    )
}

/// A server that answers every request with `body` after `delay`, and keeps
/// what it was sent.
async fn recording(
    body: Vec<u8>,
    delay: Duration,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let addr = serve(move |req, mut s| {
        let log = log.clone();
        let body = body.clone();
        async move {
            log.lock().unwrap().push(req);
            tokio::time::sleep(delay).await;
            let _ = s.write_all(&body).await;
            let _ = s.shutdown().await;
        }
    })
    .await;
    (addr, seen)
}

fn bound(chats: &LlmChatsHandle, id: u32) -> (Arc<ChatSession>, BufferId) {
    let session = chats.create();
    let buffer = BufferId(id);
    chats.bind(&session.name, buffer).unwrap();
    (session, buffer)
}

/// What the host's resolver leaves for a buffer with `:setlocal llm.model=…`.
fn local_model(value: &str) -> ResolvedOptions {
    let mut set = OptionOverrideSet::new();
    set.push(OptionOverride::new(
        TypeId::of::<LlmModel>(),
        value.to_string(),
    ));
    let mut resolved = ResolvedOptions::new();
    Resolver::new().resolve_into_with_origins([(&set, OptionOrigin::BufferLocal)], &mut resolved);
    resolved
}

async fn say(chats: &LlmChatsHandle, session: &Arc<ChatSession>, text: &str) {
    chats.submit(session, text.into());
    until("turn", || !session.is_busy()).await;
}

fn model_of(request: &str) -> String {
    body_json(request)["model"].as_str().unwrap().to_string()
}

/// Two exchanges, the first with signed reasoning and a tool call, written
/// straight into the model.
fn seed_two_exchanges(session: &ChatSession) {
    session.store.mutate(&session.key, |c| {
        c.push_user_text("first question");
        c.extend_reasoning("thinking");
        c.attach_reasoning_data(OpaqueReasoning {
            provider: "anthropic".into(),
            data: "sig-1".into(),
        });
        c.push_tool_call(
            "call-1".into(),
            None,
            "read a".into(),
            ToolStatus::Ok,
            ToolKind::Read,
            None,
            Some("contents of a".into()),
        );
        c.tool_calls.insert(
            "call-1".into(),
            ToolCallData {
                name: "lattice__read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
                malformed: None,
            },
        );
        c.extend_text(Role::Assistant, "first answer");
        c.push_user_text("second question");
        c.extend_text(Role::Assistant, "second answer");
    });
}

#[tokio::test]
async fn a_fork_holds_the_history_before_the_question_and_leaves_the_original() {
    let (addr, seen) = recording(sse(OPENAI_OK_BODY), Duration::ZERO).await;
    let chats = chats_with(&mock_provider(addr));
    let (original, buffer) = bound(&chats, 1);
    seed_two_exchanges(&original);
    let before = original.store.snapshot();

    // `gb` on the second question's text.
    let opts = chats.projection_options_for(buffer);
    let shown = lattice_ai::conversation::render_conversation_with(&before, &opts);
    let line = shown
        .lines()
        .position(|l| l.contains("second question"))
        .unwrap() as u32;
    let Effect::OpenSyntheticBuffer { name, .. } = fork_effect(&chats, buffer, line) else {
        panic!("expected the fork's buffer to open");
    };
    assert_ne!(name, original.name, "a new chat, not this one");
    let fork_buffer = BufferId(2);
    let (fork, _) = chats.bind(&name, fork_buffer).unwrap();

    // The history up to the question, tool calls and their arguments included,
    // and the question ready to edit.
    let forked = fork.store.snapshot();
    assert_eq!(forked.turns, before.turns[..2].to_vec());
    assert_eq!(forked.tool_calls, before.tool_calls);
    assert_eq!(
        forked
            .reasoning_data
            .values()
            .map(|d| d.data.as_str())
            .collect::<Vec<_>>(),
        ["sig-1"]
    );
    assert_eq!(fork.take_prompt_seed().as_deref(), Some("second question"));
    assert_eq!(
        fork.take_prompt_seed(),
        None,
        "taken once: a reopen does not refill"
    );

    // Sending from the fork carries the earlier exchange, the call included.
    say(&chats, &fork, "second question, rephrased").await;
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("first answer") && sent[0].contains("a.rs"),
        "{}",
        sent[0]
    );
    assert!(
        sent[0].contains("contents of a"),
        "the call's result: {}",
        sent[0]
    );
    assert!(
        !sent[0].contains("second answer"),
        "nothing after the fork point"
    );

    // The original is exactly as it was.
    assert_eq!(original.store.snapshot(), before);

    // On the prompt there is nothing to fork from, and it says so.
    let prompt_line = shown.matches('\n').count() as u32;
    assert!(matches!(
        fork_effect(&chats, buffer, prompt_line),
        Effect::Echo { text, .. } if text.contains("past turn")
    ));
}

#[tokio::test]
async fn retry_sends_the_last_question_again_with_the_settings_as_they_are_now() {
    let (addr, seen) = recording(sse(OPENAI_OK_BODY), Duration::ZERO).await;
    let chats = chats_with(&mock_provider(addr));
    let (session, buffer) = bound(&chats, 1);
    say(&chats, &session, "the question").await;
    assert_eq!(model_of(&seen.lock().unwrap()[0]), "test-model");

    chats.on_buffer_options(buffer, &local_model("mock/other-model"));
    chats.retry(&session).unwrap();
    until("the retried reply", || {
        !session.is_busy() && seen.lock().unwrap().len() == 2
    })
    .await;

    let sent = seen.lock().unwrap().clone();
    assert_eq!(
        model_of(&sent[1]),
        "other-model",
        "current settings, not the old ones"
    );
    assert_eq!(sent[1].matches("the question").count(), 1, "{}", sent[1]);
    assert!(
        !sent[1].contains("Héllo"),
        "the old reply is not history: {}",
        sent[1]
    );
    let snap = session.store.snapshot();
    let roles: Vec<Role> = snap.turns.iter().map(|t| t.role).collect();
    assert_eq!(
        roles,
        [Role::User, Role::Assistant],
        "the reply was replaced"
    );
    assert!(snap.error.is_none());
}

#[tokio::test]
async fn retry_while_a_reply_is_coming_is_refused() {
    let (addr, seen) = recording(sse(OPENAI_OK_BODY), Duration::from_millis(400)).await;
    let chats = chats_with(&mock_provider(addr));
    let (session, _) = bound(&chats, 1);
    chats.submit(&session, "slow".into());
    assert!(session.is_busy());
    let err = chats.retry(&session).unwrap_err();
    assert!(err.contains("still coming"), "{err}");
    until("the one reply", || !session.is_busy()).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "nothing extra was sent");
    assert_eq!(session.store.snapshot().turns.len(), 2);
}

#[tokio::test]
async fn retry_with_nothing_asked_says_so() {
    let (addr, _) = recording(sse(OPENAI_OK_BODY), Duration::ZERO).await;
    let chats = chats_with(&mock_provider(addr));
    let (session, _) = bound(&chats, 1);
    let err = chats.retry(&session).unwrap_err();
    assert!(err.contains("nothing to retry"), "{err}");
    assert!(!session.is_busy());
}

/// A log sink for the notes a request writes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[tokio::test]
async fn retrying_on_another_provider_drops_its_reasoning_and_says_so_once() {
    let log = Captured::default();
    let writer = log.clone();
    // Global: the request is prepared on whichever thread runs it. Only this
    // test installs one, and it reads back only its own provider's note (the
    // fork test's chat writes one too, to `mock`).
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish(),
    )
    .unwrap();
    let (claude, claude_seen) = recording(sse(ANTHROPIC_BODY), Duration::ZERO).await;
    let (mock, mock_seen) = recording(sse(OPENAI_OK_BODY), Duration::ZERO).await;
    let chats = chats_with(&format!(
        r#"
        [llm.providers.claude]
        kind = "anthropic"
        base_url = "http://{claude}"
        model = "claude-model"
        api_key_command = "echo sk-test"
        [llm.providers.local]
        kind = "openai-compatible"
        base_url = "http://{mock}/v1"
        model = "test-model"
        "#
    ));
    let (session, buffer) = bound(&chats, 1);
    chats.on_buffer_options(buffer, &local_model("claude/claude-model"));
    say(&chats, &session, "one").await;
    say(&chats, &session, "two").await;
    let sent = claude_seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert!(
        sent[1].contains(SIGNATURE),
        "its own reasoning goes back: {}",
        sent[1]
    );
    assert!(
        !log.text().contains("not sent to local"),
        "nothing dropped yet: {}",
        log.text()
    );

    chats.on_buffer_options(buffer, &local_model("local/test-model"));
    chats.retry(&session).unwrap();
    until("the retry", || {
        !session.is_busy() && mock_seen.lock().unwrap().len() == 1
    })
    .await;
    let retried = mock_seen.lock().unwrap()[0].clone();
    assert!(
        !retried.contains(SIGNATURE),
        "another's reasoning is not sent: {retried}"
    );
    assert!(retried.contains("two"), "{retried}");
    let note = log.text();
    assert!(
        note.contains("reasoning written by anthropic is not sent to local"),
        "the drop is said: {note}"
    );

    // Said once per chat, not on every turn after.
    chats.retry(&session).unwrap();
    until("the second retry", || {
        !session.is_busy() && mock_seen.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(log.text().matches("not sent to local").count(), 1);
    let blocks = &session.store.snapshot().turns[3].blocks;
    assert!(blocks.iter().any(|b| matches!(b, Block::Text(_))));
}
