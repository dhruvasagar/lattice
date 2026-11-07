//! The request lifecycle: ids, states, the live snapshot and cancellation.
//!
//! [`RequestRegistry`] owns every in-flight request. Each runs as a task spawned
//! on the runtime [`Handle`] given at construction (the host's
//! `boot.runtime_handle()`), never a bare `tokio::spawn` from the caller, so a
//! caller on the editor actor's `current_thread` runtime still gets its I/O,
//! decoding and sink off that thread.
//!
//! Readers (`:llm-requests`, the modeline) take [`RequestRegistry::snapshot`], a
//! lock-free `ArcSwap` load. State changes also publish a typed
//! [`RequestStateChanged`] event on the bus when one was supplied, and call the
//! optional notify callback.
//!
//! # Retention
//!
//! A request in a terminal state (`Done`, `Failed`, `Cancelled`) leaves the live
//! set but stays in the snapshot as history: the [`RETAINED_TERMINAL`] most
//! recently finished ones are kept, oldest evicted first. Live requests are
//! never evicted. A terminal state is final: later transitions are ignored.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use lattice_core::BufferId;
use lattice_runtime::EventBus;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use super::provider::Provider;
use super::tools::BoxFuture;
use super::transport::{self, Outcome, SinkFlow, scrub};
use super::{ApiKey, LlmRequest, StopReason, StreamEvent, Usage};

/// How many finished requests the snapshot keeps for display.
pub const RETAINED_TERMINAL: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestState {
    Pending,
    Streaming,
    AwaitingTool,
    AwaitingReview,
    Done,
    Failed(String),
    Cancelled,
}

impl RequestState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_) | Self::Cancelled)
    }
}

/// Where a request's output is going. Minimal for now: the buffer and a short
/// label for listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTarget {
    pub buffer: BufferId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestInfo {
    pub id: RequestId,
    pub target: RequestTarget,
    pub model: String,
    pub state: RequestState,
    /// Latest cumulative usage the provider reported, if any.
    pub usage: Option<Usage>,
}

/// Published on the event bus on every state change.
#[derive(Debug, Clone)]
pub struct RequestStateChanged {
    pub id: RequestId,
    pub state: RequestState,
}

lattice_protocol::register_event!(
    RequestStateChanged,
    "llm.request-state-changed",
    "Fired when an LLM request changes state (pending, streaming, done, failed, cancelled).",
    "lattice-ai",
);

/// Receives a request's events on the request task. Must not block.
pub trait Sink: Send {
    fn event(&mut self, ev: StreamEvent) -> SinkFlow;
}

/// Shared registry handle, the `ServiceRegistry` registration/lookup type.
pub type RequestRegistryHandle = Arc<RequestRegistry>;

type Notify = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    entries: Vec<RequestInfo>,
    tokens: std::collections::HashMap<RequestId, CancellationToken>,
}

struct Shared {
    handle: Handle,
    client: Result<reqwest::Client, String>,
    snapshot: ArcSwap<Vec<RequestInfo>>,
    inner: std::sync::Mutex<Inner>,
    next_id: AtomicU64,
    bus: Option<Arc<EventBus>>,
    notify: Option<Notify>,
    idle: Duration,
}

pub struct RequestRegistry {
    shared: Arc<Shared>,
}

impl RequestRegistry {
    /// `handle` is the runtime requests run on (`boot.runtime_handle()`); `bus`
    /// receives [`RequestStateChanged`]; `notify` is an optional extra hook.
    pub fn new(handle: Handle, bus: Option<Arc<EventBus>>, notify: Option<Notify>) -> Self {
        Self::with_idle_timeout(handle, bus, notify, transport::IDLE_TIMEOUT)
    }

    pub fn with_idle_timeout(
        handle: Handle,
        bus: Option<Arc<EventBus>>,
        notify: Option<Notify>,
        idle: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                handle,
                client: transport::build_client(),
                snapshot: ArcSwap::from_pointee(Vec::new()),
                inner: std::sync::Mutex::new(Inner {
                    entries: Vec::new(),
                    tokens: Default::default(),
                }),
                next_id: AtomicU64::new(1),
                bus,
                notify,
                idle,
            }),
        }
    }

    /// Start a request against a server that needs no key (a local Ollama).
    pub fn start(
        &self,
        target: RequestTarget,
        req: LlmRequest,
        p: Arc<dyn Provider>,
        sink: Box<dyn Sink>,
    ) -> RequestId {
        self.start_with_key(target, req, p, ApiKey::new(""), sink)
    }

    /// Start a request. Returns at once; the work runs on the registry's runtime.
    pub fn start_with_key(
        &self,
        target: RequestTarget,
        req: LlmRequest,
        p: Arc<dyn Provider>,
        key: ApiKey,
        sink: Box<dyn Sink>,
    ) -> RequestId {
        self.start_inner(target, req, p, key, sink, None)
    }

    /// LL.16: start a request that may use tools. When an exchange ends asking
    /// for tools, the request moves to `AwaitingTool` and `rounds` runs them
    /// and supplies the next exchange; the request stays one id, one state and
    /// one cancellation token throughout, so `:llm-stop` stops the whole loop
    /// wherever it is.
    pub fn start_loop(
        &self,
        target: RequestTarget,
        req: LlmRequest,
        p: Arc<dyn Provider>,
        key: ApiKey,
        sink: Box<dyn Sink>,
        rounds: Box<dyn Rounds>,
    ) -> RequestId {
        self.start_inner(target, req, p, key, sink, Some(rounds))
    }

    fn start_inner(
        &self,
        target: RequestTarget,
        req: LlmRequest,
        p: Arc<dyn Provider>,
        key: ApiKey,
        sink: Box<dyn Sink>,
        rounds: Option<Box<dyn Rounds>>,
    ) -> RequestId {
        let shared = self.shared.clone();
        let id = RequestId(shared.next_id.fetch_add(1, Ordering::Relaxed));
        let token = CancellationToken::new();
        shared.insert(
            RequestInfo {
                id,
                target,
                model: req.model.clone(),
                state: RequestState::Pending,
                usage: None,
            },
            token.clone(),
        );
        let work = shared.clone();
        // The inner task is joined so a panicking sink still ends in a terminal
        // state rather than a request stuck `Streaming` forever.
        shared.handle.spawn(async move {
            let inner =
                work.handle
                    .spawn(drive(work.clone(), id, req, p, key, sink, token, rounds));
            if inner.await.is_err() {
                work.finish(id, RequestState::Failed("the request task panicked".into()));
            }
        });
        id
    }

    /// Cancel a request. Immediate even on a stalled stream; a no-op once the
    /// request is terminal.
    pub fn cancel(&self, id: RequestId) {
        let token = self.shared.lock().tokens.get(&id).cloned();
        if let Some(t) = token {
            t.cancel();
        }
    }

    /// Live requests and recently finished ones, in start order.
    pub fn snapshot(&self) -> Arc<Vec<RequestInfo>> {
        self.shared.snapshot.load_full()
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock only means a sink panicked elsewhere; the data is intact.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn insert(&self, info: RequestInfo, token: CancellationToken) {
        let state = info.state.clone();
        let id = info.id;
        {
            let mut g = self.lock();
            g.tokens.insert(id, token);
            g.entries.push(info);
            self.snapshot.store(Arc::new(g.entries.clone()));
        }
        self.announce(id, state);
    }

    /// Move `id` to `state`. A terminal state is final.
    fn finish(&self, id: RequestId, state: RequestState) {
        self.transition(id, state);
    }

    fn transition(&self, id: RequestId, state: RequestState) {
        {
            let mut g = self.lock();
            let Some(entry) = g.entries.iter_mut().find(|e| e.id == id) else {
                return;
            };
            if entry.state.is_terminal() || entry.state == state {
                return;
            }
            entry.state = state.clone();
            if state.is_terminal() {
                g.tokens.remove(&id);
                evict_old_terminal(&mut g.entries);
            }
            self.snapshot.store(Arc::new(g.entries.clone()));
        }
        self.announce(id, state);
    }

    fn set_usage(&self, id: RequestId, usage: Usage) {
        let mut g = self.lock();
        if let Some(e) = g.entries.iter_mut().find(|e| e.id == id) {
            e.usage = Some(usage);
            self.snapshot.store(Arc::new(g.entries.clone()));
        }
    }

    fn announce(&self, id: RequestId, state: RequestState) {
        if let Some(bus) = &self.bus {
            bus.publish_typed(RequestStateChanged { id, state });
        }
        if let Some(n) = &self.notify {
            n();
        }
    }
}

fn evict_old_terminal(entries: &mut Vec<RequestInfo>) {
    let terminal = entries.iter().filter(|e| e.state.is_terminal()).count();
    let mut excess = terminal.saturating_sub(RETAINED_TERMINAL);
    entries.retain(|e| {
        if excess > 0 && e.state.is_terminal() {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    shared: Arc<Shared>,
    id: RequestId,
    mut req: LlmRequest,
    provider: Arc<dyn Provider>,
    key: ApiKey,
    mut sink: Box<dyn Sink>,
    cancel: CancellationToken,
    mut rounds: Option<Box<dyn Rounds>>,
) {
    let state = loop {
        let mut watched = Watched {
            inner: sink.as_mut(),
            stop: None,
        };
        let outcome = run_request(
            &shared,
            id,
            &req,
            provider.as_ref(),
            &key,
            &mut watched,
            &cancel,
        )
        .await;
        let stop = watched.stop.take();
        match outcome {
            Outcome::Done => {}
            Outcome::Stopped | Outcome::Cancelled => break RequestState::Cancelled,
            Outcome::Failed(msg) => break RequestState::Failed(msg),
        }
        let Some(rounds) = rounds
            .as_mut()
            .filter(|_| stop == Some(StopReason::ToolUse))
        else {
            break RequestState::Done;
        };
        shared.transition(id, RequestState::AwaitingTool);
        let round = tokio::select! {
            biased;
            _ = cancel.cancelled() => break RequestState::Cancelled,
            round = rounds.after_tools(&cancel) => round,
        };
        match round {
            Round::Next(next) => req = next,
            Round::Finish => break RequestState::Done,
            Round::Fail(msg) => break RequestState::Failed(msg),
        }
        if cancel.is_cancelled() {
            break RequestState::Cancelled;
        }
    };
    shared.finish(id, state);
}

/// What runs between the exchanges of a tool loop (LL.16).
pub trait Rounds: Send {
    /// The last exchange ended asking for tools: run them, and say what
    /// happens next. A cancel drops the future, so a round must leave its
    /// record valid at any await point.
    fn after_tools<'a>(&'a mut self, cancel: &'a CancellationToken) -> BoxFuture<'a, Round>;
}

/// The outcome of a round.
#[derive(Debug)]
pub enum Round {
    /// Send this request next: the conversation so far, with the results.
    Next(LlmRequest),
    /// Nothing more to send.
    Finish,
    /// Stop the loop, saying why.
    Fail(String),
}

/// A sink that remembers why the exchange ended.
struct Watched<'a> {
    inner: &'a mut dyn Sink,
    stop: Option<StopReason>,
}

impl Sink for Watched<'_> {
    fn event(&mut self, ev: StreamEvent) -> SinkFlow {
        if let StreamEvent::Done(reason) = &ev {
            self.stop = Some(reason.clone());
        }
        self.inner.event(ev)
    }
}

async fn run_request(
    shared: &Arc<Shared>,
    id: RequestId,
    req: &LlmRequest,
    provider: &dyn Provider,
    key: &ApiKey,
    sink: &mut dyn Sink,
    cancel: &CancellationToken,
) -> Outcome {
    let client = match &shared.client {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(scrub(e, key)),
    };
    let http = match provider.build(req, key) {
        Ok(h) => h,
        Err(e) => return Outcome::Failed(scrub(&e.to_string(), key)),
    };
    let decoder = provider.decoder();
    let ex = transport::Exchange {
        client,
        key,
        cancel,
        idle: shared.idle,
    };
    transport::run(
        ex,
        http,
        decoder,
        || shared.transition(id, RequestState::Streaming),
        |events| {
            let mut flow = SinkFlow::Continue;
            for ev in events {
                if let StreamEvent::Usage(u) = &ev {
                    shared.set_usage(id, u.clone());
                }
                // Keep delivering after Stop is not allowed: stop at the first.
                if sink.event(ev) == SinkFlow::Stop {
                    flow = SinkFlow::Stop;
                    break;
                }
            }
            flow
        },
    )
    .await
}
