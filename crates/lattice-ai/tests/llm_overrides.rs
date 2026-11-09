//! LL.8: overriding the settings on the go. The menu, the scopes it writes to,
//! command arguments, the `@profile` cookie and its confirm, and the system-prompt
//! buffer's apply. The buffer half of `:w` is tested in
//! `lattice-host/tests/llm_system_write.rs`.

#![cfg(feature = "llm")]
#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::any::TypeId;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_ai::llm::chat::{
    ChatSession, LlmChats, LlmChatsHandle, MenuScope, SendPlan, spawn_turn_listener,
};
use lattice_ai::llm::commands::{parse_request_args, system_target};
use lattice_ai::llm::config::LlmConfig;
use lattice_ai::llm::menu::{self, MenuIds};
use lattice_ai::llm::modes::chat::LlmChatMode;
use lattice_ai::llm::options::LlmModel;
use lattice_ai::llm::settings::{RequestOverrides, SettingKey};
use lattice_ai::llm::{RequestRegistry, RequestRegistryHandle};
use lattice_config::{OptionOrigin, OptionOverride, OptionOverrideSet, ResolvedOptions, Resolver};
use lattice_core::BufferId;
use lattice_grammar::CommandRegistry;
use lattice_grammar::args::{ArgValue, Args};
use lattice_grammar::effect::Effect;
use lattice_mode::{ActionContext, Mode, ServiceRegistry};
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

const PROFILES: &str = r#"
    [llm.profiles.writer]
    model = "mock/writer-model"
    [llm.profiles.coding]
    model = "mock/coding-model"
    tools = ["exec"]
    [llm.profiles.plain]
    model = "mock/plain-model"
    tools = []
"#;

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

fn bind(chats: &LlmChatsHandle, session: &Arc<ChatSession>, id: u32) -> BufferId {
    let buffer = BufferId(id);
    chats.bind(&session.name, buffer).unwrap();
    buffer
}

async fn say(chats: &LlmChatsHandle, session: &Arc<ChatSession>, text: &str) {
    chats.submit(session, text.into());
    until("turn", || !session.is_busy()).await;
}

/// What the host's resolver leaves for a buffer with these `:setlocal` values.
fn local_model(value: &'static str) -> ResolvedOptions {
    let mut set = OptionOverrideSet::new();
    set.push(OptionOverride::new(
        TypeId::of::<LlmModel>(),
        value.to_string(),
    ));
    let mut resolved = ResolvedOptions::new();
    Resolver::new().resolve_into_with_origins([(&set, OptionOrigin::BufferLocal)], &mut resolved);
    resolved
}

fn menu_ids() -> MenuIds {
    let mut registry = CommandRegistry::new();
    menu::register_actions(&mut registry);
    MenuIds::resolve(&registry).unwrap()
}

/// Run a mode's handler by action name, as the host would.
fn fire(
    mode: &dyn Fn() -> Vec<lattice_mode::ActionHandlerContribution>,
    name: &str,
    buffer: BufferId,
    args: Args,
    prompt: Option<&str>,
) -> Option<Effect> {
    let handler = mode()
        .into_iter()
        .find(|c| c.action_name == name)
        .unwrap_or_else(|| panic!("no handler {name}"))
        .handler;
    let services = ServiceRegistry::new();
    let events = lattice_runtime::EventBus::new();
    handler(&ActionContext {
        buffer_id: lattice_protocol::ids::BufferId::new(buffer.0 as u64),
        cursor: lattice_protocol::position::Position::ZERO,
        selection: None,
        services: &services,
        events: &events,
        prompt_value: prompt,
        args,
        buffer_locals: None,
    })
}

fn items(spec: &lattice_picker::TransientSpec) -> Vec<&lattice_picker::TransientItem> {
    spec.groups.iter().flat_map(|g| g.items.iter()).collect()
}

fn row<'a>(
    spec: &'a lattice_picker::TransientSpec,
    label: &str,
) -> &'a lattice_picker::TransientItem {
    items(spec)
        .into_iter()
        .find(|i| i.label == label)
        .unwrap_or_else(|| panic!("no row {label}"))
}

// ---------------------------------------------------------------------------
// The menu
// ---------------------------------------------------------------------------

/// The project layer is read off-thread and cached, so the menu (built on the
/// editor thread) can show it: value and origin both.
#[tokio::test]
async fn the_menu_shows_a_project_models_value_and_origin() {
    let root = std::env::temp_dir().join(format!("lattice-ll8-menu-proj-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".lattice")).unwrap();
    std::fs::write(
        root.join(".lattice/llm.toml"),
        "model = \"mock/project-model\"\n",
    )
    .unwrap();
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    chats.set_project_root(root.clone());
    let ids = menu_ids();

    // Starting a chat refreshes the cache, off the calling thread.
    let session = chats.create();
    let buffer = bind(&chats, &session, 57);
    until("project layer cached", || {
        let spec = menu::build(&chats, &ids, Some(buffer), "x");
        row(&spec, "model").description.contains("project-model")
    })
    .await;
    let spec = menu::build(&chats, &ids, Some(buffer), "x");
    let model = row(&spec, "model");
    assert!(
        model.description.contains("(project)"),
        "origin is the project: {}",
        model.description
    );

    // A session value still beats it, and says so.
    chats.on_buffer_options(buffer, &local_model("mock/local"));
    let spec = menu::build(&chats, &ids, Some(buffer), "x");
    assert!(
        !row(&spec, "model").description.contains("(project)"),
        "{}",
        row(&spec, "model").description
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_menu_shows_each_value_with_its_origin_and_names_the_session() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(
        addr,
        &format!("temperature = 0.4\n{PROFILES}\n[llm]\ndefault_profile = \"writer\""),
    ));
    let ids = menu_ids();

    // A buffer with no chat: the session the next request would create.
    let spec = menu::build(&chats, &ids, Some(BufferId(40)), "foo.rs");
    assert!(
        spec.title.contains("new session from foo.rs"),
        "{}",
        spec.title
    );
    let model = row(&spec, "model");
    assert!(
        model.description.contains("mock/writer-model"),
        "{}",
        model.description
    );
    assert!(
        model.description.contains("profile: writer"),
        "{}",
        model.description
    );
    assert_eq!(model.key, vec!["m".to_string()]);

    // A chat with a buffer-local model: the origin follows the value.
    let session = chats.create();
    let buffer = bind(&chats, &session, 41);
    chats.on_buffer_options(buffer, &local_model("mock/local"));
    let spec = menu::build(&chats, &ids, Some(buffer), "ignored");
    assert!(spec.title.contains(&session.name), "{}", spec.title);
    let model = row(&spec, "model");
    assert!(
        model.description.contains("mock/local"),
        "{}",
        model.description
    );
    assert!(
        model.description.contains("buffer-local"),
        "{}",
        model.description
    );
    assert!(
        row(&spec, "system prompt").description.contains("default"),
        "an unset value says it is the default"
    );
}

#[tokio::test]
async fn the_scope_toggle_stays_toggled_and_the_menu_says_so() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    let ids = menu_ids();
    let buffer = BufferId(42);
    let handlers = {
        let chats = chats.clone();
        move || menu::handlers(&chats)
    };

    let spec = menu::build(&chats, &ids, Some(buffer), "x");
    assert!(spec.groups[0].label.contains("session"));

    // `=` switches, and reopens the menu where the user was.
    let effect = fire(&handlers, menu::ACTION_SCOPE, buffer, Args::None, None).unwrap();
    let Effect::Many(parts) = effect else {
        panic!("expected echo + reopen");
    };
    assert!(
        parts.iter().any(
            |e| matches!(e, Effect::OpenTransient { source, .. } if source == menu::MENU_SOURCE)
        ),
        "the menu reopens"
    );
    assert_eq!(chats.menu_scope(buffer), MenuScope::NextRequest);
    let spec = menu::build(&chats, &ids, Some(buffer), "x");
    assert!(
        spec.groups[0].label.contains("next request"),
        "{}",
        spec.groups[0].label
    );

    // It stays: building again, or acting, does not flip it back.
    menu::apply_choice(&chats, buffer, SettingKey::Model, "mock/x");
    assert_eq!(chats.menu_scope(buffer), MenuScope::NextRequest);
    // Another buffer has its own.
    assert_eq!(chats.menu_scope(BufferId(43)), MenuScope::Session);
}

#[tokio::test]
async fn session_scope_persists_and_next_request_scope_does_not() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 44);

    // Session scope: the menu emits a `:setlocal`, the host mirrors it back,
    // and every later request carries it.
    let effect = menu::apply_choice(&chats, buffer, SettingKey::Model, "mock/session-model");
    let Effect::Many(parts) = effect else {
        panic!("expected setlocal + echo");
    };
    assert!(matches!(
        &parts[0],
        Effect::SetLocalOption { spec } if spec == "llm.model=mock/session-model"
    ));
    chats.on_buffer_options(buffer, &local_model("mock/session-model"));
    say(&chats, &session, "one").await;
    say(&chats, &session, "two").await;
    {
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("\"model\":\"session-model\""));
        assert!(seen[1].contains("\"model\":\"session-model\""));
    }

    // Next-request scope: held in the chat driver, taken by one submission.
    chats.toggle_menu_scope(buffer);
    let effect = menu::apply_choice(&chats, buffer, SettingKey::Temperature, "0.9");
    assert!(
        matches!(effect, Effect::Echo { .. }),
        "no :setlocal in this scope"
    );
    assert_eq!(
        chats
            .next_request(buffer)
            .get(&SettingKey::Temperature)
            .map(String::as_str),
        Some("0.9")
    );
    say(&chats, &session, "three").await;
    say(&chats, &session, "four").await;
    let seen = seen.lock().unwrap();
    assert!(
        seen[2].contains("\"temperature\":"),
        "the next request carried it: {}",
        seen[2]
    );
    assert!(
        !seen[3].contains("\"temperature\""),
        "the request after did not"
    );
    assert!(chats.next_request(buffer).is_empty());
}

#[tokio::test]
async fn a_menu_choice_made_during_a_stream_applies_to_the_next_turn() {
    let (addr, seen) = recording_server(Duration::from_millis(300)).await;
    let chats = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 45);
    chats.toggle_menu_scope(buffer);

    chats.submit(&session, "first".into());
    until("first on the wire", || !seen.lock().unwrap().is_empty()).await;
    menu::apply_choice(&chats, buffer, SettingKey::Model, "mock/after");
    until("first done", || !session.is_busy()).await;
    say(&chats, &session, "second").await;

    let seen = seen.lock().unwrap();
    assert!(
        seen[0].contains("\"model\":\"provider-model\""),
        "in flight is untouched"
    );
    assert!(seen[1].contains("\"model\":\"after\""), "{}", seen[1]);
}

#[tokio::test]
async fn an_invalid_choice_is_refused_with_the_reason_and_changes_nothing() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    let buffer = BufferId(46);
    let effect = menu::apply_choice(&chats, buffer, SettingKey::Temperature, "hot");
    let Effect::Echo { text, .. } = effect else {
        panic!("expected an error echo");
    };
    assert!(text.contains("llm.temperature"), "{text}");
    chats.toggle_menu_scope(buffer);
    menu::apply_choice(&chats, buffer, SettingKey::Temperature, "hot");
    assert!(chats.next_request(buffer).is_empty());
}

#[tokio::test]
async fn clear_drops_the_session_overrides_and_the_pending_choice() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 47);
    chats.on_buffer_options(buffer, &local_model("mock/m"));
    chats.set_next_request(buffer, SettingKey::Temperature, "0.1");
    let handlers = {
        let chats = chats.clone();
        move || menu::handlers(&chats)
    };
    let Effect::Many(parts) =
        fire(&handlers, menu::ACTION_CLEAR, buffer, Args::None, None).unwrap()
    else {
        panic!("expected a batch");
    };
    assert!(
        parts
            .iter()
            .any(|e| matches!(e, Effect::SetLocalOption { spec } if spec == "llm.model&"))
    );
    let Some(Effect::Echo { text, .. }) = parts.last().cloned() else {
        panic!("expected an echo");
    };
    assert!(
        text.contains("llm.model") && text.contains("llm.temperature"),
        "{text}"
    );
    assert!(chats.next_request(buffer).is_empty());
}

// ---------------------------------------------------------------------------
// Command arguments
// ---------------------------------------------------------------------------

#[test]
fn leading_key_value_words_are_overrides_and_the_rest_is_the_prompt() {
    let (set, rest) = parse_request_args("model=local/qwen temperature=0 explain this").unwrap();
    assert_eq!(
        set.get(&SettingKey::Model).map(String::as_str),
        Some("local/qwen")
    );
    assert_eq!(
        set.get(&SettingKey::Temperature).map(String::as_str),
        Some("0")
    );
    assert_eq!(rest, "explain this");

    // An unknown key starts the prompt; so does a value-less word.
    let (set, rest) = parse_request_args("x=1 is wrong").unwrap();
    assert!(set.is_empty());
    assert_eq!(rest, "x=1 is wrong");
    let (set, rest) = parse_request_args("model=a/b").unwrap();
    assert_eq!(set.len(), 1);
    assert_eq!(rest, "");
    // System is not an argument; it is a paragraph and has `:llm-system`.
    let (set, rest) = parse_request_args("system=be terse").unwrap();
    assert!(set.is_empty());
    assert_eq!(rest, "system=be terse");
}

#[test]
fn a_bad_value_for_a_known_key_is_an_error_naming_it() {
    let e = parse_request_args("temperature=hot explain").unwrap_err();
    assert!(e.contains("llm.temperature"), "{e}");
    assert!(parse_request_args("model= explain").is_err());
}

#[tokio::test]
async fn a_model_argument_applies_to_one_request_only() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    let session = chats.create();
    let (values, rest) = parse_request_args("model=mock/once temperature=0 hello").unwrap();
    chats.submit_with(
        &session,
        rest,
        RequestOverrides {
            values,
            ..RequestOverrides::default()
        },
    );
    until("first turn", || !session.is_busy()).await;
    say(&chats, &session, "again").await;

    let seen = seen.lock().unwrap();
    assert!(seen[0].contains("\"model\":\"once\""), "{}", seen[0]);
    assert!(
        seen[1].contains("\"model\":\"provider-model\""),
        "{}",
        seen[1]
    );
}

#[tokio::test]
async fn each_queued_prompt_keeps_its_own_overrides() {
    let (addr, seen) = recording_server(Duration::from_millis(250)).await;
    let chats = chats_with(config(addr, ""));
    let session = chats.create();
    let with_model = |m: &str| RequestOverrides {
        values: [(SettingKey::Model, m.to_string())].into_iter().collect(),
        ..RequestOverrides::default()
    };
    chats.submit_with(&session, "a".into(), with_model("mock/first"));
    chats.submit_with(&session, "b".into(), with_model("mock/second"));
    chats.submit_with(&session, "c".into(), RequestOverrides::default());
    until("all three sent", || {
        seen.lock().unwrap().len() == 3 && !session.is_busy()
    })
    .await;
    let seen = seen.lock().unwrap();
    assert!(seen[0].contains("\"model\":\"first\""));
    assert!(seen[1].contains("\"model\":\"second\""));
    assert!(seen[2].contains("\"model\":\"provider-model\""));
}

// ---------------------------------------------------------------------------
// @profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn at_profile_in_the_typed_text_applies_to_that_request_only() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, PROFILES));
    let session = chats.create();
    let buffer = bind(&chats, &session, 50);

    let typed = "@writer tighten this";
    let SendPlan::Profile(p) = chats.plan_send(buffer, typed) else {
        panic!("expected a profile plan");
    };
    chats.submit_with(
        &session,
        typed.into(),
        RequestOverrides {
            profile: Some(p),
            pasted_profile: true,
            ..RequestOverrides::default()
        },
    );
    until("turn", || !session.is_busy()).await;
    // The next turn has no cookie, and the earlier one in the history is not
    // scanned again.
    assert_eq!(chats.plan_send(buffer, "and now?"), SendPlan::Plain);
    say(&chats, &session, "and now?").await;

    let seen = seen.lock().unwrap();
    assert!(
        seen[0].contains("\"model\":\"writer-model\""),
        "{}",
        seen[0]
    );
    assert!(
        seen[1].contains("\"model\":\"provider-model\""),
        "{}",
        seen[1]
    );
}

/// `@writer` in a prompt is an explicit request for that one turn, so it beats
/// a session `llm.model`; the turn after falls back to the session's value.
#[tokio::test]
async fn at_profile_beats_a_session_model_for_that_turn_only() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, PROFILES));
    let session = chats.create();
    let buffer = bind(&chats, &session, 56);
    chats.on_buffer_options(buffer, &local_model("mock/session-model"));

    let typed = "@writer tighten this";
    let SendPlan::Profile(p) = chats.plan_send(buffer, typed) else {
        panic!("expected a profile plan");
    };
    chats.submit_with(
        &session,
        typed.into(),
        RequestOverrides {
            profile: Some(p),
            pasted_profile: true,
            ..RequestOverrides::default()
        },
    );
    until("turn", || !session.is_busy()).await;
    say(&chats, &session, "and now?").await;

    let seen = seen.lock().unwrap();
    assert!(
        seen[0].contains("\"model\":\"writer-model\""),
        "the profile named in the prompt wins: {}",
        seen[0]
    );
    assert!(
        seen[1].contains("\"model\":\"session-model\""),
        "the next turn is back on the session: {}",
        seen[1]
    );
}

#[tokio::test]
async fn an_unknown_cookie_is_left_in_the_text_and_sent() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, PROFILES));
    let session = chats.create();
    let buffer = bind(&chats, &session, 51);
    assert_eq!(chats.plan_send(buffer, "@unknown hello"), SendPlan::Plain);
    say(&chats, &session, "@unknown hello").await;
    let body = seen.lock().unwrap()[0].clone();
    assert!(body.contains("@unknown hello"), "{body}");
    assert!(body.contains("\"model\":\"provider-model\""));
}

#[tokio::test]
async fn selected_source_is_never_scanned_only_what_was_typed() {
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, PROFILES));
    let buffer = BufferId(52);
    // An operator passes the typed instruction alone; the source it selected
    // (which mentions @coding) is not part of what is scanned.
    let source = "// TODO: ask @coding to fix this\nfn main() {}";
    let typed = "explain";
    assert_eq!(chats.plan_send(buffer, typed), SendPlan::Plain);
    // The same words typed by the user do count.
    assert!(matches!(
        chats.plan_send(buffer, &format!("{typed} {}", &source[..0])),
        SendPlan::Plain
    ));
    assert!(matches!(
        chats.plan_send(buffer, "@coding explain"),
        SendPlan::NeedsConfirm(_)
    ));
}

#[tokio::test]
async fn a_profile_that_raises_tools_asks_first_and_one_that_does_not_does_not() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, PROFILES));
    let session = chats.create();
    let buffer = bind(&chats, &session, 53);

    // `coding` turns `exec` on, which the session does not have.
    assert_eq!(
        chats.plan_send(buffer, "@coding fix it"),
        SendPlan::NeedsConfirm("coding".into())
    );
    // `plain` narrows to nothing, `writer` names no tools: neither asks.
    assert_eq!(
        chats.plan_send(buffer, "@plain hi"),
        SendPlan::Profile("plain".into())
    );
    assert_eq!(
        chats.plan_send(buffer, "@writer hi"),
        SendPlan::Profile("writer".into())
    );

    // Nothing was sent while the question was open.
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(session.store.snapshot().submission_count(), 0);

    // The yes half sends that prompt, with that profile, once.
    let mode = LlmChatMode::new(chats.clone());
    let handlers = move || mode.action_handlers();
    let args = Args::List(vec![
        ArgValue::String("coding".into()),
        ArgValue::String("@coding fix it".into()),
    ]);
    assert!(
        fire(
            &handlers,
            "action:llm-chat-send-widened",
            buffer,
            args,
            None
        )
        .is_none()
    );
    until("turn", || !session.is_busy()).await;
    say(&chats, &session, "next").await;
    let seen = seen.lock().unwrap();
    assert!(
        seen[0].contains("\"model\":\"coding-model\""),
        "{}",
        seen[0]
    );
    assert!(
        seen[1].contains("\"model\":\"provider-model\""),
        "the grant was for one request"
    );
}

#[tokio::test]
async fn a_pasted_profile_that_does_not_widen_cannot_raise_tools() {
    // The restricted path is what `plan_send`'s Profile arm uses: the resolved
    // request keeps the session's tools whatever the profile says.
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, PROFILES));
    let session = chats.create();
    let buffer = bind(&chats, &session, 54);
    assert!(chats.profile_widens_tools(buffer, "coding"));
    assert!(!chats.profile_widens_tools(buffer, "writer"));
    assert!(!chats.profile_widens_tools(buffer, "plain"));
}

// ---------------------------------------------------------------------------
// :llm-system
// ---------------------------------------------------------------------------

#[test]
fn the_system_buffer_name_carries_its_target_and_scope() {
    use lattice_ai::llm::commands::{system_buffer_name, system_scope};
    let name = system_buffer_name(BufferId(9), 3, MenuScope::Session);
    assert_eq!(system_target(&name), Some(BufferId(9)));
    assert_eq!(system_scope(&name), Some(MenuScope::Session));
    let next = system_buffer_name(BufferId(9), 3, MenuScope::NextRequest);
    assert_eq!(system_target(&next), Some(BufferId(9)));
    assert_eq!(system_scope(&next), Some(MenuScope::NextRequest));
    assert_eq!(system_target("*llm:1*"), None);
    assert_eq!(system_scope("*llm:1*"), None);
    assert_ne!(
        system_buffer_name(BufferId(9), 4, MenuScope::Session),
        name,
        "each open is fresh"
    );
}

/// The menu's `s` row opens the form in the scope the menu was in, and `:w`
/// from that form goes where the label said.
#[tokio::test]
async fn the_system_row_applies_in_the_scope_it_was_opened_in() {
    use lattice_ai::llm::commands::system_scope;
    use lattice_ai::llm::modes::system::apply_text;
    let (addr, _seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, ""));
    let session = chats.create();
    let buffer = bind(&chats, &session, 58);
    let handlers = {
        let chats = chats.clone();
        move || menu::handlers(&chats)
    };
    let open = |scope_label: &str| {
        let Some(Effect::OpenSyntheticBuffer { name, .. }) = fire(
            &handlers,
            menu::ACTION_EDIT,
            buffer,
            Args::String("llm.system".into()),
            None,
        ) else {
            panic!("{scope_label}: the row opens a form");
        };
        name
    };

    // Session scope (the default).
    let name = open("session");
    assert_eq!(system_scope(&name), Some(MenuScope::Session));
    let Effect::Many(parts) = apply_text(&chats, &name, "Be brief.", buffer) else {
        panic!("expected a batch");
    };
    assert!(
        parts.iter().any(|e| matches!(
            e, Effect::SetLocalOption { spec } if spec == "llm.system=Be brief."
        )),
        "session scope sets the buffer-local option"
    );
    assert!(chats.next_request(buffer).is_empty());

    // Next-request scope: held for one request, no :setlocal.
    chats.toggle_menu_scope(buffer);
    let name = open("next request");
    assert_eq!(system_scope(&name), Some(MenuScope::NextRequest));
    let Effect::Many(parts) = apply_text(&chats, &name, "Be terse.", buffer) else {
        panic!("expected a batch");
    };
    assert!(
        !parts
            .iter()
            .any(|e| matches!(e, Effect::SetLocalOption { .. })),
        "next-request scope must not touch the session"
    );
    assert!(parts.iter().any(|e| matches!(
        e, Effect::Echo { text, .. } if text.contains("next request")
    )));
    assert_eq!(
        chats
            .next_request(buffer)
            .get(&SettingKey::System)
            .map(String::as_str),
        Some("Be terse.")
    );
}

// ---------------------------------------------------------------------------
// AGENTS.md (GR.4)
// ---------------------------------------------------------------------------

/// The project's `AGENTS.md` reaches the model after the configured system
/// prompt; `llm.agents_md = false` leaves it out, and so does `llm.exclude`.
#[tokio::test]
async fn agents_md_joins_the_system_prompt_unless_turned_off_or_excluded() {
    let (addr, seen) = recording_server(Duration::ZERO).await;
    let chats = chats_with(config(addr, "[llm]\nsystem = \"Be brief.\"\n"));
    let registry = Arc::new(lattice_config::ConfigRegistry::new());
    registry.init_from_linkme();
    chats.set_registry(registry.clone());
    let root = std::env::temp_dir().join(format!("lattice-agents-md-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("AGENTS.md"), "Run cargo fmt before committing.\n").unwrap();
    chats.set_project_root(root.clone());

    let session = chats.create();
    bind(&chats, &session, 60);
    say(&chats, &session, "first").await;
    registry
        .parse_and_set_command("llm.agents_md=false")
        .unwrap();
    say(&chats, &session, "second").await;
    registry
        .parse_and_set_command("llm.agents_md=true")
        .unwrap();
    registry
        .parse_and_set_command("llm.exclude=AGENTS.md")
        .unwrap();
    say(&chats, &session, "third").await;

    let seen = seen.lock().unwrap();
    assert!(
        seen[0].contains(
            "Be brief.\\n\\nThe project's instructions for AI tools, from AGENTS.md:\\n\\n\
             Run cargo fmt before committing."
        ),
        "{}",
        seen[0]
    );
    for (i, why) in [(1, "llm.agents_md=false"), (2, "llm.exclude")] {
        assert!(!seen[i].contains("cargo fmt"), "{why}: {}", seen[i]);
        assert!(seen[i].contains("Be brief."), "{why}: {}", seen[i]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
