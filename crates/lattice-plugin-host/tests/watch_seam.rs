//! OR.2 end-to-end — a guest is told a file changed, with nobody pressing a key.
//!
//! The unit tests in `watch_host.rs` cover coalescing, the grant and teardown.
//! What they cannot cover is the claim the slice exists for: that a change on
//! disk reaches the **guest** on its own, through the plugin's event actor,
//! without any action being dispatched afterwards.
//!
//! That distinction is the whole point. A test that publishes an event, or
//! presses a key, or drives the actor to completion after arranging everything,
//! passes just as happily against a seam that only ever delivers when something
//! else happens to run — which is the "it works, but only after I hit
//! something" bug class, and it reads as a rendering fault rather than as a
//! missing wake. So these tests spawn the actor, touch a file, and then do
//! **nothing** but wait for the guest's own log to grow.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::manifest::Capability;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_runtime::EventBus;
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn data_dir(data_base: &Path) -> PathBuf {
    data_base.join(PLUGIN_ID).join("data")
}

fn recorded(data_base: &Path) -> Vec<String> {
    match std::fs::read_to_string(data_dir(data_base).join("received.log")) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Poll the guest's log until a line matching `want` appears, or give up.
///
/// **Polling, and nothing else.** No event is published, no action dispatched,
/// no actor driven to completion — the only thing this does between checks is
/// sleep. If the line appears, it appeared because the watcher delivered it on
/// the plugin's own task.
async fn wait_for(
    data_base: &Path,
    want: impl Fn(&str) -> bool,
    budget: Duration,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if let Some(line) = recorded(data_base).into_iter().find(|l| want(l)) {
            return Some(line);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

/// Everything a watch test needs: a host, a spawned+running actor, the data base
/// the guest's log lives under, and the directory it is watching.
struct Harness {
    _dirs: TempDir,
    data_base: PathBuf,
    watched: PathBuf,
    actor: tokio::task::JoinHandle<()>,
    bus: Arc<EventBus>,
}

impl Harness {
    /// `granted` decides whether the manifest carries `fs:read` over the watched
    /// directory — the only difference between the working case and the refused
    /// one.
    async fn start(wasm: &str, granted: bool) -> Self {
        let dirs = TempDir::new().unwrap();
        let data_base = dirs.path().join("data");
        let watched = dirs.path().join("corpus");
        std::fs::create_dir_all(&watched).unwrap();

        // The guest reads its watch target out of its own data dir, because a
        // component cannot know where `/data` lives on the host — and because a
        // real plugin learns its corpus root from configuration anyway.
        std::fs::create_dir_all(data_dir(&data_base)).unwrap();
        std::fs::write(
            data_dir(&data_base).join("watch-target"),
            watched.to_str().unwrap(),
        )
        .unwrap();

        let host =
            PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
        let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
        let requested = if granted {
            vec![Capability::FsRead(watched.clone())]
        } else {
            Vec::new()
        };
        let manifest = PluginManifest::new(PLUGIN_ID, requested, CapabilitySet::empty());
        let bus = Arc::new(EventBus::new());

        let (_subs, actor) = host
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
        // The subscriptions stay live (not unsubscribed), so the actor keeps
        // running — which is what a real editor does and what makes "no
        // keypress" meaningful.
        let actor = tokio::spawn(actor.run());

        Self {
            _dirs: dirs,
            data_base,
            watched,
            actor,
            bus,
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// **The test that matters.** A file lands on disk and the guest hears about it
/// with nothing else running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_reaches_the_guest_without_a_keypress() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = Harness::start(wasm, true).await;

    assert!(
        wait_for(&h.data_base, |l| l == "watch:ok", Duration::from_secs(5))
            .await
            .is_some(),
        "the guest armed its watch: {:?}",
        recorded(&h.data_base)
    );

    std::fs::write(h.watched.join("note.org"), "* a new note\n").unwrap();

    let line = wait_for(
        &h.data_base,
        |l| l.starts_with("6:files-changed:") && l.contains("note.org"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        line.is_some(),
        "the change reached the guest with no action dispatched: {:?}",
        recorded(&h.data_base)
    );
}

/// The `git pull` case, end to end: 200 files must not be 200 guest calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_reaches_the_guest_as_few_batches_carrying_many_paths() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = Harness::start(wasm, true).await;
    wait_for(&h.data_base, |l| l == "watch:ok", Duration::from_secs(5))
        .await
        .expect("the guest armed its watch");

    for i in 0..200 {
        std::fs::write(h.watched.join(format!("n{i}.org")), "x").unwrap();
    }

    // Settle on the BATCH count, not on the log length.
    //
    // The log already holds two lines before the writes — `watch:ok` and the
    // ungranted-watch refusal, both written at registration — so a settle loop
    // watching the total length can see it "stop changing" at 2 and conclude it
    // is done before a single batch has arrived. That is a stop condition
    // satisfied by the thing it was not waiting for, and it makes the test pass
    // or fail on whether the first batch beat the second poll.
    let count = |base: &Path| -> usize {
        recorded(base)
            .iter()
            .filter(|l| l.starts_with("6:files-changed:"))
            .count()
    };
    // First, wait for at least one batch — however long the machine takes.
    assert!(
        wait_for(
            &h.data_base,
            |l| l.starts_with("6:files-changed:"),
            Duration::from_secs(30),
        )
        .await
        .is_some(),
        "at least one batch reached the guest: {:?}",
        recorded(&h.data_base)
    );
    // Then wait for the batches to stop arriving.
    let mut previous = 0usize;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = count(&h.data_base);
        if now == previous {
            break;
        }
        previous = now;
    }

    let batches: Vec<String> = recorded(&h.data_base)
        .into_iter()
        .filter(|l| l.starts_with("6:files-changed:"))
        .collect();
    assert!(
        batches.len() < 20,
        "200 writes became {} guest calls, not 200: {batches:?}",
        batches.len()
    );
    // …and the batches genuinely carry many paths rather than being many
    // one-path deliveries that merely happened to be few.
    let widest: usize = batches
        .iter()
        .filter_map(|l| l.split(':').nth(2))
        .filter_map(|n| n.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    assert!(
        widest > 10,
        "the widest batch carried {widest} paths — coalescing collapsed count, not breadth"
    );
}

/// `unwatch` reaches a live watcher. The guest disarms itself when it sees
/// `stop.org`; nothing after that is delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unwatch_stops_delivery() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = Harness::start(wasm, true).await;
    wait_for(&h.data_base, |l| l == "watch:ok", Duration::from_secs(5))
        .await
        .expect("the guest armed its watch");

    std::fs::write(h.watched.join("stop.org"), "x").unwrap();
    assert!(
        wait_for(&h.data_base, |l| l == "unwatch:ok", Duration::from_secs(10))
            .await
            .is_some(),
        "the guest disarmed its watch: {:?}",
        recorded(&h.data_base)
    );

    let before = recorded(&h.data_base).len();
    std::fs::write(h.watched.join("after.org"), "x").unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after: Vec<String> = recorded(&h.data_base);
    assert!(
        !after.iter().any(|l| l.contains("after.org")),
        "nothing is delivered after `unwatch`: {after:?}"
    );
    assert_eq!(
        after.len(),
        before,
        "and no batch at all arrived: {after:?}"
    );
}

/// A plugin with no `fs:read` grant cannot watch, is told so by name, and keeps
/// working — the honest degradation, not a failed load.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ungranted_watch_is_refused_and_the_plugin_keeps_working() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = Harness::start(wasm, false).await;

    let line = wait_for(
        &h.data_base,
        |l| l.starts_with("watch:"),
        Duration::from_secs(5),
    )
    .await
    .expect("the guest recorded what its watch attempt answered");
    assert!(
        line.starts_with("watch:err(") && line.contains("granted paths"),
        "the refusal names the grant as what to fix: {line}"
    );

    // The plugin is not dead. Publishing an event it subscribes to and seeing
    // it recorded is the proof: a refused capability must degrade, not take the
    // instance down. (This is the one place a test deliberately DOES make
    // something happen — liveness is what it is asserting, not delivery.)
    h.bus.publish(lattice_protocol::Event::BeforeQuit);
    assert!(
        wait_for(&h.data_base, |l| l == "2:quit", Duration::from_secs(5))
            .await
            .is_some(),
        "the plugin kept working after the refusal: {:?}",
        recorded(&h.data_base)
    );

    std::fs::write(h.watched.join("note.org"), "x").unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !recorded(&h.data_base)
            .iter()
            .any(|l| l.starts_with("6:files-changed:")),
        "and a refused watch really delivers nothing"
    );
}

/// The granted plugin's *second* watch — over `/`, which it was not granted —
/// is refused even though its first succeeded. A gate that armed on the first
/// call and stopped checking would pass every test above and fail this one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gate_is_per_call_not_per_plugin() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = Harness::start(wasm, true).await;

    let denied = wait_for(
        &h.data_base,
        |l| l.starts_with("denied:"),
        Duration::from_secs(5),
    )
    .await
    .expect("the guest recorded its second, ungranted watch attempt");
    assert!(
        denied.starts_with("denied:err(") && denied.contains("denied"),
        "a granted plugin is still refused a path outside its grant: {denied}"
    );
}
