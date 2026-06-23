//! Boot-composition BC.1: the generic *inbound* primitive.
//!
//! Generalizes the I3 `ClaudeCodeInboundBus` and LSP's hand-rolled inbound
//! buses into ONE reusable primitive: a channel whose `send` **wakes the
//! editor** (`async_landed.notify_one`) so off-keystroke work reaches the
//! screen WITHOUT a keypress, and whose items are drained per-tick through a
//! handler that maps each to existing [`Effect`]s.
//!
//! CRITICAL (paramount goal #4 — async-correct *by construction*, not by
//! discipline): the wake is baked into [`InboundBus::send`], so it is
//! structurally impossible to forget. This is the bug class
//! `boot-composition.md` §3 designs out: "forget the wake and a refresh only
//! reaches the screen on the next keypress."
//!
//! Pairs with [`TickCallbackRegistry`](crate::tick_callback::TickCallbackRegistry):
//! [`make_inbound`] returns the bus PLUS a [`TickCallback`] drain closure the
//! caller registers with the registry. Per `feedback_mode_owns_its_surface`,
//! the `handler` (the map from request → `Effect`) lives in the owning crate;
//! the host only runs the generic drain + applies the returned effects.
//!
//! The host-facing bundle (`lattice_host::BootContext`) exposes this as
//! `BootContext::inbound::<T>(handler)`, which calls [`make_inbound`] against
//! the editor's `async_landed` and registers the drain on the shared
//! tick-callback registry — so a subsystem gets the bus back and never
//! touches the wake or the registry directly.

use std::sync::Arc;

use lattice_grammar::effect::Effect;
use tokio::sync::{Notify, mpsc};

use crate::tick_callback::TickCallback;

/// Sender half of the inbound primitive. Held by the off-thread producer (a
/// WS task, an LSP forwarder, …); `send` hands an item to the editor thread
/// and wakes it.
///
/// `Clone` is implemented manually so `T` need *not* be `Clone` — only the
/// channel sender and the `Arc<Notify>` are cloned (a `#[derive(Clone)]`
/// would spuriously bound `T: Clone`).
pub struct InboundBus<T> {
    tx: mpsc::UnboundedSender<T>,
    wake: Arc<Notify>,
}

impl<T> Clone for InboundBus<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: Arc::clone(&self.wake),
        }
    }
}

impl<T> InboundBus<T> {
    /// Send an item and **wake the editor** so the per-tick drain runs
    /// off-keystroke. The wake fires only on a successful send.
    ///
    /// Returns the item back on failure (receiver dropped — the subsystem
    /// stopped) so the caller reports a graceful error instead of awaiting a
    /// reply that will never resolve. Mirrors `ClaudeCodeInboundBus::send`.
    pub fn send(&self, item: T) -> Result<(), T> {
        self.tx.send(item).map_err(|e| e.0)?;
        self.wake.notify_one();
        Ok(())
    }
}

/// Build an inbound primitive.
///
/// Returns the [`InboundBus`] (the sender, whose `send` wakes `wake`) and a
/// [`TickCallback`] drain closure. The caller registers the drain with the
/// [`TickCallbackRegistry`](crate::tick_callback::TickCallbackRegistry); each
/// tick the drain `try_recv`s every pending item, runs it through `handler`,
/// and returns the concatenated `Effect`s for the host to apply.
///
/// `handler` maps one inbound item to zero or more `Effect`s — the
/// crate-owned validate→map step (e.g. the I3 optimistic-ack: resolve the
/// request's oneshot, return one effect on a valid target, none on an unknown
/// one). It is `FnMut` so it may carry mutable state (a read-state cache, a
/// counter) across drains, exactly like the existing `make_drain` closures.
pub fn make_inbound<T, H>(wake: Arc<Notify>, mut handler: H) -> (InboundBus<T>, TickCallback)
where
    T: Send + 'static,
    H: FnMut(T) -> Vec<Effect> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<T>();
    let bus = InboundBus { tx, wake };
    let drain: TickCallback = Box::new(move || {
        let mut effects = Vec::new();
        while let Ok(item) = rx.try_recv() {
            effects.extend(handler(item));
        }
        effects
    });
    (bus, drain)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A non-`Clone` payload, to prove `InboundBus<T>: Clone` does not require
    /// `T: Clone`. (`Debug` only so `send(..).unwrap()` can format the `Err`.)
    #[derive(Debug)]
    struct NotClone(u32);

    #[test]
    fn drain_runs_handler_over_each_item_in_order() {
        let wake = Arc::new(Notify::new());
        let seen = Arc::new(Mutex::new(Vec::<u32>::new()));
        let seen_in = Arc::clone(&seen);
        let (bus, mut drain) = make_inbound::<u32, _>(wake, move |n| {
            seen_in.lock().unwrap().push(n);
            // Return one effect per item so ordering / concatenation is also
            // observable at the effect level.
            vec![Effect::None]
        });

        bus.send(1).unwrap();
        bus.send(2).unwrap();
        bus.send(3).unwrap();

        let effects = drain();
        assert_eq!(effects.len(), 3, "one effect per drained item");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![1, 2, 3],
            "handler runs over items in send order"
        );
    }

    #[test]
    fn drain_with_nothing_pending_is_empty() {
        let wake = Arc::new(Notify::new());
        let (_bus, mut drain) = make_inbound::<u32, _>(wake, |_| vec![Effect::None]);
        assert!(drain().is_empty(), "no items pending → no effects");
    }

    #[test]
    fn handler_may_drop_items_returning_no_effect() {
        // The I3 optimistic-ack maps unknown targets to *no* effect. Prove a
        // handler returning an empty vec contributes nothing.
        let wake = Arc::new(Notify::new());
        let (bus, mut drain) = make_inbound::<u32, _>(wake, |n| {
            if n % 2 == 0 {
                vec![Effect::None]
            } else {
                Vec::new()
            }
        });
        bus.send(1).unwrap(); // dropped
        bus.send(2).unwrap(); // kept
        bus.send(3).unwrap(); // dropped
        assert_eq!(drain().len(), 1, "only the even item yields an effect");
    }

    #[tokio::test]
    async fn send_wakes_the_editor() {
        let wake = Arc::new(Notify::new());
        let (bus, _drain) = make_inbound::<u32, _>(Arc::clone(&wake), |_| vec![Effect::None]);
        bus.send(7).unwrap();
        // The permit stored by `notify_one` must let a `notified()` resolve
        // promptly — i.e. the actor would wake off-keystroke.
        let woke = tokio::time::timeout(Duration::from_millis(200), wake.notified()).await;
        assert!(woke.is_ok(), "send must wake the editor");
    }

    #[test]
    fn dropped_receiver_makes_send_fail_gracefully() {
        let wake = Arc::new(Notify::new());
        let (bus, drain) = make_inbound::<u32, _>(wake, |_| vec![Effect::None]);
        drop(drain); // the drain owns the receiver — dropping it = subsystem stopped
        let result = bus.send(7);
        assert_eq!(
            result.err(),
            Some(7),
            "dropped receiver → send returns the item back"
        );
    }

    #[test]
    fn bus_is_clone_without_t_clone() {
        let wake = Arc::new(Notify::new());
        let (bus, mut drain) = make_inbound::<NotClone, _>(wake, |v| {
            let _ = v.0;
            vec![Effect::None]
        });
        let bus2 = bus.clone();
        bus.send(NotClone(1)).unwrap();
        bus2.send(NotClone(2)).unwrap();
        assert_eq!(drain().len(), 2, "both clones feed the same drain");
    }
}
