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
//! - Filters by [`lattice_protocol::EventKind`] only. Document
//!   pattern, major-mode, and predicate filters are declared in
//!   the design doc and stay TODO until callers need them.
//! - Sinks are [`SubscriptionTarget::Channel`] (an `mpsc::Sender`)
//!   or [`SubscriptionTarget::Invocation`] (a `CommandInvocation`
//!   the bus runs through the document actor's dispatch when the
//!   App wires that path).
//! - Plugin handler target ([`SubscriptionTarget::Plugin`] in
//!   §5.10) is omitted -- WASM hosting isn't online in v1.
//! - Indexed dispatch: subscriptions live in a
//!   `HashMap<EventKind, Vec<Subscription>>`. Publish iterates the
//!   bucket for one kind, never the global list.
//! - Bus is `Send + Sync`; the inner state is one `Mutex` (write
//!   path: subscribe / unsubscribe / publish). Lock contention is
//!   not a concern at v1 publish rates (handful per keystroke).
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

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_grammar::CommandInvocation;
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

/// Filter applied at publish time. v1 honors `kinds` only; the
/// other fields are reserved (the design declares them in §5.10).
#[derive(Debug, Default, Clone)]
pub struct EventFilter {
    /// Kinds this subscription cares about. `None` means "all
    /// kinds" -- the wildcard. v1 callers should always pass
    /// `Some(...)` to keep dispatch indexed; the wildcard is
    /// supported for one-off debugging / introspection sinks.
    pub kinds: Option<Vec<EventKind>>,
}

impl EventFilter {
    /// Convenience: subscribe to a single kind.
    pub fn kind(k: EventKind) -> Self {
        Self {
            kinds: Some(vec![k]),
        }
    }

    /// Convenience: subscribe to a list of kinds.
    pub fn kinds(ks: Vec<EventKind>) -> Self {
        Self { kinds: Some(ks) }
    }

    /// Wildcard: every event.
    pub fn any() -> Self {
        Self { kinds: None }
    }
}

/// What the bus does when a matching event arrives.
#[derive(Debug, Clone)]
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
}

#[derive(Debug)]
struct Subscription {
    id: SubscriptionId,
    target: SubscriptionTarget,
}

#[derive(Debug, Default)]
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
        let mut inner = self.inner.lock().expect("EventBus poisoned");
        match filter.kinds {
            Some(kinds) => {
                // Multi-kind subscriptions register once per kind
                // bucket so dispatch never has to re-check the
                // filter for the kind it already matched on.
                for k in kinds {
                    inner.by_kind.entry(k).or_default().push(Subscription {
                        id,
                        target: target.clone(),
                    });
                }
            }
            None => inner.wildcard.push(Subscription { id, target }),
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
        removed || inner.wildcard.len() != before_wild
    }

    /// Fire `event` to every matching subscriber. Channel sinks
    /// receive a clone (the event has `Clone`); Invocation sinks
    /// queue onto [`Self::drain_pending_invocations`] for the App
    /// to dispatch. Closed channel senders are removed in-place.
    pub fn publish(&self, event: Event) {
        let kind = event.kind();
        let mut inner = self.inner.lock().expect("EventBus poisoned");

        // Visit the indexed bucket then the wildcard sinks. We
        // collect Invocation targets first (then push at the end)
        // so the lock is held briefly and we never re-enter
        // publish from a handler that consumes a queued
        // invocation.
        let mut new_invocations: Vec<CommandInvocation> = Vec::new();
        if let Some(bucket) = inner.by_kind.get_mut(&kind) {
            dispatch_bucket(bucket, &event, &mut new_invocations);
        }
        dispatch_bucket(&mut inner.wildcard, &event, &mut new_invocations);

        if !new_invocations.is_empty() {
            inner.pending_invocations.extend(new_invocations);
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
}

/// Dispatch a single bucket's subscriptions against `event`. Closed
/// channel sinks are pruned in-place; Invocation targets are
/// pushed onto `out_invocations` for the bus to surface via
/// [`EventBus::drain_pending_invocations`].
fn dispatch_bucket(
    bucket: &mut Vec<Subscription>,
    event: &Event,
    out_invocations: &mut Vec<CommandInvocation>,
) {
    bucket.retain(|sub| match &sub.target {
        SubscriptionTarget::Channel(tx) => tx.send(event.clone()).is_ok(),
        SubscriptionTarget::Invocation(inv) => {
            out_invocations.push(inv.clone());
            true
        }
    });
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
}
