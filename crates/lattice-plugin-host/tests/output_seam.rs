//! LH.0.5 end-to-end — a guest writes to an output buffer.
//!
//! The store's own tests (`output.rs`) cover what a write does to the ring and
//! which event it publishes; `plugin-output-mode`'s cover what a view does
//! with those events. What needs a real guest is the seam in front of both:
//! that the lines, the status and the reset cross the boundary intact, that
//! the buffer is owned by the plugin's NAME, and that a host with no store
//! refuses in words rather than dropping the lines.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::output::{
    OutputChange, OutputState, OutputStatus, PluginOutput, PluginOutputHandle, PluginOutputPushed,
};
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

async fn wait_for(data_dir: &Path, want: impl Fn(&str) -> bool) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Some(line) = recorded(data_dir).into_iter().find(|l| want(l)) {
            return Some(line);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

struct Harness {
    _dirs: TempDir,
    data_dir: PathBuf,
    output: PluginOutputHandle,
    pushed: Arc<Mutex<Vec<PluginOutputPushed>>>,
    actor: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// Spawn the fixture, asking it to write to the output buffer `name`. `wired`
/// decides whether the host was given a store; `prepare` runs on the store
/// before the guest does.
async fn start(
    wasm: &str,
    name: &str,
    wired: bool,
    prepare: impl FnOnce(&PluginOutput),
) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("output-request"), format!("{name}\n")).unwrap();

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let output: PluginOutputHandle = Arc::new(PluginOutput::new());
    let pushed = Arc::new(Mutex::new(Vec::new()));
    prepare(&output);
    let sink = pushed.clone();
    output.set_event_publisher(Box::new(move |ev| sink.lock().unwrap().push(ev)));
    if wired {
        host.set_plugin_output(output.clone());
        assert!(host.plugin_output_wired());
    }
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    // No capabilities: writing to your own output buffer needs none.
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());
    let (_subs, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::UserInstalled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn events plugin");
    Harness {
        _dirs: dirs,
        data_dir,
        output,
        pushed,
        actor: tokio::spawn(actor.run()),
    }
}

/// Lines, status and reset all cross, in the order the guest made them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_fills_its_output_buffer() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, "*lsp-install:rust-analyzer*", true, |_| {}).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("output:")).await;
    assert_eq!(line.as_deref(), Some("output:ok"));

    let snap = h.output.snapshot("*lsp-install:rust-analyzer*").unwrap();
    assert_eq!(
        snap.lines,
        vec!["resolving rust-analyzer", "downloading", "verifying"],
        "a string with a newline in it is two lines"
    );
    assert_eq!(
        snap.status,
        Some(OutputStatus {
            state: OutputState::Running,
            text: "downloading\u{2026} 43%".into(),
        })
    );
    assert_eq!(snap.epoch, 1, "the guest reset before it wrote");

    let pushed = h.pushed.lock().unwrap();
    let changes: Vec<&OutputChange> = pushed.iter().map(|p| &p.change).collect();
    assert!(
        matches!(
            changes.as_slice(),
            [
                OutputChange::Reset,
                OutputChange::Status(_),
                OutputChange::Append { first_seq: 0, .. }
            ]
        ),
        "one event per call, in call order: {changes:?}"
    );
    assert!(
        pushed
            .iter()
            .all(|p| &*p.name == "*lsp-install:rust-analyzer*")
    );
}

/// The owner is the plugin's manifest name, so another plugin's buffer is not
/// this one's to write — and the refusal reaches the guest as an `err`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_cannot_write_into_another_plugins_buffer() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, "*theirs*", true, |output| {
        output
            .append("someone-else", "*theirs*", vec!["untouched".into()])
            .unwrap();
    })
    .await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("output:"))
        .await
        .expect("the guest reported");
    assert!(
        line.contains("belongs to plugin 'someone-else'"),
        "names the owner: {line}"
    );
    let snap = h.output.snapshot("*theirs*").unwrap();
    assert_eq!(snap.lines, vec!["untouched"]);
    assert_eq!(snap.epoch, 0, "not reset either");
    assert!(h.pushed.lock().unwrap().is_empty(), "nothing was published");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_malformed_buffer_name_is_refused_by_name() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, "install-log", true, |_| {}).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("output:"))
        .await
        .expect("the guest reported");
    assert!(
        line.contains("'install-log' is not an output buffer name"),
        "{line}"
    );
    assert!(h.output.snapshot("install-log").is_none());
}

/// A host nothing wired a store into says so; it does not accept the lines and
/// lose them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unwired_host_refuses_instead_of_dropping_the_lines() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, "*log*", false, |_| {}).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("output:"))
        .await
        .expect("the guest reported");
    assert!(line.contains("no plugin-output store wired"), "{line}");
    assert!(h.output.snapshot("*log*").is_none());
}
