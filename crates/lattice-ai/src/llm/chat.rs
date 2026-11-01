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
//!
//! A turn that ends by interrupt hands over to the next queued prompt. A turn
//! that ends in failure does not: the error is shown, the queued prompts stay
//! visible, and the next explicit send resumes them in order. The failed user
//! turn is left out of the provider history (see `history::messages`), so two
//! user turns are never sent back to back.
//!
//! The queue and `busy` are decided together under the session's turn lock, in
//! `submit` and in `end_turn`, so a send landing between a turn ending and the
//! next being chosen cannot strand a prompt.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use lattice_agent::SessionKey;
use lattice_core::BufferId;
use tokio::runtime::Handle;

use super::ApiKey;
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
    /// The resolved key, fetched once per chat so a password manager prompts
    /// once, not once per turn.
    api_key: Mutex<Option<ApiKey>>,
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
    /// Chats no buffer is bound to: freshly created ones awaiting their buffer's
    /// activation, and ones whose buffer's guard dropped, awaiting a
    /// reactivation. Oldest first, bounded by `UNBOUND_CAP`.
    unbound: Vec<Arc<ChatSession>>,
    by_buffer: HashMap<BufferId, (u64, Arc<ChatSession>)>,
    by_request: HashMap<RequestId, Arc<ChatSession>>,
    /// Terminal states announced before their request was registered here.
    ended_early: HashMap<RequestId, RequestState>,
    /// Insertion order of `ended_early`, for evicting the oldest.
    ended_order: VecDeque<RequestId>,
}

/// Bound on `ended_early`: requests this registry never owns would otherwise
/// accumulate there.
const ENDED_EARLY_CAP: usize = 256;

/// Bound on `Registry::unbound`: a buffer that never activates would otherwise
/// leave its chat there for good.
const UNBOUND_CAP: usize = 32;

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

    /// How chat transcripts are projected right now. Read per projection, so a
    /// changed fold setting applies to the next one.
    pub fn projection_options(&self) -> crate::conversation::ProjectionOptions {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .projection_options()
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
            name,
            key,
            store,
            settings,
            turn: Mutex::new(TurnState::default()),
            api_key: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        {
            let mut reg = self.reg();
            reg.unbound.push(session.clone());
            if reg.unbound.len() > UNBOUND_CAP {
                reg.unbound.remove(0);
            }
        }
        session
    }

    /// Attach the chat named `name` to `buffer`. Returns the session and the
    /// binding generation [`release`](Self::release) must present. A chat whose
    /// earlier guard was released (the mode was reactivated) is reopened, with
    /// its history intact.
    pub fn bind(&self, name: &str, buffer: BufferId) -> Option<(Arc<ChatSession>, u64)> {
        let generation = self.next_bind.fetch_add(1, Ordering::Relaxed);
        let mut reg = self.reg();
        let session = match reg.unbound.iter().position(|s| s.name == name) {
            Some(i) => reg.unbound.remove(i),
            None => reg
                .by_buffer
                .values()
                .find(|(_, s)| s.name == name)
                .map(|(_, s)| s.clone())?,
        };
        session.closed.store(false, Ordering::Release);
        reg.by_buffer.insert(buffer, (generation, session.clone()));
        Some((session, generation))
    }

    /// How many chats no buffer is bound to.
    #[doc(hidden)]
    pub fn unbound_count(&self) -> usize {
        self.reg().unbound.len()
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

    /// The buffer's guard dropped: unbind the chat and cancel what it has in flight. A
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
            reg.unbound.push(session.clone());
            if reg.unbound.len() > UNBOUND_CAP {
                reg.unbound.remove(0);
            }
            session
        };
        session.closed.store(true, Ordering::Release);
        self.interrupt(&session);
    }

    /// Submit a prompt: send it now, or queue it behind the running turn.
    ///
    /// Idle with prompts still queued (a failure held them) the new prompt joins
    /// the back of the queue and the front is sent, so the queue resumes in order.
    pub fn submit(self: &Arc<Self>, session: &Arc<ChatSession>, text: String) {
        let send_now = {
            let mut turn = session.turn();
            let busy = turn.busy;
            let mut started = false;
            session.store.mutate(&session.key, |c| {
                c.queued.push(text);
                if !busy {
                    let next = c.queued.remove(0);
                    c.error = None;
                    c.push_user_text(&next);
                    c.status = SessionStatus::Thinking;
                    started = true;
                }
            });
            if started {
                turn.busy = true;
                turn.interrupted = false;
            }
            started
        };
        if send_now {
            self.start_request(session);
        }
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
            let cached = session
                .api_key
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let key = match cached {
                Some(k) => k,
                None => match resolve_key(&settings.provider_config).await {
                    Ok(k) => {
                        *session.api_key.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(k.clone());
                        k
                    }
                    Err(e) => {
                        chats.end_turn(&session, Some(e));
                        return;
                    }
                },
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
                Some(state) => {
                    reg.ended_order.retain(|o| *o != id);
                    Some(state)
                }
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
                    // Evict the oldest only: clearing would drop a state whose
                    // request is about to register, leaving its turn busy forever.
                    while reg.ended_early.len() >= ENDED_EARLY_CAP {
                        let Some(old) = reg.ended_order.pop_front() else {
                            break;
                        };
                        reg.ended_early.remove(&old);
                    }
                    reg.ended_early.insert(id, state.clone());
                    reg.ended_order.push_back(id);
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
    /// prompt unless the turn failed. Dequeuing and folding the prompt in as a
    /// user turn is ONE mutation, so the drain never sees the submission count
    /// dip and recover (which it would read as a fresh submission and clear the
    /// prompt). The queue decision and `busy` are one critical section under
    /// the turn lock, shared with `submit`.
    fn end_turn(self: &Arc<Self>, session: &Arc<ChatSession>, error: Option<String>) {
        let closed = session.closed.load(Ordering::Acquire);
        let failed = error.is_some();
        let started_next = {
            let mut turn = session.turn();
            let mut started = false;
            session.store.mutate(&session.key, |c| {
                c.status = SessionStatus::Idle;
                if failed {
                    c.error = error;
                }
                if !failed && !closed && !c.queued.is_empty() {
                    let next = c.queued.remove(0);
                    c.error = None;
                    c.push_user_text(&next);
                    c.status = SessionStatus::Thinking;
                    started = true;
                }
            });
            turn.request = None;
            turn.interrupted = false;
            turn.busy = started;
            started
        };
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::llm::RequestRegistry;

    #[tokio::test]
    async fn the_early_ended_cache_evicts_the_oldest_and_keeps_the_rest() {
        let handle = Handle::current();
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
            handle.clone(),
            Some(bus.clone()),
            None,
        ));
        let chats = Arc::new(LlmChats::new(handle, requests, bus, LlmConfig::default()));
        for i in 0..=ENDED_EARLY_CAP as u64 {
            chats.on_request_state(RequestId(i), RequestState::Done);
        }
        let reg = chats.reg();
        assert_eq!(reg.ended_early.len(), ENDED_EARLY_CAP);
        assert!(!reg.ended_early.contains_key(&RequestId(0)), "oldest goes");
        assert!(reg.ended_early.contains_key(&RequestId(1)));
        assert!(
            reg.ended_early
                .contains_key(&RequestId(ENDED_EARLY_CAP as u64))
        );
    }
}
