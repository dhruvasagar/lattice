//! LH.0.3 end-to-end — a guest runs a program and hears what it printed and how
//! it exited, with nobody pressing a key; and the trust tier decides who may.
//!
//! The unit tests in `process_host.rs` cover the process itself. Two things
//! need a real guest and a real grant computation: that output and exit reach
//! the guest on its own actor with no action dispatched afterwards, and that
//! `proc:spawn` in a manifest is honoured for a bundled plugin and **withheld
//! from a user-installed one asking for exactly the same thing**. The second
//! is the trust boundary; it is decided in `capability::grant`, not in the
//! seam, so only a test that goes through both can see it hold.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![cfg(unix)]
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

fn recorded(data_dir: &Path) -> Vec<String> {
    match std::fs::read_to_string(data_dir.join("received.log")) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Poll the guest's log until a line matching `want` appears, or give up.
/// **Polling, and nothing else.**
async fn wait_for(data_dir: &Path, want: impl Fn(&str) -> bool) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Some(line) = recorded(data_dir).into_iter().find(|l| want(l)) {
            return Some(line);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

struct Harness {
    dirs: TempDir,
    data_dir: PathBuf,
    actor: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// Spawn the fixture, whose manifest asks for `proc:spawn`, at `tier`, asking
/// it to run `sh -c <script>`. `{dir}` in the script is a scratch directory.
async fn start(wasm: &str, tier: TrustTier, script: &str) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let script = script.replace("{dir}", dirs.path().to_str().unwrap());
    std::fs::write(
        data_dir.join("spawn-request"),
        format!("sh\n-c\n{script}\n"),
    )
    .unwrap();

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
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
            tier,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn events plugin");
    Harness {
        dirs,
        data_dir,
        actor: tokio::spawn(actor.run()),
    }
}

/// **The test that matters.** Output, then exit, reach the guest unprompted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bundled_plugin_runs_a_process_and_hears_its_output_and_exit() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, TrustTier::Bundled, "echo installing; echo done >&2").await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("10:exit:")).await;
    let log = recorded(&h.data_dir);
    assert_eq!(
        line.as_deref(),
        Some("10:exit:ok"),
        "the exit reached the guest with no action dispatched: {log:?}"
    );
    assert!(log.iter().any(|l| l == "spawn:started"), "{log:?}");
    assert!(log.iter().any(|l| l == "10:out:installing"), "{log:?}");
    assert!(
        log.iter().any(|l| l == "10:out:done"),
        "stderr too: {log:?}"
    );
    let exit = log.iter().position(|l| l == "10:exit:ok").unwrap();
    assert!(
        log.iter()
            .enumerate()
            .all(|(i, l)| !l.starts_with("10:out:") || i < exit),
        "all output is delivered before the exit: {log:?}"
    );
}

/// A failing install is an outcome the guest can show, not a crash.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_process_reaches_the_guest_with_its_status() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, TrustTier::Bundled, "echo no network; exit 7").await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("10:exit:"))
        .await
        .unwrap_or_else(|| panic!("no exit: {:?}", recorded(&h.data_dir)));
    assert!(line.contains("status 7"), "{line}");
    assert!(
        recorded(&h.data_dir)
            .iter()
            .any(|l| l == "10:out:no network"),
        "with the output that explains it"
    );
}

/// **The trust boundary.** The same component, the same manifest, the same
/// request — a user-installed plugin is refused, and nothing runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_installed_plugin_asking_for_proc_spawn_is_refused() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, TrustTier::UserInstalled, "touch {dir}/ran").await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("spawn:"))
        .await
        .expect("the guest recorded the call's result");
    assert!(line.starts_with("spawn:err("), "{line}");
    assert!(line.contains("proc:spawn"), "names the grant: {line}");
    // Long enough that a process, had one started, would have run.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!h.dirs.path().join("ran").exists(), "and nothing ran");
}
