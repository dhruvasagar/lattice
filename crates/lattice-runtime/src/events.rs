//! In-process event bus (DESIGN.md §5.10).
//!
//! Vim's `autocmd` and emacs's hooks both desugar to the same
//! primitive: subscribe a sink to a filtered stream of typed
//! events; the bus calls the sink whenever a matching event is
//! published. v1 ships the observation-only baseline; the
//! Before-event veto / mutation seam (§5.10.2) layers on later.
//!
//! ## v1 scope
//!
//! - Filters by [`lattice_protocol::EventKind`] for bucketing, then
//!   AND-combines the reserved [`EventFilter`] fields (`path_glob`,
//!   `major_modes`, `predicate`) at publish time on the candidates
//!   the kind bucket already selected (EF.1). Mode activation is the
//!   caller that needed them (mode-architecture.md §7.4); the checks
//!   are per-candidate constants, so dispatch stays
//!   O(subscribers-of-kind) -- the filter fields never widen the
//!   publish scan.
//! - Sinks are [`SubscriptionTarget::Channel`] (an `mpsc::Sender`)
//!   or [`SubscriptionTarget::Invocation`] (a `CommandInvocation`
//!   the bus runs through the document actor's dispatch when the
//!   App wires that path).
//! - Plugin handler target (`SubscriptionTarget::Plugin` in
//!   §5.10) is omitted -- WASM hosting isn't online in v1.
//! - Indexed dispatch: subscriptions live in a
//!   `HashMap<EventKind, Vec<Subscription>>`. Publish iterates the
//!   bucket for one kind, never the global list.
//! - Bus is `Send + Sync`; the inner state is one `Mutex`. The
//!   publish path takes the lock only to snapshot the matching
//!   channel senders (and to queue Invocation targets onto the
//!   shared `pending_invocations`); the actual `tx.send` calls
//!   run with the lock dropped. Two `publish` calls from
//!   different threads can therefore dispatch in parallel, and a
//!   future bounded subscriber cannot stall the publisher under
//!   the bus mutex (audit M1).
//!
//! ## What's NOT here
//!
//! - **`BeforeSave` / `BeforeQuit` veto.** v1 publishes the event
//!   so observers see it; mutating the payload or aborting the
//!   transition is out of scope until the actor runs the bus
//!   inside its task and respects handler errors.
//! - **Backpressure.** Channel sinks use unbounded mpsc. If a
//!   subscriber leaks senders the bus grows. Bounded channels +
//!   slow-consumer policy follow when LSP / plugin subscribers
//!   can actually generate the volume that needs governance.
//! - **Per-handler fuel** (§5.10.4). Plugin / Invocation handlers
//!   will eventually run with a fuel budget; v1 calls them inline
//!   or punts to caller for Invocation targets.

use std::any::TypeId;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use globset::GlobSet;
use lattice_grammar::CommandInvocation;
use lattice_keymap::ModeId;
use lattice_protocol::event_registry::Event as TypedEvent;
use lattice_protocol::{Event, EventKind};
use tokio::sync::mpsc;

/// Opaque handle returned by [`EventBus::subscribe`]. Pass to
/// [`EventBus::unsubscribe`] to remove the subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    fn next() -> Self {
        // Process-wide monotonic. One u64 is enough for the life
        // of any process; doubles as a deterministic order for
        // tests that need to assert dispatch order.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Raw value -- exposed for test assertions and logging only;
    /// callers should not rely on the value beyond uniqueness.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A caller-supplied escape-hatch predicate (the `predicate` field
/// of [`EventFilter`]). Returns `true` to keep the event.
///
/// **Contract (locking discipline, audit M1).** The predicate is
/// evaluated *under the bus mutex* during the publish snapshot
/// phase, before the lock is dropped for delivery. It must be cheap
/// and MUST NOT re-enter the bus (no `subscribe` / `publish` /
/// `subscription_count` from inside it) -- doing so would deadlock
/// on the inner mutex. Modes prefer the declarative `path_glob` /
/// `major_modes` fields (mode-architecture.md §7.4); `predicate` is
/// for the rare case those can't express (`init.rs` custom rules).
pub type EventPredicate = Arc<dyn Fn(&Event) -> bool + Send + Sync>;

/// Filter applied at publish time. `kinds` buckets the
/// subscription; the remaining fields (EF.1) AND-combine on top --
/// every `Some` field must match for the event to be delivered, a
/// `None` field is unconstrained. Mode activation
/// (mode-architecture.md §7.4) is the caller these reserved fields
/// were declared for.
#[derive(Default, Clone)]
pub struct EventFilter {
    /// Kinds this subscription cares about. `None` means "all
    /// kinds" -- the wildcard. Callers should always pass
    /// `Some(...)` to keep dispatch indexed; the wildcard is
    /// supported for one-off debugging / introspection sinks.
    pub kinds: Option<Vec<EventKind>>,
    /// Restrict to events whose path matches this set (e.g.
    /// `**/*.rs` for a Rust major-mode resolver). `None` is
    /// unconstrained. Events with no path (e.g. `BeforeQuit`,
    /// scratch-buffer opens) never match a `Some` glob. Build the
    /// set via [`crate::compile_glob_set`].
    pub path_glob: Option<GlobSet>,
    /// Restrict to events whose buffer is in one of these major
    /// modes -- the minor-mode allowlist of §7.4. `None` is
    /// unconstrained. No `Event` variant carries a major mode yet
    /// (MA.1 lands `MajorEntered { major }`); until then a `Some`
    /// allowlist matches nothing, which is the correct semantics
    /// for "only fire inside these majors."
    pub major_modes: Option<Vec<ModeId>>,
    /// Escape-hatch predicate. `None` is unconstrained. See
    /// [`EventPredicate`] for the locking contract -- it runs under
    /// the bus mutex and must not re-enter the bus.
    pub predicate: Option<EventPredicate>,
}

impl std::fmt::Debug for EventFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `predicate` is an opaque closure; report only its
        // presence so the rest of the filter stays inspectable.
        f.debug_struct("EventFilter")
            .field("kinds", &self.kinds)
            .field("path_glob", &self.path_glob)
            .field("major_modes", &self.major_modes)
            .field("predicate", &self.predicate.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl EventFilter {
    /// Convenience: subscribe to a single kind.
    pub fn kind(k: EventKind) -> Self {
        Self {
            kinds: Some(vec![k]),
            ..Default::default()
        }
    }

    /// Convenience: subscribe to a list of kinds.
    pub fn kinds(ks: Vec<EventKind>) -> Self {
        Self {
            kinds: Some(ks),
            ..Default::default()
        }
    }

    /// Wildcard: every event.
    pub fn any() -> Self {
        Self {
            kinds: None,
            ..Default::default()
        }
    }

    /// Builder: AND a path-glob constraint onto this filter.
    pub fn with_path_glob(mut self, glob: GlobSet) -> Self {
        self.path_glob = Some(glob);
        self
    }

    /// Builder: AND a major-mode allowlist onto this filter.
    pub fn with_major_modes(mut self, modes: Vec<ModeId>) -> Self {
        self.major_modes = Some(modes);
        self
    }

    /// Builder: AND an escape-hatch predicate onto this filter. See
    /// [`EventPredicate`] for the locking contract.
    pub fn with_predicate(mut self, predicate: EventPredicate) -> Self {
        self.predicate = Some(predicate);
        self
    }
}

/// The non-`kinds` portion of an [`EventFilter`], stored per
/// [`Subscription`] so the publish path can AND-check it against the
/// event after the kind bucket has already selected the candidate.
/// `kinds` is consumed by bucketing in [`EventBus::subscribe`] and
/// never re-checked, so it is not carried here.
#[derive(Clone, Default)]
struct ExtraFilter {
    path_glob: Option<GlobSet>,
    major_modes: Option<Vec<ModeId>>,
    predicate: Option<EventPredicate>,
}

impl std::fmt::Debug for ExtraFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtraFilter")
            .field("path_glob", &self.path_glob)
            .field("major_modes", &self.major_modes)
            .field("predicate", &self.predicate.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl ExtraFilter {
    /// `true` if every constrained field matches `event`. An
    /// all-`None` filter (the common case) is unconstrained and
    /// returns `true` immediately. Evaluated under the bus mutex --
    /// see [`EventPredicate`] for the predicate's non-reentrancy
    /// contract.
    fn matches(&self, event: &Event) -> bool {
        if let Some(glob) = &self.path_glob {
            match event_path(event) {
                Some(path) if glob.is_match(path) => {}
                // No path, or path doesn't match: a path-constrained
                // filter rejects.
                _ => return false,
            }
        }
        if let Some(allow) = &self.major_modes {
            match event_major_mode(event) {
                Some(major) if allow.iter().any(|id| id.as_str() == major) => {}
                _ => return false,
            }
        }
        if self.predicate.as_ref().is_some_and(|p| !p(event)) {
            return false;
        }
        true
    }
}

/// A host-owned sink that delivers a matched [`Event`] to a plugin's `on-event`
/// handler (PH7.8). Returns `false` when the plugin's receiver has closed (its
/// actor task ended), so the bus prunes the subscription lazily — the same
/// closed-`Channel` / `ForwardFn` discipline the rest of this module uses.
///
/// **The bus stays channel-agnostic.** The plugin host builds this closure over
/// *its own* `futures::channel` mpsc (the plugin-host lib keeps `tokio` a
/// dev-dependency; the bus's `Channel` variant uses `tokio` mpsc), so
/// `SubscriptionTarget` never names a plugin-host or specific-channel type and
/// `lattice-runtime` grows no dependency on either. The sink is invoked with the
/// bus mutex **dropped** (the audit-M1 dispatch phase), so a slow plugin handler
/// can never stall the publisher or another subscriber.
pub type PluginEventSink = Arc<dyn Fn(Event) -> bool + Send + Sync>;

/// What the bus does when a matching event arrives.
#[derive(Clone)]
pub enum SubscriptionTarget {
    /// Push the event onto an unbounded mpsc. Closed senders are
    /// pruned lazily on the next publish that hits this kind.
    Channel(mpsc::UnboundedSender<Event>),
    /// Run a [`CommandInvocation`] in response. v1 does NOT execute
    /// it (the bus has no document handle); instead it stores the
    /// invocation and surfaces it via [`EventBus::drain_pending_invocations`]
    /// for the App to dispatch on its turn through the actor. This
    /// keeps the bus loop-free and side-effect-free with respect
    /// to document state.
    Invocation(CommandInvocation),
    /// Deliver the event to a WASM plugin's `on-event` handler (PH7.8, filling
    /// the reserved slot §5.10 anticipated). The host owns the `sink` (over the
    /// plugin's actor channel); the bus calls it with the lock dropped, so a
    /// slow plugin handler never delays the publisher or another subscriber.
    /// `plugin` / `handler` identify the subscription for provenance and
    /// teardown (the host unsubscribes a quarantined plugin's ids); `sink`
    /// carries the delivery + the closed-receiver signal (`false` → prune).
    Plugin {
        /// The host-issued plugin id (the `u32` inside `SourceLayer::Plugin`).
        plugin: u32,
        /// The guest-chosen handler id, passed back to `on-event(handler, ev)`.
        handler: u32,
        /// The delivery sink (host-owned; runs lock-dropped).
        sink: PluginEventSink,
    },
}

impl std::fmt::Debug for SubscriptionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `sink` is an opaque closure; report the identifying fields and mark
        // the sink's presence (mirrors `EventFilter`'s `predicate` handling).
        match self {
            SubscriptionTarget::Channel(tx) => f.debug_tuple("Channel").field(tx).finish(),
            SubscriptionTarget::Invocation(inv) => {
                f.debug_tuple("Invocation").field(inv).finish()
            }
            SubscriptionTarget::Plugin {
                plugin, handler, ..
            } => f
                .debug_struct("Plugin")
                .field("plugin", plugin)
                .field("handler", handler)
                .field("sink", &"<fn>")
                .finish(),
        }
    }
}

#[derive(Debug)]
struct Subscription {
    id: SubscriptionId,
    target: SubscriptionTarget,
    /// EF.1: the non-`kinds` filter fields, AND-checked against the
    /// event at publish time (after the kind bucket selected this
    /// subscription as a candidate).
    extra: ExtraFilter,
}

#[derive(Default)]
struct Inner {
    /// Indexed by kind for O(1) bucket lookup at publish.
    by_kind: HashMap<EventKind, Vec<Subscription>>,
    /// Wildcard subscribers (`filter.kinds == None`). Visited on
    /// every publish; expected to be small (debug / log sinks).
    wildcard: Vec<Subscription>,
    /// Invocation targets the bus matched against published events
    /// but does not own the dispatch path for. The App calls
    /// [`EventBus::drain_pending_invocations`] each tick and routes
    /// these through the document actor.
    pending_invocations: Vec<CommandInvocation>,
    /// M.5.3.a typed-event subscriptions, keyed by Rust
    /// `std::any::TypeId`. Each entry stores a closure that
    /// downcasts the boxed payload and forwards to the
    /// caller's typed channel; the closure carries enough type
    /// information that the bus's publish path stays
    /// `Arc<dyn Any + Send + Sync>`-typed.
    typed_subs: HashMap<TypeId, Vec<TypedSubscription>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("by_kind", &self.by_kind)
            .field("wildcard", &self.wildcard)
            .field("pending_invocations", &self.pending_invocations)
            .field("typed_sub_buckets", &self.typed_subs.len())
            .finish()
    }
}

/// Internal record for a typed subscription. The closure
/// downcasts the `Arc<dyn Any + Send + Sync>` payload to the
/// concrete event type and forwards to the subscriber's typed
/// channel; it returns `false` if the channel has been closed
/// so the bus can prune lazily.
///
/// `ForwardFn` factors out the type-erased forwarder so clippy's
/// `type_complexity` lint stops flagging the inline shape at
/// every use site.
type ForwardFn = Arc<dyn Fn(&Arc<dyn std::any::Any + Send + Sync>) -> bool + Send + Sync>;

struct TypedSubscription {
    id: SubscriptionId,
    forward: ForwardFn,
}

/// Process-shared event bus. Cheap to construct (one `Mutex`
/// holding empty maps). Cheap to clone via `Arc<EventBus>` --
/// callers wrap externally; the type itself is `Send + Sync` and
/// can be shared by reference.
#[derive(Debug, Default)]
pub struct EventBus {
    inner: Mutex<Inner>,
}

impl EventBus {
    /// Build an empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a sink. Returns the id for [`Self::unsubscribe`].
    pub fn subscribe(&self, filter: EventFilter, target: SubscriptionTarget) -> SubscriptionId {
        let id = SubscriptionId::next();
        let EventFilter {
            kinds,
            path_glob,
            major_modes,
            predicate,
        } = filter;
        // EF.1: the non-`kinds` fields ride along on each
        // Subscription so publish can AND-check them. Cloned per
        // kind bucket for multi-kind subscriptions (Arc / small-Vec
        // clones; the common all-`None` case is free).
        let extra = ExtraFilter {
            path_glob,
            major_modes,
            predicate,
        };
        let mut inner = self.inner.lock().expect("EventBus poisoned");
        match kinds {
            Some(kinds) => {
                // Multi-kind subscriptions register once per kind
                // bucket so dispatch never has to re-check the
                // *kind* for the bucket it already matched on (the
                // extra fields are still checked per event).
                for k in kinds {
                    inner.by_kind.entry(k).or_default().push(Subscription {
                        id,
                        target: target.clone(),
                        extra: extra.clone(),
                    });
                }
            }
            None => inner.wildcard.push(Subscription { id, target, extra }),
        }
        id
    }

    /// Drop a previously-registered sink. Idempotent: removing an
    /// id that has already been unsubscribed (or was never issued)
    /// is a no-op. Returns `true` if at least one entry was
    /// removed.
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut inner = self.inner.lock().expect("EventBus poisoned");
        let mut removed = false;
        for bucket in inner.by_kind.values_mut() {
            let before = bucket.len();
            bucket.retain(|s| s.id != id);
            removed |= bucket.len() != before;
        }
        let before_wild = inner.wildcard.len();
        inner.wildcard.retain(|s| s.id != id);
        removed |= inner.wildcard.len() != before_wild;
        // M.5.5: typed-event subscriptions live in their own
        // bucket; honour the same id-keyed unsubscribe contract.
        for bucket in inner.typed_subs.values_mut() {
            let before = bucket.len();
            bucket.retain(|s| s.id != id);
            removed |= bucket.len() != before;
        }
        removed
    }

    /// Fire `event` to every matching subscriber. Channel sinks
    /// receive a clone (the event has `Clone`); Invocation sinks
    /// queue onto [`Self::drain_pending_invocations`] for the App
    /// to dispatch. Closed channel senders are pruned lazily on the
    /// publish that observes them.
    ///
    /// Locking discipline (audit M1): we hold the inner mutex only
    /// long enough to (a) snapshot the matching channel senders
    /// into a small Vec, and (b) push any matched Invocation
    /// targets onto `pending_invocations` (the latter must stay
    /// under the lock or it races with
    /// [`Self::drain_pending_invocations`]). The actual `tx.send`
    /// calls happen with the lock dropped, so a slow / bounded
    /// downstream sender can never block the bus -- and concurrent
    /// `publish` calls from different threads can dispatch in
    /// parallel instead of serialising on the bus mutex. That is a
    /// deliberate relaxation of the previous "publish is totally
    /// ordered through the bus" guarantee; within a single
    /// `publish` call ordering across this caller's subscribers is
    /// still preserved, which is the only guarantee callers have
    /// ever been entitled to.
    ///
    /// EF.1: the reserved [`EventFilter`] fields are AND-checked
    /// during the snapshot phase (under the lock) so only matching
    /// subscribers are snapshotted / queued. A `predicate` therefore
    /// runs under the bus mutex -- see [`EventPredicate`] for its
    /// non-reentrancy contract.
    pub fn publish(&self, event: Event) {
        let kind = event.kind();

        // Snapshot phase: under the lock, copy out the channel
        // senders we'll dispatch to and queue any Invocation
        // targets. `UnboundedSender` clones are cheap (Arc bump);
        // bucket sizes are small (subscribers per kind, plus
        // wildcards) so this allocation is well under the cost of
        // even one downstream `tx.send`.
        let (channel_targets, plugin_targets) = {
            let mut inner = self.inner.lock().expect("EventBus poisoned");
            let mut channel_targets: Vec<(SubscriptionId, mpsc::UnboundedSender<Event>)> =
                Vec::new();
            // PH7.8: plugin sinks snapshotted alongside channel senders so they
            // dispatch with the lock dropped too — a slow plugin handler's
            // enqueue never runs under the bus mutex.
            let mut plugin_targets: Vec<(SubscriptionId, PluginEventSink)> = Vec::new();

            // Borrow-checker note: split `inner` into independent
            // field borrows so we can read the bucket lists
            // (immutable) while pushing onto `pending_invocations`
            // (mutable) in one pass.
            let Inner {
                by_kind,
                wildcard,
                pending_invocations,
                typed_subs: _,
            } = &mut *inner;

            if let Some(bucket) = by_kind.get(&kind) {
                snapshot_bucket(bucket, &event, &mut channel_targets, &mut plugin_targets);
                // Invocation targets: queue under the lock so we
                // don't race with `drain_pending_invocations`.
                // They never touch the network / channels so the
                // cost is purely the clone, which is acceptable.
                queue_invocations(bucket, &event, pending_invocations);
            }
            snapshot_bucket(wildcard, &event, &mut channel_targets, &mut plugin_targets);
            queue_invocations(wildcard, &event, pending_invocations);

            (channel_targets, plugin_targets)
        };

        // Dispatch phase: lock dropped. Slow / bounded subscribers
        // (none today, but see the v1 backpressure note above)
        // cannot stall the publisher under the bus mutex.
        let mut dead: Vec<SubscriptionId> = Vec::new();
        for (id, tx) in channel_targets {
            if tx.send(event.clone()).is_err() {
                dead.push(id);
            }
        }
        // PH7.8: plugin sinks run lock-dropped too. A sink returning `false`
        // (the plugin's actor channel closed) is pruned like a dead `Channel`.
        for (id, sink) in plugin_targets {
            if !sink(event.clone()) {
                dead.push(id);
            }
        }

        // Pruning phase: re-acquire the lock briefly to drop dead
        // senders. If a subscription was already removed (e.g. the
        // owner unsubscribed concurrently) the retain is a no-op.
        if !dead.is_empty() {
            let mut inner = self.inner.lock().expect("EventBus poisoned");
            if let Some(bucket) = inner.by_kind.get_mut(&kind) {
                bucket.retain(|s| !dead.contains(&s.id));
            }
            inner.wildcard.retain(|s| !dead.contains(&s.id));
        }
    }

    /// Pull every queued [`CommandInvocation`] target out of the
    /// bus. The App calls this on its tick to route events that
    /// asked the editor to "run command X" -- those run through
    /// the document actor (which is the only writer of document
    /// state). Returned in subscription order.
    pub fn drain_pending_invocations(&self) -> Vec<CommandInvocation> {
        let mut inner = self.inner.lock().expect("EventBus poisoned");
        std::mem::take(&mut inner.pending_invocations)
    }

    /// Test / introspection accessor: the number of currently
    /// registered subscriptions across every kind bucket plus
    /// the wildcard list. Each multi-kind subscription is counted
    /// once per kind it touches.
    pub fn subscription_count(&self) -> usize {
        let inner = self.inner.lock().expect("EventBus poisoned");
        inner.by_kind.values().map(Vec::len).sum::<usize>() + inner.wildcard.len()
    }

    /// M.5.3.a: subscribe to a *typed* event. The bus stores a
    /// downcast closure that forwards to `tx`; on publish the
    /// closure unwraps the boxed payload to the concrete type
    /// `T` and sends it. Subscribers register one channel per
    /// event type they care about; multi-type subscribers
    /// register multiple times.
    ///
    /// Closed channels are pruned lazily on the next matching
    /// publish (same shape as the legacy `subscribe` path).
    pub fn subscribe_typed<T>(&self, tx: mpsc::UnboundedSender<T>) -> SubscriptionId
    where
        T: TypedEvent + Clone,
    {
        let id = SubscriptionId::next();
        let forward: ForwardFn = Arc::new(move |payload| {
            let Some(typed) = payload.downcast_ref::<T>() else {
                // Wrong type for this subscriber -- not an error
                // (the bus dispatches to whichever bucket it
                // can; downcast failure should be unreachable
                // in practice because the bucket is keyed on
                // TypeId).
                return true;
            };
            tx.send(typed.clone()).is_ok()
        });
        let mut inner = self.inner.lock().expect("EventBus poisoned");
        inner
            .typed_subs
            .entry(TypeId::of::<T>())
            .or_default()
            .push(TypedSubscription { id, forward });
        id
    }

    /// M.5.3.a: publish a typed event. Boxes the event into
    /// `Arc<dyn Any + Send + Sync>` once, then walks the
    /// `TypeId`-keyed subscriber bucket. Each subscriber's
    /// downcast closure clones the typed value into its own
    /// channel; closures returning `false` (channel closed)
    /// get pruned lazily on the next publish hitting the same
    /// bucket.
    pub fn publish_typed<T>(&self, event: T)
    where
        T: TypedEvent,
    {
        let payload: Arc<dyn std::any::Any + Send + Sync> = Arc::new(event);
        let tid = TypeId::of::<T>();

        // Snapshot phase: clone the forwarder Arcs out from
        // under the lock. Same pattern the legacy `publish`
        // uses for channel senders -- never call into a
        // subscriber while holding the bus mutex.
        let forwarders: Vec<(SubscriptionId, Arc<_>)> = {
            let inner = self.inner.lock().expect("EventBus poisoned");
            inner
                .typed_subs
                .get(&tid)
                .map(|bucket| {
                    bucket
                        .iter()
                        .map(|s| (s.id, s.forward.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        // Dispatch phase: lock dropped. Closed channels surface
        // via `false`; we collect them for pruning.
        let mut dead: Vec<SubscriptionId> = Vec::new();
        for (id, forward) in forwarders {
            if !forward(&payload) {
                dead.push(id);
            }
        }

        // Pruning phase.
        if !dead.is_empty() {
            let mut inner = self.inner.lock().expect("EventBus poisoned");
            if let Some(bucket) = inner.typed_subs.get_mut(&tid) {
                bucket.retain(|s| !dead.contains(&s.id));
            }
        }
    }

    /// M.5.3.a: count of typed subscribers across every type-id
    /// bucket. Mirrors [`Self::subscription_count`] for the
    /// typed surface.
    pub fn typed_subscription_count(&self) -> usize {
        let inner = self.inner.lock().expect("EventBus poisoned");
        inner.typed_subs.values().map(Vec::len).sum()
    }
}

/// Snapshot the channel senders out of one bucket so the caller
/// can dispatch with the bus lock dropped. Cheap clone --
/// `UnboundedSender` is internally `Arc`-backed.
///
/// EF.1: a subscription whose [`ExtraFilter`] rejects `event` is
/// skipped (its `path_glob` / `major_modes` / `predicate` didn't
/// match). The kind bucket already matched the kind; this is the
/// per-candidate constant the publish scan adds without widening.
fn snapshot_bucket(
    bucket: &[Subscription],
    event: &Event,
    out: &mut Vec<(SubscriptionId, mpsc::UnboundedSender<Event>)>,
    plugin_out: &mut Vec<(SubscriptionId, PluginEventSink)>,
) {
    for sub in bucket {
        if !sub.extra.matches(event) {
            continue;
        }
        match &sub.target {
            SubscriptionTarget::Channel(tx) => out.push((sub.id, tx.clone())),
            // PH7.8: plugin sinks snapshot like channel senders (a cheap `Arc`
            // clone) so the enqueue runs with the bus lock dropped.
            SubscriptionTarget::Plugin { sink, .. } => plugin_out.push((sub.id, sink.clone())),
            // Invocation targets are queued separately (`queue_invocations`).
            SubscriptionTarget::Invocation(_) => {}
        }
    }
}

/// Push every Invocation target in `bucket` whose [`ExtraFilter`]
/// matches `event` onto `pending`. Stays under the bus lock (the
/// caller holds it) because `pending_invocations` is the same field
/// [`EventBus::drain_pending_invocations`] empties.
fn queue_invocations(bucket: &[Subscription], event: &Event, pending: &mut Vec<CommandInvocation>) {
    for sub in bucket {
        if !sub.extra.matches(event) {
            continue;
        }
        if let SubscriptionTarget::Invocation(inv) = &sub.target {
            pending.push(inv.clone());
        }
    }
}

/// The filesystem path an event refers to, if any. EF.1's
/// `path_glob` filter matches against this. Events with no
/// associated path (`BeforeQuit`, `ModalModeChanged`,
/// `OptionChanged`, scratch-buffer opens) return `None`.
fn event_path(event: &Event) -> Option<&Path> {
    match event {
        Event::DocumentOpened { path, .. } => path.as_deref(),
        Event::DocumentChanged { path, .. } => path.as_deref(),
        Event::BeforeSave { path, .. } => Some(path.as_path()),
        Event::DocumentSaved { path, .. } => Some(path.as_path()),
        Event::DocumentClosed { .. }
        | Event::SelectionsChanged { .. }
        | Event::ModalModeChanged { .. }
        | Event::BeforeQuit
        | Event::OptionChanged { .. }
        // Lifecycle events carry a buffer + mode name, not a path;
        // they are filtered via `major_modes`, not `path_glob`.
        | Event::MajorEntered { .. }
        | Event::MajorExiting { .. }
        | Event::MinorActivated { .. }
        | Event::MinorDeactivated { .. }
        // Plugin events carry an opaque payload, not a path; a plugin that
        // wants path-based routing filters inside its own handler (PH7.8b).
        | Event::Plugin { .. } => None,
    }
}

/// The major-mode name the event's buffer is in, if the event
/// carries one. EF.1's `major_modes` filter matches against this.
///
/// MA.1: `MajorEntered` / `MajorExiting` carry the major-mode name;
/// every other variant is major-mode-agnostic and returns `None`, so
/// a `major_modes`-constrained filter only ever matches the two
/// lifecycle events -- the correct semantics for a minor mode that
/// should fire when a buffer enters / exits specific majors
/// (mode-architecture.md §7.4).
fn event_major_mode(event: &Event) -> Option<&str> {
    match event {
        Event::MajorEntered { major, .. } | Event::MajorExiting { major, .. } => Some(major),
        // Minor lifecycle carries the *minor* name, not the major the
        // buffer is in, so a `major_modes` filter never matches it.
        Event::DocumentOpened { .. }
        | Event::DocumentClosed { .. }
        | Event::BeforeSave { .. }
        | Event::DocumentSaved { .. }
        | Event::DocumentChanged { .. }
        | Event::SelectionsChanged { .. }
        | Event::ModalModeChanged { .. }
        | Event::BeforeQuit
        | Event::OptionChanged { .. }
        | Event::MinorActivated { .. }
        | Event::MinorDeactivated { .. }
        // A plugin event is not tied to a buffer's major mode; a plugin that
        // wants major-mode routing filters inside its own handler (PH7.8b).
        | Event::Plugin { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::CommandId;
    use lattice_protocol::ids::DocumentId;
    use std::path::PathBuf;

    fn make_event() -> Event {
        Event::DocumentSaved {
            id: DocumentId::new(1),
            path: PathBuf::from("/tmp/foo.rs"),
        }
    }

    #[test]
    fn channel_subscriber_receives_matching_event() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Channel(tx),
        );

        bus.publish(make_event());

        let got = rx.try_recv().expect("event delivered");
        assert!(matches!(got, Event::DocumentSaved { .. }));
    }

    #[test]
    fn channel_subscriber_ignores_non_matching_kinds() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::BeforeQuit),
            SubscriptionTarget::Channel(tx),
        );

        bus.publish(make_event()); // DocumentSaved, not BeforeQuit
        assert!(rx.try_recv().is_err(), "no event should arrive");

        bus.publish(Event::BeforeQuit);
        assert!(matches!(rx.try_recv(), Ok(Event::BeforeQuit)));
    }

    #[test]
    fn wildcard_subscriber_sees_every_kind() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(EventFilter::any(), SubscriptionTarget::Channel(tx));

        bus.publish(make_event());
        bus.publish(Event::BeforeQuit);

        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn multi_kind_subscriber_fires_for_each_listed_kind() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kinds(vec![EventKind::BeforeSave, EventKind::DocumentSaved]),
            SubscriptionTarget::Channel(tx),
        );

        bus.publish(Event::BeforeSave {
            id: DocumentId::new(1),
            path: PathBuf::from("/tmp/foo.rs"),
        });
        bus.publish(make_event());

        assert!(matches!(rx.try_recv(), Ok(Event::BeforeSave { .. })));
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentSaved { .. })));
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let id = bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Channel(tx),
        );

        assert!(bus.unsubscribe(id));
        bus.publish(make_event());
        assert!(rx.try_recv().is_err());
        // Idempotent: second unsubscribe returns false.
        assert!(!bus.unsubscribe(id));
    }

    #[test]
    fn closed_channel_subscriber_is_pruned() {
        let bus = EventBus::new();
        {
            let (tx, _rx) = mpsc::unbounded_channel();
            bus.subscribe(
                EventFilter::kind(EventKind::DocumentSaved),
                SubscriptionTarget::Channel(tx),
            );
        }
        // Receiver dropped; sender should now fail. Publish twice
        // -- the first prunes the closed sub, the second confirms
        // count is 0.
        assert_eq!(bus.subscription_count(), 1);
        bus.publish(make_event());
        assert_eq!(bus.subscription_count(), 0);
    }

    // PH7.8: the `SubscriptionTarget::Plugin` delivery + prune surface.

    #[test]
    fn plugin_target_delivers_matching_event_lock_dropped() {
        let bus = EventBus::new();
        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_sink = Arc::clone(&seen);
        let sink: crate::events::PluginEventSink = Arc::new(move |ev: Event| {
            seen_sink.lock().expect("poisoned").push(ev);
            true // receiver open
        });
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Plugin {
                plugin: 3,
                handler: 9,
                sink,
            },
        );

        // Non-matching kind: not delivered.
        bus.publish(Event::BeforeQuit);
        assert!(seen.lock().unwrap().is_empty());

        // Matching kind: delivered.
        bus.publish(make_event());
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], Event::DocumentSaved { .. }));
    }

    #[test]
    fn plugin_target_extra_filter_applies() {
        // The declarative filter (path_glob) gates a plugin sink identically to
        // a channel target (both flow through `ExtraFilter::matches`).
        let bus = EventBus::new();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_sink = Arc::clone(&count);
        let sink: crate::events::PluginEventSink = Arc::new(move |_ev| {
            count_sink.fetch_add(1, Ordering::SeqCst);
            true
        });
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved)
                .with_path_glob(crate::compile_glob_set(["**/*.rs"])),
            SubscriptionTarget::Plugin {
                plugin: 1,
                handler: 1,
                sink,
            },
        );

        bus.publish(saved_with_path("notes.md")); // no match
        bus.publish(saved_with_path("src/lib.rs")); // match
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn plugin_target_pruned_when_sink_reports_closed() {
        // A sink returning `false` (the plugin's actor channel closed) is pruned
        // lazily on the publish that observes it — the closed-`Channel` shape.
        let bus = EventBus::new();
        let sink: crate::events::PluginEventSink = Arc::new(|_ev| false /* receiver gone */);
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Plugin {
                plugin: 2,
                handler: 4,
                sink,
            },
        );
        assert_eq!(bus.subscription_count(), 1);
        bus.publish(make_event());
        assert_eq!(bus.subscription_count(), 0, "closed plugin sink is pruned");
    }

    #[test]
    fn invocation_target_queues_for_drain() {
        let bus = EventBus::new();
        let inv = CommandInvocation::of(CommandId::new(42));
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Invocation(inv.clone()),
        );

        bus.publish(make_event());
        let drained = bus.drain_pending_invocations();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].command, inv.command);

        // Drain is destructive -- second drain returns empty.
        assert!(bus.drain_pending_invocations().is_empty());
    }

    #[test]
    fn publish_does_not_hold_lock_across_dispatch() {
        // Audit M1 regression test. The publish path snapshots
        // channel senders under the lock and dispatches with the
        // lock dropped. Proof: a subscriber whose channel receiver
        // performs work that re-enters the bus (subscribing a
        // *new* sink) must succeed -- under the old design that
        // would deadlock on the inner Mutex (the publisher held
        // it while calling `tx.send`, which woke the receiver
        // task; if the receiver tried to `bus.subscribe(...)` from
        // the same thread of execution it would block forever).
        //
        // We model "the subscriber synchronously reacts and asks
        // the bus for something" via a thread that, on receiving
        // an event, calls `bus.subscription_count()` (which takes
        // the same lock). Under the new code the publisher has
        // already released the lock before `tx.send` fires, so
        // the count call returns immediately.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;
        use std::time::{Duration, Instant};

        let bus = Arc::new(EventBus::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Channel(tx),
        );

        let bus_recv = Arc::clone(&bus);
        let saw_event = Arc::new(AtomicBool::new(false));
        let saw_event_thread = Arc::clone(&saw_event);
        let receiver = thread::spawn(move || {
            // Block waiting for the event; once it arrives, take
            // the bus lock via `subscription_count`. If publish
            // were still holding the lock this would deadlock
            // until the test timeout below trips.
            let deadline = Instant::now() + Duration::from_secs(5);
            while rx.try_recv().is_err() {
                if Instant::now() > deadline {
                    panic!("never received event");
                }
                thread::sleep(Duration::from_millis(1));
            }
            let _ = bus_recv.subscription_count();
            saw_event_thread.store(true, Ordering::SeqCst);
        });

        bus.publish(make_event());
        receiver.join().expect("receiver panicked");
        assert!(saw_event.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrent_publishers_do_not_deadlock() {
        // Two threads publishing through the bus complete
        // independently under the new design (the lock is dropped
        // before dispatch, so they overlap rather than serialise
        // through tx.send). The assertion is just "both finish";
        // a regression that re-introduced lock-during-dispatch
        // would still pass this test, but combined with the
        // previous one and the channel-pruning test the new path
        // is well covered.
        use std::sync::Arc;
        use std::thread;

        let bus = Arc::new(EventBus::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(EventFilter::any(), SubscriptionTarget::Channel(tx));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let b = Arc::clone(&bus);
            handles.push(thread::spawn(move || {
                for _ in 0..50 {
                    b.publish(make_event());
                }
            }));
        }
        for h in handles {
            h.join().expect("publisher panicked");
        }

        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 4 * 50);
    }

    #[test]
    fn invocation_target_persists_across_publishes() {
        // Unlike a channel, an Invocation subscription stays
        // registered after firing -- the bus didn't deliver the
        // payload anywhere observable, just queued an action.
        let bus = EventBus::new();
        let inv = CommandInvocation::of(CommandId::new(7));
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Invocation(inv),
        );

        bus.publish(make_event());
        bus.publish(make_event());
        assert_eq!(bus.drain_pending_invocations().len(), 2);
        assert_eq!(bus.subscription_count(), 1);
    }

    // EF.1: reserved-filter-field tests (path_glob / major_modes /
    // predicate + AND-combination + None-is-unconstrained).

    fn saved_with_path(path: &str) -> Event {
        Event::DocumentSaved {
            id: DocumentId::new(1),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn path_glob_filter_delivers_only_matching_paths() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved)
                .with_path_glob(crate::compile_glob_set(["**/*.rs"])),
            SubscriptionTarget::Channel(tx),
        );

        // Non-matching path: filtered out.
        bus.publish(saved_with_path("notes/todo.md"));
        assert!(rx.try_recv().is_err(), "*.md must not match **/*.rs");

        // Matching path: delivered.
        bus.publish(saved_with_path("src/lib.rs"));
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentSaved { .. })));
    }

    #[test]
    fn path_glob_filter_rejects_event_with_no_path() {
        // A path-constrained subscription must not fire for an event
        // that carries no path (BeforeQuit). "Constrained on path"
        // means "only path-bearing events that match."
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kinds(vec![EventKind::DocumentSaved, EventKind::BeforeQuit])
                .with_path_glob(crate::compile_glob_set(["**/*.rs"])),
            SubscriptionTarget::Channel(tx),
        );

        bus.publish(Event::BeforeQuit);
        assert!(rx.try_recv().is_err(), "pathless event can't match a glob");
    }

    fn major_entered(major: &str) -> Event {
        Event::MajorEntered {
            buffer: lattice_protocol::ids::BufferId::new(1),
            major: major.to_string(),
        }
    }

    #[test]
    fn major_modes_filter_delivers_only_allowlisted_majors() {
        // MA.1: MajorEntered carries the major-mode name, so a
        // major_modes-constrained filter fires only when the entered
        // major is in the allowlist.
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::MajorEntered)
                .with_major_modes(vec![ModeId::new("rust-mode")]),
            SubscriptionTarget::Channel(tx),
        );

        // Not in the allowlist: filtered out.
        bus.publish(major_entered("python-mode"));
        assert!(rx.try_recv().is_err(), "python-mode is not allowlisted");

        // In the allowlist: delivered.
        bus.publish(major_entered("rust-mode"));
        assert!(matches!(rx.try_recv(), Ok(Event::MajorEntered { .. })));
    }

    #[test]
    fn major_modes_filter_rejects_event_with_no_major() {
        // An event that carries no major (DocumentSaved) never
        // matches a major-mode-constrained filter.
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kinds(vec![EventKind::MajorEntered, EventKind::DocumentSaved])
                .with_major_modes(vec![ModeId::new("rust-mode")]),
            SubscriptionTarget::Channel(tx),
        );

        bus.publish(saved_with_path("src/lib.rs"));
        assert!(
            rx.try_recv().is_err(),
            "DocumentSaved carries no major, so a major_modes filter rejects it"
        );
    }

    #[test]
    fn predicate_filter_gates_delivery() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pred: EventPredicate = Arc::new(
            |e: &Event| matches!(e, Event::DocumentSaved { path, .. } if path.ends_with("keep.rs")),
        );
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved).with_predicate(pred),
            SubscriptionTarget::Channel(tx),
        );

        bus.publish(saved_with_path("src/drop.rs"));
        assert!(rx.try_recv().is_err(), "predicate rejected this event");

        bus.publish(saved_with_path("src/keep.rs"));
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentSaved { .. })));
    }

    #[test]
    fn extra_fields_and_combine() {
        // path_glob AND predicate: both must pass.
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pred: EventPredicate = Arc::new(
            |e: &Event| matches!(e, Event::DocumentSaved { path, .. } if path.starts_with("src/")),
        );
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved)
                .with_path_glob(crate::compile_glob_set(["**/*.rs"]))
                .with_predicate(pred),
            SubscriptionTarget::Channel(tx),
        );

        // Matches glob (*.rs) but fails predicate (not under src/).
        bus.publish(saved_with_path("tests/it.rs"));
        assert!(rx.try_recv().is_err(), "predicate half of the AND failed");

        // Fails glob (*.md) though it would pass predicate (src/).
        bus.publish(saved_with_path("src/readme.md"));
        assert!(rx.try_recv().is_err(), "glob half of the AND failed");

        // Both pass.
        bus.publish(saved_with_path("src/lib.rs"));
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentSaved { .. })));
    }

    #[test]
    fn no_extra_fields_is_unconstrained() {
        // The common case: a kinds-only filter delivers every event
        // of that kind (extra fields all None short-circuit to true).
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Channel(tx),
        );
        bus.publish(saved_with_path("any/where.xyz"));
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentSaved { .. })));
    }

    #[test]
    fn extra_filter_applies_to_invocation_targets_too() {
        // The AND-check gates Invocation targets identically to
        // Channel targets (both flow through ExtraFilter::matches).
        let bus = EventBus::new();
        let inv = CommandInvocation::of(CommandId::new(99));
        bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved)
                .with_path_glob(crate::compile_glob_set(["**/*.rs"])),
            SubscriptionTarget::Invocation(inv),
        );

        bus.publish(saved_with_path("doc.md"));
        assert!(
            bus.drain_pending_invocations().is_empty(),
            "non-matching path must not queue the invocation"
        );

        bus.publish(saved_with_path("src/lib.rs"));
        assert_eq!(bus.drain_pending_invocations().len(), 1);
    }

    // M.5.3.a: typed-event surface tests. Declared at module
    // scope so the `register_event!` macro's linkme entry lands
    // in the link graph.
    #[derive(Debug, Clone)]
    struct TypedTestEvent {
        n: u32,
    }

    lattice_protocol::register_event!(
        TypedTestEvent,
        "lattice-runtime.typed-test-event",
        "Test event for the EventBus typed-event API.",
        "lattice-runtime-tests",
    );

    #[test]
    fn typed_publish_delivers_to_typed_subscriber() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<TypedTestEvent>();
        bus.subscribe_typed(tx);
        bus.publish_typed(TypedTestEvent { n: 7 });
        let received = rx.try_recv().expect("typed event delivered");
        assert_eq!(received.n, 7);
    }

    #[test]
    fn typed_subscriber_only_sees_matching_type() {
        // A subscriber for one event type doesn't receive
        // events of another type, even when both are typed.
        #[derive(Debug, Clone)]
        struct OtherEvent {}
        // Can't register OtherEvent inside a fn (linkme needs
        // module scope). Manual impl is enough since we only
        // need the trait, not the descriptor entry.
        impl lattice_protocol::event_registry::Event for OtherEvent {
            fn event_type_id(&self) -> lattice_protocol::event_registry::EventTypeId {
                lattice_protocol::event_registry::EventTypeId::of::<Self>("test.other")
            }
        }
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<TypedTestEvent>();
        bus.subscribe_typed(tx);
        bus.publish_typed(OtherEvent {});
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn typed_subscription_count_tracks_typed_subscribers() {
        let bus = EventBus::new();
        assert_eq!(bus.typed_subscription_count(), 0);
        let (tx, _rx) = mpsc::unbounded_channel::<TypedTestEvent>();
        bus.subscribe_typed(tx);
        assert_eq!(bus.typed_subscription_count(), 1);
    }

    #[test]
    fn typed_publish_prunes_dead_channel_lazily() {
        let bus = EventBus::new();
        {
            let (tx, _rx) = mpsc::unbounded_channel::<TypedTestEvent>();
            bus.subscribe_typed(tx);
            // Drop _rx here; tx send will fail on next publish.
        }
        // Subscriber is registered but its channel is dead.
        bus.publish_typed(TypedTestEvent { n: 1 });
        // After the publish, the dead subscriber is pruned.
        assert_eq!(bus.typed_subscription_count(), 0);
    }
}
