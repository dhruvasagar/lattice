//! LL.5: the chat driver against a local mock server -- the model, the queue,
//! failures, and cancel-on-close. The buffer half is tested in
//! `lattice-host/tests/llm_chat_buffer.rs`.

#![cfg(feature = "llm")]
#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_ai::conversation::{Block, Role, SessionStatus};
use lattice_ai::llm::chat::{ChatSession, LlmChats, LlmChatsHandle, spawn_turn_listener};
use lattice_ai::llm::config::LlmConfig;
use lattice_ai::llm::{RequestRegistry, RequestRegistryHandle};
use llm_support::*;
use tokio::io::AsyncWriteExt;

fn config_for(addr: std::net::SocketAddr, extra: &str) -> LlmConfig {
    let text = format!(
        r#"
        [llm.providers.mock]
        kind = "openai-compatible"
        base_url = "http://{addr}/v1"
        model = "test-model"
        {extra}
        "#
    );
    LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
}

fn chats_with(config: LlmConfig) -> LlmChatsHandle {
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

const WAIT: Duration = Duration::from_secs(5);

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < WAIT, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn texts(session: &ChatSession) -> Vec<(Role, String)> {
    session
        .store
        .snapshot()
        .turns
        .iter()
        .map(|t| {
            let text = t
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<String>();
            (t.role, text)
        })
        .collect()
}

fn sse(body: &str) -> Vec<u8> {
    format!("{SSE_HEAD}{body}").into_bytes()
}

#[tokio::test]
async fn a_reply_streams_into_the_model_and_the_turn_ends_idle() {
    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();
    assert_eq!(session.name, "*llm:1*");

    chats.submit(&session, "hi there".into());
    until("reply", || !session.is_busy()).await;

    let t = texts(&session);
    assert_eq!(t[0], (Role::User, "hi there".into()));
    assert_eq!(t[1], (Role::Assistant, "Héllo → 世界 ✓".into()));
    let snap = session.store.snapshot();
    assert_eq!(snap.status, SessionStatus::Idle);
    assert_eq!(snap.assistant_label(), "test-model");
    assert!(snap.error.is_none());
    // 12 in + 9 out + 4 cached, as the adapter reports them.
    assert!(snap.usage.is_some(), "usage reached the model");
}

#[tokio::test]
async fn cost_is_shown_only_when_both_prices_are_configured() {
    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(
        addr,
        "input_cost_per_mtok = 3.0\noutput_cost_per_mtok = 15.0",
    ));
    let session = chats.create();
    chats.submit(&session, "x".into());
    until("done", || !session.is_busy()).await;
    let cost = session.store.snapshot().usage.unwrap().cost.unwrap();
    assert!(cost.amount > 0.0);
    assert_eq!(cost.currency, "USD");

    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(addr, "input_cost_per_mtok = 3.0"));
    let session = chats.create();
    chats.submit(&session, "x".into());
    until("done", || !session.is_busy()).await;
    assert!(session.store.snapshot().usage.unwrap().cost.is_none());
}

/// Review focus 2: a prompt sent while a reply streams is queued and shown, then
/// sent in order, and the first reply is never split by it.
#[tokio::test]
async fn sending_while_streaming_queues_shows_it_and_sends_in_order() {
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = bodies.clone();
    let addr = serve(move |req, mut s| {
        let seen = seen.clone();
        async move {
            let n = {
                let mut g = seen.lock().unwrap();
                g.push(req);
                g.len()
            };
            if n == 1 {
                // Hold the first reply so the second prompt arrives mid-stream.
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            let _ = s.write_all(&sse(OPENAI_OK_BODY)).await;
            let _ = s.shutdown().await;
        }
    })
    .await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();

    chats.submit(&session, "first".into());
    chats.submit(&session, "second".into());
    chats.submit(&session, "third".into());

    // Shown, not dropped, and not yet a turn.
    let snap = session.store.snapshot();
    assert_eq!(snap.queued, vec!["second".to_string(), "third".to_string()]);
    assert_eq!(snap.turns.len(), 1, "queued prompts are not turns yet");
    assert_eq!(snap.submission_count(), 3);
    let shown = lattice_ai::conversation::render_conversation(&snap);
    assert!(shown.contains("you (queued):\nsecond\n"), "{shown}");

    until("all three replies", || {
        texts(&session).len() == 6 && !session.is_busy()
    })
    .await;

    let t = texts(&session);
    let roles: Vec<Role> = t.iter().map(|(r, _)| *r).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant
        ],
        "each reply sits under its own question: {t:?}"
    );
    assert_eq!(t[0].1, "first");
    assert_eq!(t[2].1, "second");
    assert_eq!(t[4].1, "third");
    assert!(session.store.snapshot().queued.is_empty());

    // The requests went out in order, each carrying the history before it.
    let sent = bodies.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert!(sent[0].contains("first") && !sent[0].contains("second"));
    assert!(sent[1].contains("first") && sent[1].contains("second"));
    assert!(sent[2].contains("third"));
}

#[tokio::test]
async fn the_second_request_carries_the_first_reply_as_history() {
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = bodies.clone();
    let addr = serve(move |req, mut s| {
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(req);
            let _ = s.write_all(&sse(OPENAI_OK_BODY)).await;
            let _ = s.shutdown().await;
        }
    })
    .await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();
    chats.submit(&session, "one".into());
    until("first", || !session.is_busy()).await;
    chats.submit(&session, "two".into());
    until("second", || {
        !session.is_busy() && texts(&session).len() == 4
    })
    .await;

    let sent = bodies.lock().unwrap().clone();
    assert!(sent[1].contains("Héllo → 世界 ✓"), "{}", sent[1]);
    assert!(sent[1].contains("two"));
}

#[tokio::test]
async fn a_missing_key_shows_an_error_and_the_chat_stays_usable() {
    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(
        addr,
        r#"api_key_env = "LATTICE_LL5_SURELY_UNSET_VAR""#,
    ));
    let session = chats.create();
    chats.submit(&session, "hello".into());
    until("error", || !session.is_busy()).await;

    let snap = session.store.snapshot();
    let err = snap.error.clone().expect("an error is shown");
    assert!(err.contains("LATTICE_LL5_SURELY_UNSET_VAR"), "{err}");
    assert_eq!(snap.status, SessionStatus::Idle);
    let shown = lattice_ai::conversation::render_conversation(&snap);
    assert!(shown.contains("! environment variable"), "{shown}");
    // The prompt is still a user turn, not lost.
    assert_eq!(texts(&session)[0], (Role::User, "hello".into()));
    // And it is usable: a second send is accepted (and fails the same way).
    chats.submit(&session, "again".into());
    until("second error", || !session.is_busy()).await;
    assert_eq!(texts(&session).len(), 2);
}

#[tokio::test]
async fn no_configured_provider_opens_with_the_reason_in_the_buffer() {
    let chats = chats_with(LlmConfig::default());
    let session = chats.create();
    let err = session.store.snapshot().error.unwrap();
    assert!(err.contains("llm.providers"), "{err}");
    chats.submit(&session, "hi".into());
    until("settled", || !session.is_busy()).await;
    assert!(
        session
            .store
            .snapshot()
            .error
            .unwrap()
            .contains("llm.providers")
    );
}

#[tokio::test]
async fn a_server_error_is_shown_not_swallowed() {
    let addr = serve(|_, mut s| async move {
        let body = r#"{"error":{"message":"overloaded"}}"#;
        let raw = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(raw.as_bytes()).await;
    })
    .await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();
    chats.submit(&session, "x".into());
    until("failure", || !session.is_busy()).await;
    let err = session.store.snapshot().error.unwrap();
    assert!(err.contains("500") && err.contains("overloaded"), "{err}");
}

/// Closing the buffer cancels the request in flight; interrupt does the same
/// without closing.
#[tokio::test]
async fn releasing_or_interrupting_cancels_the_stream() {
    // Headers and one event, then silence.
    let addr = serve(|_, mut s| async move {
        let _ = s
            .write_all(format!("{SSE_HEAD}data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"par\"}}}}]}}\n\n").as_bytes())
            .await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;
    let chats = chats_with(config_for(addr, ""));

    let a = chats.create();
    let (_s, gen_a) = chats.bind(&a.name, lattice_core::BufferId(7)).unwrap();
    chats.submit(&a, "q".into());
    until("streaming", || texts(&a).len() == 2).await;
    chats.interrupt(&a);
    until("interrupt ends the turn", || !a.is_busy()).await;
    assert_eq!(a.store.snapshot().status, SessionStatus::Idle);

    // Release: a fresh turn is cancelled by the buffer closing.
    chats.submit(&a, "again".into());
    until("streaming again", || texts(&a).len() == 4).await;
    chats.release(lattice_core::BufferId(7), gen_a);
    until("release ends the turn", || !a.is_busy()).await;
    assert!(chats.for_buffer(lattice_core::BufferId(7)).is_none());
}

/// A stale guard (the buffer was re-bound since) must not tear down the chat.
#[tokio::test]
async fn a_stale_release_does_not_unbind_the_new_binding() {
    let chats = chats_with(LlmConfig::default());
    let a = chats.create();
    let b = lattice_core::BufferId(3);
    let (_, old) = chats.bind(&a.name, b).unwrap();
    let (_, new) = chats.bind(&a.name, b).unwrap();
    chats.release(b, old);
    assert!(chats.for_buffer(b).is_some(), "the new binding survives");
    chats.release(b, new);
    assert!(chats.for_buffer(b).is_none());
}

#[tokio::test]
async fn two_chats_do_not_cross() {
    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(addr, ""));
    let a = chats.create();
    let b = chats.create();
    assert_ne!(a.name, b.name);
    chats.submit(&a, "for a".into());
    chats.submit(&b, "for b".into());
    until("both", || {
        !a.is_busy() && !b.is_busy() && texts(&a).len() == 2 && texts(&b).len() == 2
    })
    .await;
    assert_eq!(texts(&a)[0].1, "for a");
    assert_eq!(texts(&b)[0].1, "for b");
}

/// A failed reply holds the queue: the error shows, the queued prompts stay
/// visible, nothing is sent on top of the failed turn, and the next explicit
/// send resumes the queue in order without sending the failed turn as history.
#[tokio::test]
async fn a_failed_reply_holds_the_queue_until_the_next_send() {
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = bodies.clone();
    let addr = serve(move |req, mut s| {
        let seen = seen.clone();
        async move {
            let n = {
                let mut g = seen.lock().unwrap();
                g.push(req);
                g.len()
            };
            if n == 1 {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let body = r#"{"error":{"message":"overloaded"}}"#;
                let raw = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(raw.as_bytes()).await;
            } else {
                let _ = s.write_all(&sse(OPENAI_OK_BODY)).await;
                let _ = s.shutdown().await;
            }
        }
    })
    .await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();
    chats.submit(&session, "first".into());
    chats.submit(&session, "second".into());
    until("the failure", || {
        !session.is_busy() && session.store.snapshot().error.is_some()
    })
    .await;
    // Give a wrongly-sent follow-up time to show itself.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snap = session.store.snapshot();
    assert_eq!(snap.queued, vec!["second".to_string()], "queue is held");
    assert!(snap.error.is_some(), "the error stays shown");
    assert_eq!(texts(&session), vec![(Role::User, "first".to_string())]);
    assert_eq!(bodies.lock().unwrap().len(), 1, "nothing sent on top");

    // The next explicit send resumes the queue in order.
    chats.submit(&session, "third".into());
    until("both replies", || {
        texts(&session).len() == 5 && !session.is_busy()
    })
    .await;
    let t = texts(&session);
    assert_eq!(t[1].1, "second");
    assert_eq!(t[2].0, Role::Assistant);
    assert_eq!(t[3].1, "third");
    let sent = bodies.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert!(
        sent[1].contains("second") && !sent[1].contains("first"),
        "the failed turn is not history: {}",
        sent[1]
    );
}

/// An ask sent into the chat from another buffer (`destination=`) while a
/// failure holds the queue goes behind the held prompts, not ahead of them.
#[tokio::test]
async fn a_source_sent_into_a_held_queue_waits_its_turn() {
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = bodies.clone();
    let addr = serve(move |req, mut s| {
        let seen = seen.clone();
        async move {
            let n = {
                let mut g = seen.lock().unwrap();
                g.push(req);
                g.len()
            };
            if n == 1 {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let body = r#"{"error":{"message":"overloaded"}}"#;
                let raw = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(raw.as_bytes()).await;
            } else {
                let _ = s.write_all(&sse(OPENAI_OK_BODY)).await;
                let _ = s.shutdown().await;
            }
        }
    })
    .await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();
    chats.submit(&session, "first".into());
    chats.submit(&session, "second".into());
    until("the failure", || {
        !session.is_busy() && session.store.snapshot().error.is_some()
    })
    .await;

    chats.submit_source(
        &session,
        lattice_ai::conversation::Block::Source {
            label: "a.rs:1".into(),
            lang: "rust".into(),
            text: "fn a() {}".into(),
            link: None,
        },
        "explain".into(),
        Default::default(),
    );
    until("both replies", || {
        texts(&session).len() == 5 && !session.is_busy()
    })
    .await;
    let t = texts(&session);
    assert_eq!(t[1].1, "second", "the held prompt goes first: {t:?}");
    assert!(t[3].1.contains("fn a() {}"), "{t:?}");
}

/// The queue and `busy` are decided together: prompts submitted from many
/// threads while turns end are all eventually sent, none stranded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_submits_never_strand_a_prompt() {
    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(addr, ""));
    let session = chats.create();
    let mut joins = Vec::new();
    for i in 0..12 {
        let (chats, session) = (chats.clone(), session.clone());
        joins.push(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(i * 7)).await;
            chats.submit(&session, format!("p{i}"));
        }));
    }
    for j in joins {
        j.await.unwrap();
    }
    until("every prompt sent and answered", || {
        let snap = session.store.snapshot();
        !session.is_busy() && snap.queued.is_empty() && snap.turns.len() == 24
    })
    .await;
    assert_eq!(session.store.snapshot().status, SessionStatus::Idle);
}

/// The key command runs once per chat, not once per turn.
#[tokio::test]
async fn the_key_command_runs_once_per_chat() {
    let log = std::env::temp_dir().join(format!("lattice-ll5-keyruns-{}", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let addr = serve_bytes(sse(OPENAI_OK_BODY)).await;
    let chats = chats_with(config_for(
        addr,
        &format!(
            "api_key_command = 'echo run >> {}; echo sk-test'",
            log.display()
        ),
    ));
    let session = chats.create();
    chats.submit(&session, "one".into());
    until("first reply", || {
        !session.is_busy() && texts(&session).len() == 2
    })
    .await;
    chats.submit(&session, "two".into());
    until("second reply", || {
        !session.is_busy() && texts(&session).len() == 4
    })
    .await;
    let runs = std::fs::read_to_string(&log).unwrap().lines().count();
    let _ = std::fs::remove_file(&log);
    assert_eq!(runs, 1, "the key was fetched again");
}

/// Reactivating the mode after its guard dropped rebinds the same chat, and a
/// chat whose buffer never activates does not accumulate.
#[tokio::test]
async fn rebinding_after_release_works_and_unbound_chats_are_bounded() {
    let chats = chats_with(LlmConfig::default());
    let a = chats.create();
    let b = lattice_core::BufferId(5);
    let (_, g1) = chats.bind(&a.name, b).unwrap();
    chats.release(b, g1);
    assert!(chats.for_buffer(b).is_none());
    let (again, g2) = chats.bind(&a.name, b).expect("reactivation rebinds");
    assert!(Arc::ptr_eq(&again, &a));
    assert!(chats.for_buffer(b).is_some());
    chats.release(b, g2);

    for _ in 0..100 {
        chats.create();
    }
    assert!(chats.unbound_count() <= 32, "{}", chats.unbound_count());
}
