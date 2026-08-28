//! OC.1 end-to-end — a **grammar** action reaches the event bus.
//!
//! `host-services` (and with it `emit-event`) has been linked into the sync
//! grammar linker since OM.11, so the seam resolved and the guest's call
//! returned normally. What was missing was the `EventEmitCtx` behind it: the
//! store's `event_emit` was populated in exactly one place — the events actor —
//! so every grammar-side `emit-event` took the `None` arm and was warned and
//! dropped. A seam that looks available and silently is not.
//!
//! That gap is invisible from the guest: a dropped emit and a delivered one are
//! the same `()` return. It is only observable with a **native subscriber**, and
//! its absence is exactly why the gap survived — so this is that test.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::Event;
use lattice_protocol::position::Position;
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

const SRC: &str = "fn m() { let x = 1; }\n";

#[test]
fn a_grammar_action_emits_onto_the_bus_and_a_native_subscriber_receives_it() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let path = guest_wasm().unwrap();
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());

    // The bus the host hands the grammar store. Subscribe BEFORE dispatch — the
    // bus is fire-and-forget, so a late subscriber cannot observe a past emit.
    let bus = Arc::new(EventBus::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(lattice_protocol::EventKind::Plugin),
        SubscriptionTarget::Channel(tx),
    );

    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-emit").unwrap();

    // Cursor at (0, 3) — the guest sends those two bytes as the payload, so the
    // assertion below proves the *guest's* bytes crossed rather than matching a
    // constant a stub could also produce.
    let cursor = Position { line: 0, byte: 3 };
    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        cursor,
        CommandInvocation::of(id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the emitting action dispatches through the sync trampoline");

    match rx.try_recv() {
        Ok(Event::Plugin { name, payload }) => {
            assert_eq!(name, "multiseam/pinged");
            assert_eq!(
                payload,
                vec![0u8, 3u8],
                "the guest's own bytes crossed verbatim"
            );
        }
        other => panic!(
            "expected the grammar action's Plugin event on the bus, got {other:?} \
             (a `None` EventEmitCtx warns and drops — that is the OC.1 gap)"
        ),
    }
}
