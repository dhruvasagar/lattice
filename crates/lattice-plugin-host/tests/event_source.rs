//! PH7.8c — the event/hook seam, driven through a real guest.
//!
//! Instantiates the `events-guest` fixture (a `wasm32-wasip2` `events-plugin`
//! component) via [`PluginHost::spawn_event_plugin`], wires its subscriptions to
//! a native [`EventBus`], publishes events, and drives the [`EventActor`] to
//! completion — proving the whole seam end to end:
//!   - `register-events` crosses (the guest's imported `events.subscribe` calls →
//!     3 recorded subscriptions → 3 bus `SubscriptionTarget::Plugin` entries),
//!   - a published event routes bus → sink → actor channel → guest `on-event`,
//!     which records it to its writable data-dir mount (`/data/received.log`),
//!   - a non-subscribed event is never delivered (the filter gates it),
//!   - a **poison** handler that traps degrades gracefully (§8): the host logs +
//!     skips *that* delivery, the `Store` survives, and later deliveries to
//!     other handlers still land.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";

/// The fixture events component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// The host path the guest's `/data/received.log` maps to for a given data base.
fn received_log(data_base: &std::path::Path) -> PathBuf {
    data_base.join(PLUGIN_ID).join("data").join("received.log")
}

fn saved(path: &str) -> Event {
    Event::DocumentSaved {
        id: DocumentId::new(1),
        path: PathBuf::from(path),
    }
}

/// Read the guest's received-log as newline-trimmed entries (empty if absent —
/// the guest only creates it on the first delivery).
fn recorded(data_base: &std::path::Path) -> Vec<String> {
    match std::fs::read_to_string(received_log(data_base)) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_receives_subscribed_events_end_to_end() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host =
        PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile events fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = EventBus::new();

    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
        )
        .await
        .expect("spawn events plugin");
    // register-events subscribed 4 handlers (saved / quit / modal / no-op).
    assert_eq!(sub_ids.len(), 4, "guest subscribed four handlers");

    // Publish two subscribed events + one NON-subscribed (DocumentOpened has no
    // handler) → only the two subscribed deliveries queue on the actor channel.
    bus.publish(saved("src/lib.rs")); // handler 1
    bus.publish(Event::BeforeQuit); // handler 2
    bus.publish(Event::DocumentOpened {
        id: DocumentId::new(2),
        path: None,
        version: 0,
        text: String::new(),
    }); // no subscriber → not delivered

    // Close the channel (drop every sink) so the actor drains the queued
    // deliveries and then returns, and drive it to completion.
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;

    let got = recorded(&data_base);
    assert_eq!(
        got,
        vec!["1:saved".to_string(), "2:quit".to_string()],
        "only the two subscribed events were delivered, in publish order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poison_handler_traps_gracefully_without_affecting_others() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host =
        PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile events fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = EventBus::new();

    // A NATIVE bus subscriber to the same kind the poison handler observes — the
    // isolation witness: it must still receive the event even though the plugin
    // traps on it.
    let (native_tx, mut native_rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::ModalModeChanged),
        SubscriptionTarget::Channel(native_tx),
    );

    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
        )
        .await
        .expect("spawn events plugin");

    // A good delivery (handler 1) lands, then the POISON event (handler 3 →
    // `unreachable!` → a wasm trap). The trap must be caught + skipped without
    // crashing the host, and must not affect the native co-subscriber.
    bus.publish(saved("before/poison.rs")); // handler 1 → "1:saved"
    let modal = Event::ModalModeChanged {
        from: "Normal".into(),
        to: "Insert".into(),
    };
    bus.publish(modal); // handler 3 (poison) + the native subscriber

    // Isolation: the native subscriber received the event synchronously at
    // publish time, wholly independent of the plugin trapping on it.
    assert!(
        matches!(native_rx.try_recv(), Ok(Event::ModalModeChanged { .. })),
        "the native co-subscriber gets the event despite the plugin trapping on it"
    );

    for id in sub_ids {
        bus.unsubscribe(id);
    }
    // `run` returning at all is the proof the host did not panic on the guest
    // trap — it caught it, logged, and continued.
    actor.run().await;

    // The pre-trap delivery landed; the poison delivery wrote nothing (the host
    // skipped it gracefully). The plugin's own later deliveries would also fail
    // (a trap taints the instance) — that degradation is isolated to the plugin.
    let got = recorded(&data_base);
    assert_eq!(
        got,
        vec!["1:saved".to_string()],
        "the pre-trap delivery landed; the poison delivery degraded gracefully to a skip"
    );
}
