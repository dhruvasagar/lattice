//! PH7.8c — the per-plugin event-delivery actor + bus wiring.
//!
//! Design fragment: `docs/dev/architecture/plugin-host.md` §5 `events` + §3
//! (Store-per-plugin, task-per-Store). Slice plan: PH7.8c.
//!
//! ## The shape
//!
//! Event delivery is **host→guest, fire-and-forget** — the opposite direction of
//! the picker/completion bridges (which are host-calls-guest-for-a-reply). The
//! host owns an mpsc; the native [`EventBus`] pushes each matched [`Event`] into
//! it via a `SubscriptionTarget::Plugin` sink (with the bus lock dropped, so a
//! slow handler never stalls the publisher or another subscriber, audit M1); the
//! [`EventActor`] drains that channel on the plugin's own task and drives the
//! guest `on-event` export. There is **no reply** — a hook observes, it does not
//! return a value (the bus is observation-only in v1, §5.10).
//!
//! So this actor is simpler than [`picker_task`](crate::picker_task): no
//! `oneshot`, no `Client` with call methods. Its input channel IS the sink the
//! bus pushes into; the loop projects each event to WIT and calls `on-event`.
//! Async delivery keeps plugin event work **off the keystroke path** entirely
//! (paramount #4), bounded per-delivery by [`PluginBudget::event`].
//!
//! ## Runtime ownership + lifecycle
//!
//! The lib owns no runtime: [`PluginHost::spawn_event_plugin`] returns the
//! `(Vec<SubscriptionId>, EventActor)` pair; the **caller** drives
//! [`EventActor::run`] on its multi-thread runtime and holds the subscription ids
//! to `EventBus::unsubscribe` on teardown. When every subscription is removed
//! (teardown / lazy prune of a closed sink), the sink `Arc`s drop, the channel
//! closes, and the actor loop ends — dropping the `Store` (the teardown seam;
//! full crash-quarantine is PH7.12).

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc;
use lattice_protocol::Event as NativeEvent;
use lattice_runtime::{EventBus, PluginEventSink, SubscriptionId, SubscriptionTarget};
use wasmtime::Store;

use crate::WitBoundary;
use crate::boundary_event::project_event_filter;
use crate::events_host::bindings::EventsPlugin;
use crate::{
    Component, EventEmitCtx, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest,
    PluginState, TrustTier, arm_store, classify_trap,
};

/// One event routed from the bus to the actor. The bus sink tags each delivery
/// with the subscription's guest-chosen `handler` id so the actor can dispatch
/// to the right `on-event` handler (a plugin may route many `:autocmd`s to
/// distinct handlers behind one export).
struct PluginEventDelivery {
    handler: u32,
    event: NativeEvent,
}

/// The per-plugin actor: owns the `Store` + events bindings for the plugin's
/// life and drives `on-event` for each delivery the bus pushes onto its channel.
/// Construct via [`PluginHost::spawn_event_plugin`]; drive by spawning
/// [`run`](Self::run) on a multi-thread runtime.
pub struct EventActor {
    store: Store<PluginState>,
    bindings: EventsPlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<PluginEventDelivery>,
    id: PluginId,
    /// Crash-quarantine (PH7.12): the first `on-event` trap trips this, firing
    /// one `PluginCrashed` and short-circuiting every later delivery before it
    /// re-enters the dead `Store`.
    quarantine: crate::Quarantine,
    /// PO.2: the boundary tracer, wired by the loader via with_tracer; None in tests / pre-wire.
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl EventActor {
    /// The host-issued identity of this plugin.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// PO.2: attach the boundary tracer (the loader calls this before spawning
    /// run()). Off the hot path — the seam is async.
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    /// Drive the actor to completion. Delivers each event in arrival order; the
    /// loop ends when the channel closes (every subscription removed / pruned),
    /// dropping the `Store`. A delivery that traps does **not** end the loop and
    /// does **not** crash the host — the trap is caught, logged, and the loop
    /// continues (§8). Note a component trap *taints its instance*: this plugin's
    /// **subsequent** deliveries then also fail (each logged + skipped), so a
    /// trapping plugin is effectively dead until it is re-instantiated
    /// (quarantine / reload is PH7.12). The guarantee held here is **isolation**:
    /// the publisher, the bus, every other subscriber, and every other plugin are
    /// untouched — only the trapping plugin degrades.
    pub async fn run(mut self) {
        while let Some(delivery) = self.rx.next().await {
            self.deliver(delivery).await;
        }
    }

    /// Deliver one event to the guest `on-event(handler, ev)` export. Every
    /// failure mode is graceful (the four-artefact clause): a projection error
    /// (non-UTF-8 path), an arm failure, or a guest trap (fuel/epoch/wasm) skips
    /// *this* delivery with a `warn!`, never a panic — the plugin remains
    /// subscribed, the publisher and every other subscriber proceed.
    async fn deliver(&mut self, delivery: PluginEventDelivery) {
        let PluginEventDelivery { handler, event } = delivery;
        // Quarantine short-circuit (PH7.12): once this instance has trapped, its
        // `Store` is dead — skip the delivery silently (the `PluginCrashed` event
        // already fired at trip time; re-logging every subsequent delivery is
        // the noise this replaces).
        if self.quarantine.is_tripped() {
            return;
        }
        let wit = match event.to_wit() {
            Ok(w) => w,
            Err(error) => {
                tracing::warn!(
                    plugin = self.id.0,
                    handler,
                    %error,
                    "event dropped: WIT projection failed"
                );
                return;
            }
        };
        if let Err(error) = arm_store(&mut self.store, self.budget) {
            tracing::warn!(plugin = self.id.0, handler, %error, "event delivery skipped: arm failed");
            return;
        }
        let __trace_start = std::time::Instant::now();
        let call_result = self
            .bindings
            .call_on_event(&mut self.store, handler, &wit)
            .await;
        match call_result {
            Ok(()) => {
                // PO.2: record the successful guest-export crossing at Debug
                // (dropped by the default Info gate — no per-delivery noise
                // unless this plugin is raised to debug/trace). Off the hot
                // path: the event seam is async, emission a cheap gated push.
                if let Some(tracer) = self.tracer.as_ref() {
                    use crate::trace::{Direction, PluginTraceRecord, TraceLevel, TraceOutcome};
                    tracer.trace(PluginTraceRecord {
                        plugin: self.id.0,
                        seam: crate::PluginSeam::Events,
                        direction: Direction::GuestExport,
                        call: std::borrow::Cow::Borrowed("on-event"),
                        level: TraceLevel::Debug,
                        outcome: TraceOutcome::Ok {
                            micros: __trace_start.elapsed().as_micros() as u64,
                            fuel_delta: 0,
                        },
                        detail: None,
                    });
                }
            }
            Err(source) => {
                // Trap (fuel/epoch/wasm) or guest panic: skip this delivery,
                // never propagate — the host, bus, and every other subscriber
                // are untouched (§8 isolation). The trap taints the instance
                // irrecoverably, so trip quarantine: `PluginCrashed` fires once
                // and every later delivery short-circuits above rather than
                // re-failing. Re-instantiation is PH7.12b.
                let kind = classify_trap(&source);
                tracing::warn!(
                    plugin = self.id.0,
                    handler,
                    ?kind,
                    "plugin event handler trapped; delivery skipped"
                );
                // PO.2: record the trapped crossing at Error (always kept),
                // mirroring `trip_and_map_traced`'s Trap outcome shape.
                if let Some(tracer) = self.tracer.as_ref() {
                    use crate::trace::{Direction, PluginTraceRecord, TraceLevel, TraceOutcome};
                    tracer.trace(PluginTraceRecord {
                        plugin: self.id.0,
                        seam: crate::PluginSeam::Events,
                        direction: Direction::GuestExport,
                        call: std::borrow::Cow::Borrowed("on-event"),
                        level: TraceLevel::Error,
                        outcome: TraceOutcome::Trap {
                            kind: kind.label().to_string(),
                            func: "on-event".to_string(),
                        },
                        detail: None,
                    });
                }
                self.quarantine.trip("on-event", kind);
            }
        }
    }
}

impl PluginHost {
    /// Instantiate an `events-plugin` component under its capability grant, run
    /// its `register-events` export to collect subscriptions, wire each to `bus`,
    /// and return the `(subscription ids, actor)` pair. The subscriptions are
    /// live the moment this returns — but deliveries only *fire* once the caller
    /// drives [`EventActor::run`] (until then they queue on the channel). Grant /
    /// data-dir / WASI are identical to
    /// [`instantiate_plugin`](Self::instantiate_plugin) (shared `build_plugin_wasi`
    /// + `new_store`); the actor is *not* spawned here (the lib owns no runtime).
    ///
    /// The caller holds the returned [`SubscriptionId`]s to `EventBus::unsubscribe`
    /// on teardown; doing so drops the sinks, closes the channel, and ends the
    /// actor loop.
    pub async fn spawn_event_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
    ) -> Result<(Vec<SubscriptionId>, EventActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "event plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings = EventsPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();

        // Wire the emit context BEFORE `register-events` runs: the guest may call
        // the imported `register-event` / `emit-event` host-services from inside
        // `register-events` (or later, from `on-event`), and both need this
        // plugin's identity + the bus (PH7.8b.2). `store` moves into the actor
        // below, carrying the context for the life of the plugin.
        store.data_mut().event_emit = Some(EventEmitCtx {
            plugin_id: id,
            bus: Arc::clone(bus),
        });

        // Drive subscription registration: the guest calls the imported
        // `events.subscribe(filter, handler)` inside `register-events`, recording
        // each into the Store's `event_subscriptions`.
        arm_store(&mut store, budget)?;
        bindings
            .call_register_events(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-events",
                kind: classify_trap(&source),
                source: source.into(),
            })?;
        let recorded = store.data_mut().event_subscriptions.take();

        // Wire each recorded subscription to the bus. The actor drains one
        // channel; each subscription's sink tags deliveries with its handler so
        // the actor dispatches to the right guest handler.
        let (tx, rx) = mpsc::unbounded();
        let mut subscription_ids = Vec::with_capacity(recorded.len());
        for sub in recorded {
            let filter = match project_event_filter(sub.filter) {
                Ok(f) => f,
                Err(error) => {
                    tracing::warn!(
                        plugin = id.0,
                        handler = sub.handler,
                        %error,
                        "event subscription skipped: filter projection failed"
                    );
                    continue;
                }
            };
            let handler = sub.handler;
            let tx = tx.clone();
            let sink: PluginEventSink = Arc::new(move |ev: NativeEvent| {
                // `false` when the actor's receiver has closed (the plugin was
                // torn down) → the bus prunes this subscription lazily.
                tx.unbounded_send(PluginEventDelivery { handler, event: ev })
                    .is_ok()
            });
            let sid = bus.subscribe(
                filter,
                SubscriptionTarget::Plugin {
                    plugin: id.0,
                    handler,
                    sink,
                },
            );
            subscription_ids.push(sid);
        }
        // Drop the original `tx`: only the sink clones (held by the live bus
        // subscriptions) keep the channel open, so the actor ends exactly when
        // the last subscription is unsubscribed/pruned.
        drop(tx);

        let actor = EventActor {
            store,
            bindings,
            budget,
            rx,
            id,
            quarantine: crate::Quarantine::new(id, Arc::clone(bus)),
            tracer: None,
        };
        Ok((subscription_ids, actor))
    }
}
