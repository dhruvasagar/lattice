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

/// A pending `wake-every` sleep, resolving to the id that came due. Boxed
/// because the [`Sleeper`](crate::Sleeper) is a trait object — the crate owns no
/// runtime, so the concrete future type belongs to the caller, not here.
type BoxWake = futures::future::BoxFuture<'static, u32>;

/// What the actor's `select` produced this turn.
///
/// OC.2 gave the loop a second input. A wake does **not** ride the bus channel:
/// it is a future on the actor's own `FuturesUnordered`, so it needs no sender
/// and — the load-bearing part — cannot keep the bus channel open. A plugin's
/// actor still ends exactly when its last subscription is pruned *and* it has no
/// wake armed, which is the property the `drop(tx)` below exists to give.
enum Turn {
    Event(PluginEventDelivery),
    /// An armed `wake-every` came due. The id may since have been cancelled;
    /// [`EventActor::deliver_wake`] is what checks.
    Wake(u32),
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
    ///
    /// **OC.2 gave the loop a second mouth.** Besides bus deliveries it drains a
    /// set of pending `wake-every` sleeps, firing `on-wake(id)` for each and
    /// re-arming it. Both inputs land on this one task, so a wake is bounded by
    /// the same budget, tripped by the same quarantine and dropped by the same
    /// task abort as an event — none of which had to be re-implemented for it.
    ///
    /// The loop now ends when the channel is closed **and** nothing is armed. A
    /// plugin that subscribes to nothing but arms a wake is a legitimate shape
    /// (org's clock does exactly that between clock-in and clock-out), and under
    /// the old `while let Some(..)` its actor would have exited before the first
    /// tick.
    pub async fn run(mut self) {
        use futures::stream::{FusedStream, FuturesUnordered};

        let mut wakes: FuturesUnordered<futures::future::BoxFuture<'static, u32>> =
            FuturesUnordered::new();
        // Wakes armed from inside `register-events`, before this loop existed.
        self.arm_pending(&mut wakes);

        loop {
            if self.rx.is_terminated() && wakes.is_empty() {
                return;
            }
            // Borrow `rx` explicitly so the select's futures are temporaries of
            // this block — `deliver` below takes `&mut self`.
            let turn = {
                let rx = &mut self.rx;
                if wakes.is_empty() {
                    match rx.next().await {
                        Some(d) => Turn::Event(d),
                        None => continue, // re-check the exit condition above
                    }
                } else if rx.is_terminated() {
                    match wakes.next().await {
                        Some(id) => Turn::Wake(id),
                        None => continue,
                    }
                } else {
                    futures::select! {
                        d = rx.next() => match d {
                            Some(d) => Turn::Event(d),
                            None => continue,
                        },
                        id = wakes.next() => match id {
                            Some(id) => Turn::Wake(id),
                            None => continue,
                        },
                    }
                }
            };
            match turn {
                Turn::Event(delivery) => self.deliver(delivery).await,
                Turn::Wake(id) => self.deliver_wake(id, &mut wakes).await,
            }
            // A guest call may have armed more wakes (`on-event` arming one is
            // org's clock-in path exactly), so re-check after every turn rather
            // than only at the top.
            self.arm_pending(&mut wakes);
        }
    }

    /// Turn every newly-armed wake into a pending sleep. A no-op on a store with
    /// no wake context (no `Sleeper` installed), which is why the whole seam can
    /// be absent without the loop knowing.
    fn arm_pending(&mut self, wakes: &mut futures::stream::FuturesUnordered<BoxWake>) {
        let Some(ctx) = self.store.data_mut().wake.as_mut() else {
            return;
        };
        let armed = ctx.take_newly_armed();
        for id in armed {
            if let Some(period) = ctx.period(id) {
                wakes.push(ctx.sleep_for(id, period));
            }
        }
    }

    /// Fire one due wake at the guest, then re-arm it.
    ///
    /// Three ways this delivers nothing, each deliberate: the plugin is
    /// quarantined (its store is dead — cancel everything so the timer stops
    /// rather than re-entering a corpse once a minute forever); the id was
    /// cancelled while its sleep was in flight (this is how `cancel-wake`
    /// reaches an already-running timer); or arming the budget failed.
    ///
    /// Re-arming happens **after** delivery, so the interval is a gap between
    /// wakes rather than a fixed schedule a slow guest could fall behind and
    /// then be flooded to catch up on.
    async fn deliver_wake(
        &mut self,
        id: u32,
        wakes: &mut futures::stream::FuturesUnordered<BoxWake>,
    ) {
        if self.quarantine.is_tripped() {
            if let Some(ctx) = self.store.data_mut().wake.as_mut() {
                ctx.cancel_all();
            }
            return;
        }
        let still_armed = self
            .store
            .data()
            .wake
            .as_ref()
            .is_some_and(|c| c.period(id).is_some());
        if !still_armed {
            // Cancelled mid-flight. Drop it; do not re-arm.
            return;
        }
        if let Err(error) = arm_store(&mut self.store, self.budget) {
            tracing::warn!(plugin = self.id.0, wake = id, %error, "wake skipped: arm failed");
            return;
        }
        let __trace_start = std::time::Instant::now();
        let call_result = self.bindings.call_on_wake(&mut self.store, id).await;
        match call_result {
            Ok(()) => {
                if let Some(tracer) = self.tracer.as_ref() {
                    use crate::trace::{Direction, PluginTraceRecord, TraceLevel, TraceOutcome};
                    tracer.trace(PluginTraceRecord {
                        plugin: self.id.0,
                        seam: crate::PluginSeam::Events,
                        direction: Direction::GuestExport,
                        call: std::borrow::Cow::Borrowed("on-wake"),
                        level: TraceLevel::Debug,
                        outcome: TraceOutcome::Ok {
                            micros: __trace_start.elapsed().as_micros() as u64,
                            fuel_delta: 0,
                        },
                        detail: None,
                    });
                }
                // Still armed? (`on-wake` itself may have cancelled it — org's
                // clock-out does exactly that from a handler.)
                if let Some(ctx) = self.store.data().wake.as_ref()
                    && let Some(period) = ctx.period(id)
                {
                    wakes.push(ctx.sleep_for(id, period));
                }
            }
            Err(source) => {
                let kind = classify_trap(&source);
                tracing::warn!(
                    plugin = self.id.0,
                    wake = id,
                    ?kind,
                    "plugin wake handler trapped; wake cancelled"
                );
                if let Some(tracer) = self.tracer.as_ref() {
                    use crate::trace::{Direction, PluginTraceRecord, TraceLevel, TraceOutcome};
                    tracer.trace(PluginTraceRecord {
                        plugin: self.id.0,
                        seam: crate::PluginSeam::Events,
                        direction: Direction::GuestExport,
                        call: std::borrow::Cow::Borrowed("on-wake"),
                        level: TraceLevel::Error,
                        outcome: TraceOutcome::Trap {
                            kind: kind.label().to_string(),
                            func: "on-wake".to_string(),
                        },
                        detail: None,
                    });
                }
                self.quarantine.trip("on-wake", kind);
                // The store is dead; a re-armed wake would only re-enter it.
                if let Some(ctx) = self.store.data_mut().wake.as_mut() {
                    ctx.cancel_all();
                }
            }
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
        // OR.2: a watch batch is ADDRESSED, and this is where the address is
        // read. The bus is a broadcast, so without this line every plugin
        // subscribed to `files-changed` would learn which files changed under
        // every *other* plugin's watched directory — a capability leak, since
        // that plugin holds no `fs:read` grant over it. The id is dropped on
        // projection, so a guest is never told its own name.
        if let NativeEvent::FilesChanged { plugin, .. } = &event
            && *plugin != self.id.0
        {
            return;
        }
        let wit = match event.to_wit() {
            Ok(w) => w,
            Err(error) => {
                // `debug!`, not `warn!`: a wildcard-filter subscription (`kinds:
                // none`) matches host-internal events whose `to_wit` deliberately
                // returns Err (e.g. `Event::PluginCrashed`), so this fires once per
                // crash — a `warn!` would flood `*messages*` per the log-levels
                // rule. Not user-actionable: the event simply can't cross to a
                // guest. A subscriber that wanted it would filter by a real kind.
                tracing::debug!(
                    plugin = self.id.0,
                    handler,
                    %error,
                    "event not delivered: no WIT projection (host-internal or non-UTF-8)"
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
                // OC.2: the store is dead, so every armed wake would now be a
                // guest call into a corpse once per period, forever. Cancel them
                // here rather than letting `deliver_wake` discover it each time.
                if let Some(ctx) = self.store.data_mut().wake.as_mut() {
                    ctx.cancel_all();
                }
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
    ///
    /// `config` is the live option registry, and the events seam needs it for
    /// the same reason `context` and `transient` do — see the wiring site below.
    pub async fn spawn_event_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
        config: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<(Vec<SubscriptionId>, EventActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "event plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
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
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2),
        // also before `register-events` — a guest may narrate from there.
        store.data_mut().log_ctx = self.log_ctx_for(id);
        // Deferred config is the whole reason `init.rs` subscribes to
        // `plugin-loaded`: a USER plugin's options do not EXIST until it loads,
        // so `config.set-option("org.capture-templates", …)` has to run from
        // `on-event`, and `docs/user/init.md` documents exactly that shape.
        // Without the registry on THIS store the call takes the
        // "plugin has no config registry wired" branch and warns into the log,
        // so the user's config silently does not apply and `:set …?` reports the
        // compiled default. `context` and `transient` wire this for the same
        // reason; a seam that runs in its own store needs it too.
        //
        // The gap survived because the CI.5 chain test drives the OTHER half of
        // the documented pattern — `modes.enable-mode`, which reaches the bus
        // rather than the registry — so the seam looked covered end to end while
        // its config path had never been called once.
        if let Some(registry) = config {
            store.data_mut().config_registry = Some(Arc::clone(registry));
        }

        // OC.2: the wake context, wired BEFORE `register-events` for the same
        // reason `event_emit` is — a guest may arm its first wake from there.
        // `None` when no `Sleeper` was installed, which leaves `wake-every`
        // answering `0` rather than pretending.
        store.data_mut().wake = self
            .sleeper
            .get()
            .map(|s| crate::wake::WakeCtx::new(Arc::clone(s)));

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
