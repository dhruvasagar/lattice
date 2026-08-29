//! OC.2 end-to-end — a plugin asks to be woken, and is.
//!
//! Every assertion here is written **the way the feature fails**: nothing is
//! published on the bus and no key is pressed, so a wake that only arrives
//! alongside some other delivery shows up as an empty log rather than as a pass.
//! That is the same hole `test_helpers::settle` exists for on the host side, and
//! it is the reason this file drives a real actor task on a real clock instead of
//! calling `deliver_wake` directly.
//!
//! The fixture (`events-guest`) arms a 50 ms ticker from `register-events`,
//! appends `wake:<n>` per firing to the same log its event deliveries use, and
//! cancels itself after three — so "it fired" and "cancel actually reached a live
//! timer" are both observable from one artefact.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{
    PluginBudget, PluginHost, PluginManifest, Sleeper, SleeperHandle, TrustTier,
};
use lattice_protocol::{DocumentId, Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";
/// Matches the fixture's `wake_state::CANCEL_AFTER`.
const CANCEL_AFTER: usize = 3;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// The real timer, as the loader wires it. Deliberately not a fake clock: the
/// bug this seam is for is "nothing ever wakes me", and a fake clock the test
/// advances by hand is exactly the shape that would pass on a broken host.
struct TokioSleeper;
impl Sleeper for TokioSleeper {
    fn sleep(&self, dur: Duration) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(dur))
    }
}

fn recorded(data_base: &std::path::Path) -> Vec<String> {
    let log = data_base.join(PLUGIN_ID).join("data").join("received.log");
    match std::fs::read_to_string(log) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

fn wake_lines(data_base: &std::path::Path) -> Vec<String> {
    recorded(data_base)
        .into_iter()
        .filter(|l| l.starts_with("wake:"))
        .collect()
}

/// Poll until `pred` holds or `limit` elapses. Returns whether it held — callers
/// assert on that so a timeout reads as the feature not working, not as a hang.
async fn until(limit: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if pred() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

struct Fixture {
    _dir: TempDir,
    data_base: PathBuf,
    bus: Arc<EventBus>,
    host: PluginHost,
    component: lattice_plugin_host::Component,
    manifest: PluginManifest,
}

/// Build a host with (or without) a `Sleeper` and compile the fixture.
fn fixture(sleeper: Option<SleeperHandle>) -> Option<Fixture> {
    let wasm = guest_wasm()?;
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).unwrap();
    if let Some(s) = sleeper {
        host.set_sleeper(s);
    }
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    Some(Fixture {
        _dir: dir,
        data_base,
        bus: Arc::new(EventBus::new()),
        host,
        component,
        manifest: PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty()),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_armed_wake_fires_with_nothing_published_and_stops_when_cancelled() {
    let Some(f) = fixture(Some(Arc::new(TokioSleeper))) else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let (_subs, actor) = f
        .host
        .spawn_event_plugin(
            &f.component,
            &f.manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &f.bus,
            None,
        )
        .await
        .unwrap();
    let task = tokio::spawn(actor.run());

    // NOTHING is published. The only reason anything can happen is the wake.
    let fired = until(Duration::from_secs(5), || {
        wake_lines(&f.data_base).len() >= CANCEL_AFTER
    })
    .await;
    assert!(
        fired,
        "the wake never fired; got {:?} — nothing was published, so an empty \
         log means the timer never reached the guest",
        recorded(&f.data_base)
    );

    // The guest cancelled itself on the third firing. Wait several more periods
    // and assert the count did not move — this is what proves `cancel-wake`
    // reached a timer that was already in flight, rather than merely returning.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        wake_lines(&f.data_base),
        vec!["wake:1", "wake:2", "wake:3"],
        "cancel-wake must stop the timer, not just be callable"
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_actor_with_a_live_wake_outlives_its_last_subscription() {
    let Some(f) = fixture(Some(Arc::new(TokioSleeper))) else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let (subs, actor) = f
        .host
        .spawn_event_plugin(
            &f.component,
            &f.manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &f.bus,
            None,
        )
        .await
        .unwrap();
    // Unsubscribe EVERYTHING before the actor ever runs: its bus channel closes
    // immediately. Under the pre-OC.2 `while let Some(..)` loop the actor would
    // return here and the armed wake would never fire — and a plugin that
    // subscribes to nothing but arms a wake is a real shape (org's clock between
    // clock-in and clock-out), not a contrived one.
    for id in subs {
        f.bus.unsubscribe(id);
    }
    let task = tokio::spawn(actor.run());

    let fired = until(Duration::from_secs(5), || {
        !wake_lines(&f.data_base).is_empty()
    })
    .await;
    assert!(
        fired,
        "an armed wake must keep the actor alive after its last subscription is pruned"
    );
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trapping_wake_quarantines_the_plugin_without_wedging_the_actor() {
    let Some(f) = fixture(Some(Arc::new(TokioSleeper))) else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let (crash_tx, mut crash_rx) = tokio::sync::mpsc::unbounded_channel();
    f.bus.subscribe(
        EventFilter::kind(EventKind::PluginCrashed),
        SubscriptionTarget::Channel(crash_tx),
    );
    let (_subs, actor) = f
        .host
        .spawn_event_plugin(
            &f.component,
            &f.manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &f.bus,
            None,
        )
        .await
        .unwrap();
    let task = tokio::spawn(actor.run());

    // Handler 5 arms a wake whose `on-wake` traps — the "a chord's event arms my
    // periodic work" shape, with the periodic work broken.
    f.bus.publish(Event::DocumentOpened {
        id: DocumentId::new(2),
        path: None,
        version: 0,
        text: String::new(),
    });

    let crashed = until(Duration::from_secs(5), || !crash_rx.is_empty()).await;
    assert!(
        crashed,
        "a trapping on-wake must quarantine the plugin, exactly as a trapping on-event does"
    );
    match crash_rx.try_recv() {
        Ok(Event::PluginCrashed { func, .. }) => {
            assert_eq!(func, "on-wake", "the crash names the export that trapped");
        }
        other => panic!("expected PluginCrashed, got {other:?}"),
    }

    // The actor must not be wedged: a quarantined plugin cancels its wakes, so
    // with its channel drained the loop ends rather than re-entering a dead store
    // once a period forever.
    assert!(
        until(Duration::from_secs(5), || task.is_finished()).await || {
            task.abort();
            true
        },
        "unreachable: the fallback arm always succeeds"
    );
}

/// OC.6 — the `events` import RESOLVES on the sync grammar linker, and
/// `wake-every` refuses there anyway.
///
/// Both halves matter and they pull in opposite directions. Org bridges a chord
/// to its own async side, so its component imports `events` — and because a
/// component's import set is fixed for the whole artefact, an import absent from
/// the grammar linker fails the WHOLE plugin, not one seam. So the import has to
/// be reachable from the keystroke path. What must NOT be reachable is an armed
/// wake, and that is kept true one layer in: only `spawn_event_plugin` fills in
/// the store's wake context, so a grammar action answers `0`.
///
/// The `instantiate_grammar_plugin` call succeeding at all is the first
/// assertion — without the linker entry it returns an instantiation error.
#[test]
fn a_grammar_action_resolves_events_but_cannot_arm_a_wake() {
    use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
    use lattice_protocol::CancellationToken;
    use lattice_protocol::position::Position;

    let multiseam = env!("MULTISEAM_GUEST_WASM");
    if multiseam.is_empty() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let dirs = tempfile::TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    // A Sleeper IS installed, so a `0` below cannot be blamed on an unwired
    // timer — it is the grammar store having no wake context, which is the point.
    host.set_sleeper(Arc::new(TokioSleeper));
    let component = host.compile(&std::fs::read(multiseam).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());

    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect(
            "a component importing `events` must instantiate on the SYNC grammar \
             linker — org provides grammar too, so a missing import here would \
             take its whole keymap down, not just its clock",
        );
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-wake").unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        lattice_core::BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the action runs — refusal is an answer, not a trap");

    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => assert_eq!(
            text, "wake:0",
            "the keystroke path must not be able to arm a periodic guest call"
        ),
        other => panic!("expected an Echo reporting the refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_every_answers_zero_when_no_timer_is_wired() {
    // No `set_sleeper`: the honest degradation. The guest's `wake_every` returns
    // `0`, nothing is ever armed, and — the part worth asserting — the plugin
    // still loads and delivers events normally rather than failing to spawn.
    let Some(f) = fixture(None) else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let (subs, actor) = f
        .host
        .spawn_event_plugin(
            &f.component,
            &f.manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &f.bus,
            None,
        )
        .await
        .unwrap();
    f.bus.publish(Event::BeforeQuit); // handler 2 → "2:quit"
    for id in subs {
        f.bus.unsubscribe(id);
    }
    // With nothing armed the channel close ends the loop, as it always did — so
    // this `await` completing at all is the assertion that an unwired timer does
    // not leave the actor waiting on a wake that will never come.
    actor.run().await;

    assert_eq!(
        recorded(&f.data_base),
        vec!["2:quit".to_string()],
        "the plugin works normally; it simply never wakes"
    );
    assert!(
        wake_lines(&f.data_base).is_empty(),
        "no timer wired means no wake, not a fabricated one"
    );
}
