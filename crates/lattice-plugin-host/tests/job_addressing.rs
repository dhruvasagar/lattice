//! LH.0.7 — a host job reports to the PLUGIN that started it, not to the seam
//! instance that did.
//!
//! A plugin with a grammar seam and an events seam is two instances with two
//! host-issued ids. An ex-command runs on the grammar instance; `on-event`
//! exists only on the events instance. LH.0.1–LH.0.3 addressed a job's events
//! to the *instance* id of whichever store started it, so a download started
//! from `:lsp-install` was addressed to an id no event actor has, and its
//! outcome went nowhere — a call that returned `ok(id)` and then answered
//! nothing. Every existing job test started and heard the job on one instance,
//! where the two ids are the same number, and so passed.
//!
//! These tests keep the ids apart on purpose (see `spread`), so a comparison
//! against the wrong one cannot pass by coincidence.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::manifest::Capability;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::Event;
use lattice_runtime::EventBus;
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn recorded(data_dir: &Path) -> Vec<String> {
    match std::fs::read_to_string(data_dir.join("received.log")) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

async fn wait_for(data_dir: &Path, line: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if recorded(data_dir).iter().any(|l| l == line) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Give two OTHER plugins their job-owner numbers first, so this plugin's is
/// not the same number as its first instance id. Without this both are `0`
/// and a filter on the wrong one is indistinguishable from the right one.
fn spread(host: &PluginHost) -> (u32, u32) {
    (host.job_owner("neighbour-a"), host.job_owner("neighbour-b"))
}

struct Harness {
    _dirs: TempDir,
    data_dir: PathBuf,
    host: PluginHost,
    bus: Arc<EventBus>,
    instance: u32,
    actor: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// Spawn the fixture's events instance, which subscribes to job output and
/// exit (handler 10) and runs `echo started`.
async fn start(wasm: &str) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("spawn-request"), "sh\n-c\necho started\n").unwrap();

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    spread(&host);
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new(
        PLUGIN_ID,
        vec![Capability::ProcSpawn],
        CapabilitySet::empty(),
    );
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
    let instance = actor.id().0;
    Harness {
        _dirs: dirs,
        data_dir,
        host,
        bus,
        instance,
        actor: tokio::spawn(actor.run()),
    }
}

fn output(owner: u32, line: &str) -> Event {
    Event::JobOutput {
        plugin: owner,
        id: 424_242,
        lines: vec![line.to_string()],
    }
}

#[test]
fn a_plugin_has_one_job_owner_and_shares_it_with_nobody() {
    let dirs = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let a = host.job_owner("lighthouse");
    let b = host.job_owner("project");
    assert_ne!(a, b);
    assert_eq!(host.job_owner("lighthouse"), a, "stable for a name");
    assert_eq!(host.job_owner("project"), b);
}

/// The fixture's own job still reports to it — now through the owner number,
/// which here is NOT its instance id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_reports_to_its_plugin_when_owner_and_instance_id_differ() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm).await;
    assert_ne!(
        h.host.job_owner(PLUGIN_ID),
        h.instance,
        "the harness must keep the two numbers apart or this test proves nothing"
    );

    assert!(
        wait_for(&h.data_dir, "10:out:started").await,
        "output reached the guest: {:?}",
        recorded(&h.data_dir)
    );
    assert!(wait_for(&h.data_dir, "10:exit:ok").await);
}

/// The case the fix is for: a job this instance did NOT start, addressed to
/// its plugin — as one started by the plugin's grammar instance is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_started_by_another_instance_of_the_plugin_is_delivered() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm).await;
    assert!(wait_for(&h.data_dir, "10:exit:ok").await);

    h.bus
        .publish(output(h.host.job_owner(PLUGIN_ID), "from-the-ex-command"));

    assert!(
        wait_for(&h.data_dir, "10:out:from-the-ex-command").await,
        "a job addressed to the plugin reaches its events instance, whoever \
         started it: {:?}",
        recorded(&h.data_dir)
    );
}

/// And the property addressing exists for: another plugin's job stays private
/// — including the one whose owner number happens to equal this instance's id,
/// which is exactly what the old comparison would have let through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_plugins_job_is_not_delivered() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm).await;
    assert!(wait_for(&h.data_dir, "10:exit:ok").await);
    let (neighbour_a, neighbour_b) = spread(&h.host);
    assert!(
        [neighbour_a, neighbour_b].contains(&h.instance),
        "one neighbour's owner number collides with this instance's id"
    );

    h.bus.publish(output(neighbour_a, "theirs-a"));
    h.bus.publish(output(neighbour_b, "theirs-b"));
    // Ordered after them on the same bus, so once this lands the two above
    // have been through the filter.
    h.bus
        .publish(output(h.host.job_owner(PLUGIN_ID), "sentinel"));

    assert!(wait_for(&h.data_dir, "10:out:sentinel").await);
    let log = recorded(&h.data_dir);
    assert!(
        !log.iter().any(|l| l.contains("theirs")),
        "another plugin's job output leaked: {log:?}"
    );
}
