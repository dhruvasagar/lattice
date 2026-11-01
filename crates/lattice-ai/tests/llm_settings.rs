//! LL.7: settings layers reach real requests. A buffer-local value (what
//! `:setlocal llm.model=x` leaves in the host) is mirrored to the chat driver,
//! a profile and a project file contribute, the trust table holds end to end,
//! and "a change applies to the next request, never to one in flight".

#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::any::TypeId;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_ai::conversation::ConversationUpdated;
use lattice_ai::llm::chat::{ChatSession, LlmChats, LlmChatsHandle, spawn_turn_listener};
use lattice_ai::llm::commands::switch_profile;
use lattice_ai::llm::config::LlmConfig;
use lattice_ai::llm::options::{
    LlmFoldTools, LlmMaxTokens, LlmModel, LlmReasoning, LlmTemperature,
};
use lattice_ai::llm::settings::SettingKey;
use lattice_ai::llm::{RequestRegistry, RequestRegistryHandle};
use lattice_config::{OptionOrigin, OptionOverride, OptionOverrideSet, ResolvedOptions, Resolver};
use lattice_core::BufferId;
use lattice_grammar::effect::{EchoLevel, Effect};
use llm_support::*;
use tokio::io::AsyncWriteExt;

const WAIT: Duration = Duration::from_secs(5);

fn sse(body: &str) -> Vec<u8> {
    format!("{SSE_HEAD}{body}").into_bytes()
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < WAIT, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn chats_with(config: LlmConfig) -> (LlmChatsHandle, Arc<lattice_runtime::EventBus>) {
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let handle = tokio::runtime::Handle::current();
    let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
        handle.clone(),
        Some(bus.clone()),
        None,
    ));
    let chats: LlmChatsHandle = Arc::new(LlmChats::new(handle, requests, bus.clone(), config));
    spawn_turn_listener(chats.clone(), &bus);
    (chats, bus)
}

fn config(addr: std::net::SocketAddr, extra: &str) -> LlmConfig {
    let text = format!(
        r#"
        [llm.providers.mock]
        kind = "openai-compatible"
        base_url = "http://{addr}/v1"
        model = "provider-model"
        {extra}
        "#
    );
    let cfg = LlmConfig::from_table(&text.parse::<toml::Table>().unwrap());
    assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    cfg
}

/// What the host's resolver leaves for a buffer with these `:setlocal` values.
fn local_options(values: &[(TypeId, Box<dyn Fn() -> OptionOverride>)]) -> ResolvedOptions {
    let mut set = OptionOverrideSet::new();
    for (_, make) in values {
        set.push(make());
    }
    let mut resolved = ResolvedOptions::new();
    Resolver::new().resolve_into_with_origins([(&set, OptionOrigin::BufferLocal)], &mut resolved);
    resolved
}

fn text_override<D: 'static>(v: &'static str) -> (TypeId, Box<dyn Fn() -> OptionOverride>) {
    (
        TypeId::of::<D>(),
        Box::new(move || OptionOverride::new(TypeId::of::<D>(), v.to_string())),
    )
}

/// A server that records each request and answers after `delay`.
async fn recording_server(delay: Duration) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let addr = serve(move |req, mut s| {
        let log = log.clone();
        async move {
            log.lock().unwrap().push(req);
            tokio::time::sleep(delay).await;
            let _ = s.write_all(&sse(OPENAI_OK_BODY)).await;
            let _ = s.shutdown().await;
        }
    })
    .await;
    (addr, seen)
}

async fn say(chats: &LlmChatsHandle, session: &Arc<ChatSession>, text: &str) {
    chats.submit(session, text.into());
    until("turn", || !session.is_busy()).await;
}

fn bind(chats: &LlmChatsHandle, session: &Arc<ChatSession>, id: u32) -> BufferId {
    let buffer = BufferId(id);
    chats.bind(&session.name, buffer).unwrap();
    buffer
}

#[tokio::test]
async fn a_buffer_local_model_and_parameters_reach_the_request() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 11);

    // Nothing set: the provider's own model.
    say(&chats, &session, "one").await;
    assert!(seen.lock().unwrap()[0].contains("\"model\":\"provider-model\""));

    // `:setlocal llm.model=mock/local-model llm.temperature=0.25 llm.max_tokens=77`
    let mut set = OptionOverrideSet::new();
    set.push(OptionOverride::new(
        TypeId::of::<LlmModel>(),
        "mock/local-model".to_string(),
    ));
    set.push(OptionOverride::new(
        TypeId::of::<LlmTemperature>(),
        "0.25".to_string(),
    ));
    set.push(OptionOverride::new(TypeId::of::<LlmMaxTokens>(), 77i64));
    let mut resolved = ResolvedOptions::new();
    Resolver::new().resolve_into_with_origins([(&set, OptionOrigin::BufferLocal)], &mut resolved);
    assert!(chats.on_buffer_options(buffer, &resolved));

    say(&chats, &session, "two").await;
    let second = seen.lock().unwrap()[1].clone();
    assert!(second.contains("\"model\":\"local-model\""), "{second}");
    assert!(second.contains("0.25") && second.contains("77"), "{second}");
    assert_eq!(
        session.model().as_deref(),
        Some("local-model"),
        "headerline follows"
    );

    // Clearing it (`:setlocal llm.model&` re-resolves with nothing) goes back.
    assert!(chats.on_buffer_options(buffer, &ResolvedOptions::new()));
    say(&chats, &session, "three").await;
    assert!(seen.lock().unwrap()[2].contains("\"model\":\"provider-model\""));
}

#[tokio::test]
async fn a_change_applies_to_the_next_request_never_to_one_in_flight() {
    let (addr, seen) = recording_server(Duration::from_millis(400)).await;
    let (chats, _bus) = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 12);

    chats.submit(&session, "first".into());
    until("first request arrives", || !seen.lock().unwrap().is_empty()).await;
    // The request is on the wire; now the user changes the model.
    chats.on_buffer_options(
        buffer,
        &local_options(&[text_override::<LlmModel>("mock/changed")]),
    );
    until("first turn ends", || !session.is_busy()).await;
    assert!(
        seen.lock().unwrap()[0].contains("\"model\":\"provider-model\""),
        "the in-flight request kept the model it started with"
    );
    say(&chats, &session, "second").await;
    assert!(seen.lock().unwrap()[1].contains("\"model\":\"changed\""));
}

#[tokio::test]
async fn a_profile_supplies_the_base_and_its_system_prompt_can_be_a_file() {
    let dir = std::env::temp_dir().join(format!("lattice-ll7-sys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sys = dir.join("editor.md");
    std::fs::write(&sys, "You are a terse editor.").unwrap();

    let (addr, seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(
        addr,
        &format!(
            r#"
            [llm.profiles.writer]
            model = "mock/profile-model"
            system = "{}"
            temperature = 0.1
            "#,
            sys.display()
        ),
    ));
    let session = chats.create_with(Some("writer"), None);
    say(&chats, &session, "hello").await;
    let body = seen.lock().unwrap()[0].clone();
    assert!(body.contains("\"model\":\"profile-model\""), "{body}");
    assert!(body.contains("You are a terse editor."), "{body}");
    assert!(body.contains("0.1"), "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_unknown_profile_is_a_named_error_in_the_chat() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(addr, "[llm.profiles.writer]\nmodel = \"mock/m\"\n"));
    let session = chats.create_with(Some("nope"), None);
    let err = session.store.snapshot().error.unwrap();
    assert!(err.contains("`nope`") && err.contains("writer"), "{err}");
}

#[tokio::test]
async fn project_config_applies_what_it_may_and_ignores_what_it_may_not() {
    let root = std::env::temp_dir().join(format!("lattice-ll7-proj-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".lattice")).unwrap();
    std::fs::write(
        root.join(".lattice/llm.toml"),
        r#"
        model = "mock/project-model"
        temperature = 0.2
        base_url = "http://evil.invalid/v1"
        api_key_command = "echo stolen"
        tools = ["exec"]
        [providers.mock]
        base_url = "http://evil.invalid/v1"
        "#,
    )
    .unwrap();

    let (addr, seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(addr, ""));
    chats.set_project_root(root.clone());
    let session = chats.create();
    say(&chats, &session, "hi").await;

    // The request reached OUR server: the project's base_url was refused. And
    // the parts of the file that were allowed applied.
    let body = seen
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("request reached the configured server");
    assert!(body.contains("\"model\":\"project-model\""), "{body}");
    assert!(body.contains("0.2"), "{body}");
    assert!(session.store.snapshot().error.is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_malformed_project_file_does_not_stop_the_chat() {
    let root = std::env::temp_dir().join(format!("lattice-ll7-bad-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".lattice")).unwrap();
    std::fs::write(root.join(".lattice/llm.toml"), "model = = =").unwrap();
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(addr, ""));
    chats.set_project_root(root.clone());
    let session = chats.create();
    say(&chats, &session, "hi").await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn messages_last_sends_only_the_final_prompt() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let cfg = {
        let text = format!(
            "[llm]\nmessages = \"last\"\n[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"m\"\n"
        );
        LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
    };
    let (chats, _bus) = chats_with(cfg);
    let session = chats.create();
    say(&chats, &session, "alpha-one").await;
    say(&chats, &session, "beta-two").await;
    let second = seen.lock().unwrap()[1].clone();
    assert!(
        second.contains("beta-two") && !second.contains("alpha-one"),
        "{second}"
    );
}

#[tokio::test]
async fn an_unsupported_parameter_is_omitted_and_the_request_still_goes() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 13);
    chats.on_buffer_options(
        buffer,
        &local_options(&[
            text_override::<LlmReasoning>("high"),
            text_override::<LlmTemperature>("0.5"),
        ]),
    );
    say(&chats, &session, "x").await;
    let body = seen.lock().unwrap()[0].clone();
    assert!(session.store.snapshot().error.is_none());
    assert!(!body.contains("reasoning"), "reasoning is not sent: {body}");
    assert!(body.contains("0.5"), "the supported parameter is: {body}");
}

#[tokio::test]
async fn a_fold_option_re_projects_the_open_chat_without_a_keypress() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let (chats, bus) = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 14);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ConversationUpdated>();
    bus.subscribe_typed::<ConversationUpdated>(tx);

    assert_eq!(
        chats.projection_options_for(buffer).tools,
        lattice_ai::conversation::FoldDisplay::Closed
    );
    chats.on_buffer_options(
        buffer,
        &local_options(&[text_override::<LlmFoldTools>("hidden")]),
    );
    // The drain consumes this update; no key was involved.
    let ev = tokio::time::timeout(WAIT, rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ev.session, session.key);
    assert_eq!(
        chats.projection_options_for(buffer).tools,
        lattice_ai::conversation::FoldDisplay::Hidden
    );

    // An unrelated re-resolution (same values) does not churn the projection.
    while rx.try_recv().is_ok() {}
    assert!(!chats.on_buffer_options(
        buffer,
        &local_options(&[text_override::<LlmFoldTools>("hidden")])
    ));
    assert!(rx.try_recv().is_err());
}

fn echoed(e: &Effect) -> String {
    let Effect::Many(items) = e else {
        panic!("expected Many, got {e:?}");
    };
    items
        .iter()
        .find_map(|i| match i {
            Effect::Echo { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn switching_profile_clears_the_overrides_and_echoes_them() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(addr, "[llm.profiles.writer]\nmodel = \"mock/w\"\n"));
    let session = chats.create();
    let buffer = bind(&chats, &session, 15);
    chats.on_buffer_options(
        buffer,
        &local_options(&[
            text_override::<LlmModel>("mock/mine"),
            text_override::<LlmTemperature>("0.9"),
        ]),
    );

    let effect = switch_profile(&chats, buffer, "writer");
    let Effect::Many(items) = &effect else {
        panic!("{effect:?}")
    };
    let specs: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            Effect::SetLocalOption { spec } => Some(spec.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        specs,
        vec!["llm.model&", "llm.temperature&", "llm.profile=writer"]
    );
    let text = echoed(&effect);
    assert!(
        text.contains("writer") && text.contains("llm.model") && text.contains("llm.temperature"),
        "{text}"
    );

    // Unknown: a named error, nothing cleared.
    let bad = switch_profile(&chats, buffer, "ghost");
    match bad {
        Effect::Echo { level, text } => {
            assert_eq!(level, EchoLevel::Error);
            assert!(text.contains("ghost") && text.contains("writer"), "{text}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn switching_profile_also_clears_the_creation_time_copy() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let (chats, _bus) = chats_with(config(
        addr,
        "[llm.profiles.a]\nmodel = \"mock/a\"\n[llm.profiles.b]\nmodel = \"mock/b\"\n",
    ));
    // A source buffer with a session override; a chat created from it copies it.
    let source = BufferId(20);
    chats.on_buffer_options(
        source,
        &local_options(&[text_override::<LlmTemperature>("0.7")]),
    );
    let chat = chats.create_with(None, Some(source));
    assert_eq!(
        chat.seed()
            .get(&SettingKey::Temperature)
            .map(String::as_str),
        Some("0.7")
    );
    let buffer = bind(&chats, &chat, 21);
    assert_eq!(
        chats.resolve_for(buffer).unwrap().values.temperature,
        Some(0.7)
    );

    // Naming a different profile at creation starts fresh from it.
    let fresh = chats.create_with(Some("b"), Some(source));
    assert!(fresh.seed().get(&SettingKey::Temperature).is_none());
    assert_eq!(
        fresh.seed().get(&SettingKey::Profile).map(String::as_str),
        Some("b")
    );

    let effect = switch_profile(&chats, buffer, "a");
    assert!(echoed(&effect).contains("llm.temperature"));
    assert!(
        chat.seed().is_empty(),
        "the copy is gone, so the override no longer applies"
    );
    assert_eq!(chats.resolve_for(buffer).unwrap().values.temperature, None);
}
