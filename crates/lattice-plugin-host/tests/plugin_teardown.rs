//! PH7.12b.3 — the plugin reload cycle, end to end through a real guest.
//!
//! Proves the capstone claim of the teardown seam: an instance that **crashes**
//! (its `on-event` traps → quarantine fires one `PluginCrashed`) can be
//! **unloaded** (`PluginTeardown::unload` unsubscribes its bus subscriptions) and
//! **reloaded** (a fresh `spawn_event_plugin` mints a new `Store` with a fresh,
//! untripped `Quarantine`) — and the reloaded instance delivers normally while
//! the crashed instance's subscriptions are gone. Reload = unload + re-spawn,
//! composed here exactly as the Phase-8 plugin manager will.
//!
//! Skips when the events fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{CapabilitySet, ModeRegistry};
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{
    PluginBudget, PluginHost, PluginManifest, PluginTeardown, TeardownRegistries, TrustTier,
};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn received_log(data_base: &std::path::Path) -> PathBuf {
    data_base.join(PLUGIN_ID).join("data").join("received.log")
}

fn recorded(data_base: &std::path::Path) -> Vec<String> {
    match std::fs::read_to_string(received_log(data_base)) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

fn saved(path: &str) -> Event {
    Event::DocumentSaved {
        id: DocumentId::new(1),
        path: PathBuf::from(path),
    }
}

/// The event handler 3 of the fixture traps on (a guest `unreachable!`).
fn poison() -> Event {
    Event::ModalModeChanged {
        from: "Normal".into(),
        to: "Insert".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_then_unload_then_reload_delivers_on_a_fresh_instance() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };

    let bus = Arc::new(EventBus::new());
    // Crash witness: count PluginCrashed across the whole cycle (expect exactly 1).
    let (crash_tx, mut crash_rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::PluginCrashed),
        SubscriptionTarget::Channel(crash_tx),
    );

    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    // ---- Instance A: crashes, then is unloaded. ----
    let dir_a = TempDir::new().unwrap();
    let data_a = dir_a.path().join("data");
    let host_a = PluginHost::with_dirs(dir_a.path().join("cache"), &data_a).expect("host A builds");
    let component_a = host_a.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let (subs_a, actor_a) = host_a
        .spawn_event_plugin(
            &component_a,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn A");
    let sub_count_a = subs_a.len();
    assert!(sub_count_a > 0, "fixture subscribes to at least one event");

    // Bundle A's teardown tokens (events-only: just its subscriptions).
    let mut teardown_a = PluginTeardown::new(actor_a.id());
    teardown_a.subscriptions = subs_a;

    // A good delivery lands, then the poison delivery crashes the instance.
    // Both are queued to A's actor channel before we unsubscribe below.
    bus.publish(saved("before/crash.rs")); // handler 1 → "1:saved"
    bus.publish(poison()); // handler 3 → trap → quarantine, one PluginCrashed

    // ---- Unload A: unsubscribe its subscriptions from the bus. This drops A's
    // bus sinks, so once its actor drains the already-queued deliveries the
    // channel closes and `run()` returns (an un-unsubscribed actor would loop
    // forever). Unload is what a reload does first. ----
    let mut commands = CommandRegistry::new();
    let mut pickers = PickerRegistry::new();
    let mut modes = ModeRegistry::new();
    let keymap = KeymapHandle::new();
    let config = ConfigRegistry::new();
    let mut decorations = lattice_mode::GutterDecorationSourceRegistry::new();
    let mut contexts = lattice_mode::ContextSourceRegistry::new();
    let theme_reg = lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette());
    let modeline: lattice_mode::ModelineServiceHandle =
        Arc::new(lattice_mode::ModelineService::new());
    let parsers = lattice_compilation::CompilationParserFactories::new_handle();
    let report = {
        let mut reg = TeardownRegistries {
            provider_views: None,
            media: &mut Default::default(),
            agenda: &mut Default::default(),
            commands: &mut commands,
            pickers: &mut pickers,
            modes: &mut modes,
            keymap: &keymap,
            config: &config,
            bus: &bus,
            decorations: &mut decorations,
            contexts: &mut contexts,
            theme: &theme_reg,
            modeline: Some(&modeline),
            parsers: &parsers,
        };
        teardown_a.unload(&mut reg)
    };
    assert_eq!(
        report.subscriptions, sub_count_a,
        "unload removed exactly A's subscriptions from the bus"
    );

    // Drain A: the pre-crash good delivery writes, the poison delivery traps →
    // quarantine → one PluginCrashed; then the closed channel ends the loop.
    actor_a.run().await;
    assert_eq!(
        recorded(&data_a),
        vec!["1:saved".to_string()],
        "A delivered the pre-crash event, then crashed"
    );

    // ---- Instance B: the reload. A fresh spawn → fresh, untripped Quarantine. ----
    let dir_b = TempDir::new().unwrap();
    let data_b = dir_b.path().join("data");
    let host_b = PluginHost::with_dirs(dir_b.path().join("cache"), &data_b).expect("host B builds");
    let component_b = host_b.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let (subs_b, actor_b) = host_b
        .spawn_event_plugin(
            &component_b,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn B (reload)");

    // Publish a good event: A's subscriptions are gone (unloaded), so only B
    // receives — and B, being freshly instantiated, is untripped and delivers.
    bus.publish(saved("after/reload.rs")); // → B's handler 1 only
    for id in subs_b {
        bus.unsubscribe(id);
    }
    actor_b.run().await;

    assert_eq!(
        recorded(&data_b),
        vec!["1:saved".to_string()],
        "the reloaded instance delivered normally — a fresh, untripped Store"
    );

    // Exactly one crash across the whole cycle: A's. The reload did NOT crash,
    // and the unloaded A received nothing further.
    assert!(crash_rx.try_recv().is_ok(), "A's crash fired");
    assert!(
        crash_rx.try_recv().is_err(),
        "the reloaded instance did not crash — one PluginCrashed total"
    );
}
