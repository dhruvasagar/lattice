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
use std::sync::Arc;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_plugin_sdk::PluginEvent;
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
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile events fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());

    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn events plugin");
    // register-events subscribed 5 handlers (saved / quit / modal / no-op, plus
    // OC.2's opened → arms the poison wake).
    assert_eq!(sub_ids.len(), 5, "guest subscribed five handlers");

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

/// PH7.8b.2 — the emit/subscribe wire, end to end through a real guest. The
/// fixture DECLARES a plugin event (`register-event`) at registration and EMITS
/// it (`emit-event`) from its save handler. This proves both host-services seams:
///   - `register-event` records into the host's runtime event registry under the
///     plugin's `plugin:<id>` provenance (surfacing in introspection), and
///   - `emit-event` publishes an opaque-payload `Event::Plugin` onto the bus that
///     a NATIVE subscriber receives verbatim — the host as a thin byte router.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_declares_and_emits_a_custom_event_end_to_end() {
    use lattice_protocol::event_registry::event_info_by_name;

    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile events fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());

    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn events plugin");

    // `register-events` already ran, so the plugin's `register-event` call landed
    // the custom event in the runtime registry, stamped with its provenance.
    let info = event_info_by_name("events-fixture.saved-echo")
        .expect("the plugin's custom event self-registered via register-event");
    assert_eq!(
        info.source,
        format!("plugin:{}", actor.id().0),
        "the event is attributed to the emitting plugin's provenance"
    );
    assert!(!info.builtin, "a plugin-defined event is not a built-in");

    // A native subscriber for the plugin-event kind — the emit witness. It stays
    // subscribed while the plugin's own subscriptions are torn down.
    let (plugin_tx, mut plugin_rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::Plugin),
        SubscriptionTarget::Channel(plugin_tx),
    );

    // Publish a save → handler 1 fires and EMITS the custom event from inside its
    // `on-event`. Queue the delivery, then close the actor channel and drain.
    bus.publish(saved("src/lib.rs"));
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;

    // The guest's emit crossed to the bus and reached the native subscriber. The
    // guest authored the payload with the PH7.8b.3 SDK derive; this test shares
    // the SAME `PluginEvent` type (the cross-plugin contract) and decodes it —
    // proving the typed round-trip guest→wire→host consumer.
    match plugin_rx.try_recv() {
        Ok(Event::Plugin { name, payload }) => {
            assert_eq!(name, SavedEcho::NAME, "the SDK-derived NAME crossed");
            let decoded = lattice_plugin_sdk::try_decode::<SavedEcho>(&name, &payload)
                .expect("name matches the contract type")
                .expect("payload is valid MessagePack for the shared type");
            assert_eq!(
                decoded.path, "src/lib.rs",
                "the typed field survives guest encode → wire → host decode"
            );
        }
        other => panic!("expected the emitted Plugin event, got {other:?}"),
    }
}

/// The cross-plugin event CONTRACT (PH7.8b.3): the exact `PluginEvent` type the
/// `events-guest` fixture emits, redeclared here so this host-side test decodes
/// the guest's payload the way a *coordinating plugin* would — sharing the type
/// via a common crate. `NAME` must match the fixture's `#[event(name = ...)]`.
#[derive(serde::Serialize, serde::Deserialize, lattice_plugin_sdk::PluginEvent)]
#[event(name = "events-fixture.saved-echo")]
struct SavedEcho {
    path: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poison_handler_traps_gracefully_without_affecting_others() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile events fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());

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
            None,
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

/// PH7.12 — crash-quarantine. The first `on-event` trap trips quarantine: it
/// fires **exactly one** `Event::PluginCrashed` on the bus, and every later
/// delivery — even to a *non-trapping* handler — short-circuits without
/// re-entering the dead `Store`. This is the formalisation of the "tainted
/// instance" note in `poison_handler_*`: today's repeated-fail-and-log becomes a
/// one-shot crash signal plus a silent no-op. Isolation is asserted by a native
/// co-subscriber that keeps receiving the event the plugin dies on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_trap_quarantines_and_emits_one_plugin_crashed() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile events fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());

    // The crash witness: a native subscriber on the host-internal
    // `PluginCrashed` kind. Counting these is how we prove "exactly once".
    let (crash_tx, mut crash_rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::PluginCrashed),
        SubscriptionTarget::Channel(crash_tx),
    );

    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn events plugin");
    let pid = actor.id().0;

    // Pre-crash good delivery (handler 1) lands. Then the poison event (handler 3
    // → wasm trap) crashes the instance. Two MORE deliveries follow — a second
    // poison and a *good* save — both must be short-circuited by quarantine (the
    // instance is dead), so neither re-traps nor writes.
    bus.publish(saved("before/crash.rs")); // handler 1 → "1:saved"
    let poison = || Event::ModalModeChanged {
        from: "Normal".into(),
        to: "Insert".into(),
    };
    bus.publish(poison()); // handler 3 → trap → quarantine trips, ONE PluginCrashed
    bus.publish(poison()); // handler 3 → short-circuited (already quarantined)
    bus.publish(saved("after/crash.rs")); // handler 1 → short-circuited (dead instance)

    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;

    // Only the pre-crash delivery wrote: the post-crash good save was skipped
    // because the whole instance is quarantined, not just the trapping handler.
    let got = recorded(&data_base);
    assert_eq!(
        got,
        vec!["1:saved".to_string()],
        "only the pre-crash delivery landed; every post-crash delivery short-circuited"
    );

    // Exactly one PluginCrashed, carrying this plugin's id, the trapping export,
    // and the `trap` kind (a guest `unreachable!`). The second poison did NOT
    // fire a second crash — the guarantee is one-shot.
    let first = crash_rx.try_recv().expect("a PluginCrashed fired");
    match first {
        Event::PluginCrashed {
            plugin,
            ref func,
            ref kind,
        } => {
            assert_eq!(plugin, pid, "crash carries the host-issued plugin id");
            assert_eq!(func, "on-event", "the trapping export is on-event");
            assert_eq!(kind, "trap", "a guest unreachable is a `trap`-kind crash");
        }
        other => panic!("expected PluginCrashed, got {other:?}"),
    }
    assert!(
        crash_rx.try_recv().is_err(),
        "quarantine is one-shot: the second trap fired no second PluginCrashed"
    );
}
