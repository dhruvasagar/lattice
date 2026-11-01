//! Chat sessions: one per `*llm:<n>*` buffer, and the turn driver.
//!
//! A [`ChatSession`] owns a [`ConversationStore`], the model of the chat; the
//! buffer is only its projection (see `conversation::transcript`). Submitting a
//! prompt folds a user turn into the store and starts a request whose
//! [`ChatSink`] folds the streamed events back in, so every byte the user sees
//! arrives through the model and the drain, with no keystroke in between.
//!
//! [`LlmChats`] is the registry and the driver. It is the one thing the ex-command
//! (which creates a chat), the mode (which binds it to a buffer) and the chords
//! (which submit and interrupt) share, registered as a service.
//!
//! # Turn lifecycle
//!
//! `busy` is set when a turn is submitted and cleared when its request reaches a
//! terminal state, which the registry announces on the bus. A prompt submitted
//! while busy goes to the model's `queued` list and is shown in the transcript;
//! it becomes a user turn only when it is actually sent, so a streaming reply is
//! never split by a later question. Queued prompts are sent in order.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use lattice_agent::SessionKey;
use lattice_core::BufferId;
use tokio::runtime::Handle;

use super::config::{ChosenModel, LlmConfig, ProviderConfig, resolve_key};
use super::history;
use super::{
    LlmRequest, Params, Provider, RequestId, RequestRegistryHandle, RequestState, RequestTarget,
    Sink, SinkFlow, StopReason, StreamEvent,
};
use crate::conversation::{
    ConversationStore, Cost, OpaqueReasoning, Role, SessionStatus, UsageSnapshot,
};

/// Provider tag of a chat's [`SessionKey`].
pub const SESSION_PROVIDER: &str = "llm";

pub type LlmChatsHandle = Arc<LlmChats>;

/// What a chat needs to send a request.
#[derive(Clone)]
pub struct Settings {
    provider_config: ProviderConfig,
    provider: Arc<dyn Provider>,
    pub model: String,
}

impl Settings {
    fn new(chosen: ChosenModel) -> Self {
        Self {
            provider: chosen.provider.build(),
            provider_config: chosen.provider,
            model: chosen.model,
        }
    }
}

#[derive(Default)]
struct TurnState {
    busy: bool,
    request: Option<RequestId>,
    interrupted: bool,
}

pub struct ChatSession {
    pub name: String,
    pub key: SessionKey,
    pub store: ConversationStore,
    /// `Err` carries the configuration problem, shown in the buffer on send.
    settings: Result<Settings, String>,
    turn: Mutex<TurnState>,
    /// Set when the buffer closes: no queued prompt is sent after it.
    closed: AtomicBool,
}

impl ChatSession {
    pub fn model(&self) -> Option<&str> {
        self.settings.as_ref().ok().map(|s| s.model.as_str())
    }

    fn turn(&self) -> MutexGuard<'_, TurnState> {
        self.turn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True while a request is in flight or being prepared.
    pub fn is_busy(&self) -> bool {
        self.turn().busy
    }
}

#[derive(Default)]
struct Registry {
    by_name: HashMap<String, Arc<ChatSession>>,
    by_buffer: HashMap<BufferId, (u64, Arc<ChatSession>)>,
    by_request: HashMap<RequestId, Arc<ChatSession>>,
    /// Terminal states announced before their request was registered here.
    ended_early: HashMap<RequestId, RequestState>,
}

/// Bound on `ended_early`: requests this registry never owns would otherwise
/// accumulate there.
const ENDED_EARLY_CAP: usize = 256;

pub struct LlmChats {
    runtime: Handle,
    requests: RequestRegistryHandle,
    bus: Arc<lattice_runtime::EventBus>,
    config: RwLock<LlmConfig>,
    next_chat: AtomicU32,
    next_bind: AtomicU64,
    inner: Mutex<Registry>,
}

impl LlmChats {
    pub fn new(
        runtime: Handle,
        requests: RequestRegistryHandle,
        bus: Arc<lattice_runtime::EventBus>,
        config: LlmConfig,
    ) -> Self {
        Self {
            runtime,
            requests,
            bus,
            config: RwLock::new(config),
            next_chat: AtomicU32::new(1),
            next_bind: AtomicU64::new(1),
            inner: Mutex::new(Registry::default()),
        }
    }

    /// Replace the configuration new chats are created from. Chats already open
    /// keep the settings they were created with.
    pub fn set_config(&self, config: LlmConfig) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = config;
    }

    fn reg(&self) -> MutexGuard<'_, Registry> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create a chat from the current config. Cheap and non-blocking, so the
    /// ex-command can call it on the editor thread. A configuration problem
    /// does not fail the creation: the buffer opens with the problem shown.
    pub fn create(&self) -> Arc<ChatSession> {
        let n = self.next_chat.fetch_add(1, Ordering::Relaxed);
        let name = format!("*llm:{n}*");
        let key = SessionKey::new(SESSION_PROVIDER, n);
        let bus = self.bus.clone();
        let store = ConversationStore::new(Arc::new(move |ev| bus.publish_typed(ev)));
        let settings = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .choose()
            .map(Settings::new);
        store.mutate(&key, |c| {
            c.assistant_label = Some(
                settings
                    .as_ref()
                    .map(|s| s.model.clone())
                    .unwrap_or_else(|_| "model".to_string()),
            );
            if let Err(e) = &settings {
                c.error = Some(e.clone());
            }
        });
        let session = Arc::new(ChatSession {
            name: name.clone(),
            key,
            store,
            settings,
            turn: Mutex::new(TurnState::default()),
            closed: AtomicBool::new(false),
        });
        self.reg().by_name.insert(name, session.clone());
        session
    }

    /// Attach the chat named `name` to `buffer`. Returns the session and the
    /// binding generation [`release`](Self::release) must present.
    pub fn bind(&self, name: &str, buffer: BufferId) -> Option<(Arc<ChatSession>, u64)> {
        let generation = self.next_bind.fetch_add(1, Ordering::Relaxed);
        let mut reg = self.reg();
        let session = reg.by_name.get(name)?.clone();
        reg.by_buffer.insert(buffer, (generation, session.clone()));
        Some((session, generation))
    }

    fn buffer_of(&self, session: &Arc<ChatSession>) -> Option<BufferId> {
        self.reg()
            .by_buffer
            .iter()
            .find(|(_, (_, s))| Arc::ptr_eq(s, session))
            .map(|(b, _)| *b)
    }

    pub fn for_buffer(&self, buffer: BufferId) -> Option<Arc<ChatSession>> {
        self.reg().by_buffer.get(&buffer).map(|(_, s)| s.clone())
    }

    /// The buffer closed: forget the chat and cancel what it has in flight. A
    /// stale generation (the buffer was re-bound since) does nothing.
    pub fn release(&self, buffer: BufferId, generation: u64) {
        let session = {
            let mut reg = self.reg();
            match reg.by_buffer.get(&buffer) {
                Some((g, _)) if *g == generation => {}
                _ => return,
            }
            let Some((_, session)) = reg.by_buffer.remove(&buffer) else {
                return;
            };
            reg.by_name.remove(&session.name);
            session
        };
        session.closed.store(true, Ordering::Release);
        self.interrupt(&session);
    }

    /// Submit a prompt: send it now, or queue it behind the running turn.
    pub fn submit(self: &Arc<Self>, session: &Arc<ChatSession>, text: String) {
        let send_now = {
            let mut turn = session.turn();
            if turn.busy {
                false
            } else {
                turn.busy = true;
                turn.interrupted = false;
                true
            }
        };
        if !send_now {
            session.store.mutate(&session.key, |c| c.queued.push(text));
            return;
        }
        session.store.mutate(&session.key, |c| {
            c.error = None;
            c.push_user_text(&text);
            c.status = SessionStatus::Thinking;
        });
        self.start_request(session);
    }

    /// Stop the running turn. A queued prompt, if any, is sent next.
    pub fn interrupt(&self, session: &Arc<ChatSession>) {
        let request = {
            let mut turn = session.turn();
            turn.interrupted = true;
            turn.request
        };
        if let Some(id) = request {
            self.requests.cancel(id);
        }
    }

    /// Start the request for the conversation as it now stands. The caller has
    /// already folded the user turn in and marked the session busy.
    fn start_request(self: &Arc<Self>, session: &Arc<ChatSession>) {
        let settings = match &session.settings {
            Ok(s) => s.clone(),
            Err(e) => {
                self.end_turn(session, Some(e.clone()));
                return;
            }
        };
        let history = history::messages(&session.store.snapshot());
        let chats = self.clone();
        let session = session.clone();
        // Key resolution may run a command, so it happens on the runtime, never
        // here on the editor thread.
        self.runtime.spawn(async move {
            let key = match resolve_key(&settings.provider_config).await {
                Ok(k) => k,
                Err(e) => {
                    chats.end_turn(&session, Some(e));
                    return;
                }
            };
            if session.turn().interrupted || session.closed.load(Ordering::Acquire) {
                chats.end_turn(&session, None);
                return;
            }
            let req = LlmRequest {
                model: settings.model.clone(),
                system: None,
                messages: history,
                tools: Vec::new(),
                params: Params::default(),
            };
            let sink = ChatSink::new(
                session.store.clone(),
                session.key.clone(),
                &settings.provider_config,
            );
            let target = RequestTarget {
                buffer: chats.buffer_of(&session).unwrap_or(BufferId(0)),
                label: session.name.clone(),
            };
            let id = chats.requests.start_with_key(
                target,
                req,
                settings.provider.clone(),
                key,
                Box::new(sink),
            );
            let interrupted = {
                let mut turn = session.turn();
                turn.request = Some(id);
                turn.interrupted
            };
            // An interrupt that landed between the check above and the id being
            // recorded found nothing to cancel; honour it now.
            if interrupted {
                chats.requests.cancel(id);
            }
            chats.register_request(id, &session);
        });
    }

    fn register_request(self: &Arc<Self>, id: RequestId, session: &Arc<ChatSession>) {
        let already_ended = {
            let mut reg = self.reg();
            match reg.ended_early.remove(&id) {
                Some(state) => Some(state),
                None => {
                    reg.by_request.insert(id, session.clone());
                    None
                }
            }
        };
        if let Some(state) = already_ended {
            self.request_ended(session, state);
        }
    }

    /// The bus listener's entry: a request changed state.
    pub fn on_request_state(self: &Arc<Self>, id: RequestId, state: RequestState) {
        if !state.is_terminal() {
            return;
        }
        let session = {
            let mut reg = self.reg();
            match reg.by_request.remove(&id) {
                Some(s) => Some(s),
                None => {
                    if reg.ended_early.len() >= ENDED_EARLY_CAP {
                        reg.ended_early.clear();
                    }
                    reg.ended_early.insert(id, state.clone());
                    None
                }
            }
        };
        if let Some(session) = session {
            self.request_ended(&session, state);
        }
    }

    fn request_ended(self: &Arc<Self>, session: &Arc<ChatSession>, state: RequestState) {
        let error = match state {
            RequestState::Failed(msg) => Some(msg),
            _ => None,
        };
        self.end_turn(session, error);
    }

    /// The turn is over: show the failure, if any, and send the next queued
    /// prompt. Dequeuing and folding the prompt in as a user turn is ONE
    /// mutation, so the drain never sees the submission count dip and recover
    /// (which it would read as a fresh submission and clear the prompt).
    fn end_turn(self: &Arc<Self>, session: &Arc<ChatSession>, error: Option<String>) {
        let closed = session.closed.load(Ordering::Acquire);
        let mut started_next = false;
        session.store.mutate(&session.key, |c| {
            c.status = SessionStatus::Idle;
            if error.is_some() {
                c.error = error;
            }
            if !closed && !c.queued.is_empty() {
                let next = c.queued.remove(0);
                c.error = None;
                c.push_user_text(&next);
                c.status = SessionStatus::Thinking;
                started_next = true;
            }
        });
        {
            let mut turn = session.turn();
            turn.request = None;
            turn.interrupted = false;
            turn.busy = started_next;
        }
        if started_next {
            self.start_request(session);
        }
    }
}

/// Folds a request's stream events into a chat's conversation model.
struct ChatSink {
    store: ConversationStore,
    key: SessionKey,
    /// USD per million tokens, both or the cost is not shown.
    pricing: Option<(f64, f64)>,
}

impl ChatSink {
    fn new(store: ConversationStore, key: SessionKey, provider: &ProviderConfig) -> Self {
        let pricing = provider
            .input_cost_per_mtok
            .zip(provider.output_cost_per_mtok);
        Self {
            store,
            key,
            pricing,
        }
    }
}

impl Sink for ChatSink {
    fn event(&mut self, ev: StreamEvent) -> SinkFlow {
        let pricing = self.pricing;
        self.store.mutate(&self.key, |c| match ev {
            StreamEvent::Text(t) => c.extend_text(Role::Assistant, &t),
            StreamEvent::Reasoning(t) => c.extend_reasoning(&t),
            StreamEvent::ReasoningData(d) => c.attach_reasoning_data(OpaqueReasoning {
                provider: d.provider,
                data: d.data,
            }),
            StreamEvent::Usage(u) => {
                let used =
                    u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens;
                let cost = pricing.map(|(input, output)| Cost {
                    amount: (u.input_tokens as f64 * input + u.output_tokens as f64 * output)
                        / 1_000_000.0,
                    currency: "USD".to_string(),
                });
                c.usage = Some(UsageSnapshot {
                    used,
                    size: 0,
                    cost,
                });
            }
            StreamEvent::Done(StopReason::MaxTokens) => {
                c.error = Some("the reply was cut off at the output token limit".to_string());
            }
            StreamEvent::Done(StopReason::Other(reason)) => {
                c.error = Some(format!("the model stopped: {reason}"));
            }
            StreamEvent::Done(_) => {}
            StreamEvent::Error(e) => c.error = Some(e.to_string()),
            // Tools arrive with the tool loop (LL.16).
            StreamEvent::ToolCallStart { .. }
            | StreamEvent::ToolCallInput { .. }
            | StreamEvent::ToolCallEnd { .. } => {}
        });
        SinkFlow::Continue
    }
}

/// Subscribe `chats` to request state changes. The listener runs on the
/// runtime, off the editor thread; what it does is cheap model folds and a
/// spawn, and the drain's wake (`ConversationProjected`) carries the result to
/// the screen.
pub fn spawn_turn_listener(chats: LlmChatsHandle, bus: &Arc<lattice_runtime::EventBus>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<super::RequestStateChanged>();
    bus.subscribe_typed::<super::RequestStateChanged>(tx);
    let runtime = chats.runtime.clone();
    runtime.spawn(async move {
        while let Some(ev) = rx.recv().await {
            chats.on_request_state(ev.id, ev.state);
        }
    });
}
