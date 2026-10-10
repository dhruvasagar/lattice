//! LH.0.2 end-to-end — a guest asks for an archive to be unpacked, is told when
//! it is done, and makes the result executable; with nobody pressing a key.
//!
//! The unit tests in `extract_host.rs` cover what an archive may and may not
//! do. This covers the two things they cannot: that an unpack reports through
//! the SAME job events a download does (the reason those events are generic),
//! and that the outcome reaches the guest on its own actor with no action
//! dispatched afterwards. As in `download_seam.rs`, the test does nothing after
//! spawning but wait for the guest's log to grow.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::manifest::Capability;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_runtime::EventBus;
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";
const BINARY: &[u8] = b"#!/bin/sh\necho a language server\n";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
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
    _dirs: TempDir,
    data_dir: PathBuf,
    dest: PathBuf,
    actor: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// Spawn the fixture asked to unpack `archive` as a bare `gz`. `granted`
/// decides whether the manifest carries `fs:write` over the managed directory.
async fn start(wasm: &str, archive: &[u8], granted: bool) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    let managed = dirs.path().join("managed");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&managed).unwrap();
    let src = managed.join("server.gz");
    let dest = managed.join("server");
    std::fs::write(&src, archive).unwrap();
    std::fs::write(
        data_dir.join("extract-request"),
        format!("{}\n{}\ngz\n", src.display(), dest.display()),
    )
    .unwrap();

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let requested = if granted {
        vec![Capability::FsWrite(managed)]
    } else {
        Vec::new()
    };
    let manifest = PluginManifest::new(PLUGIN_ID, requested, CapabilitySet::empty());
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
        dest,
        actor: tokio::spawn(actor.run()),
    }
}

/// **The install path's second half.** Unpacked, reported, made runnable —
/// each step the guest's own, none of them prompted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpack_reaches_the_guest_which_then_makes_it_executable() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, &gz(BINARY), true).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("set-executable:")).await;
    let log = recorded(&h.data_dir);
    assert_eq!(
        line.as_deref(),
        Some("set-executable:ok"),
        "the guest heard the outcome and acted on it, unprompted: {log:?}"
    );
    assert!(log.iter().any(|l| l == "extract:started"), "{log:?}");
    assert!(log.iter().any(|l| l == "9:extract-finished:ok"), "{log:?}");
    assert_eq!(std::fs::read(&h.dest).unwrap(), BINARY);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&h.dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "runnable");
    }
}

/// A corrupt archive is a reported failure, with nothing left to mistake for
/// an install.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_corrupt_archive_reaches_the_guest_as_a_failure() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut bytes = gz(&[b'x'; 8192]);
    bytes.truncate(bytes.len() / 2);
    let h = start(wasm, &bytes, true).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("9:extract-finished"))
        .await
        .unwrap_or_else(|| panic!("no outcome: {:?}", recorded(&h.data_dir)));
    assert!(line.starts_with("9:extract-finished:err("), "{line}");
    assert!(!h.dest.exists(), "nothing was installed");
    assert!(
        !recorded(&h.data_dir)
            .iter()
            .any(|l| l.starts_with("set-executable:")),
        "and the guest did not go on to the next step"
    );
}

/// No `fs:write` over the destination: refused at the call, by name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_without_the_grant_is_refused_at_the_call() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, &gz(BINARY), false).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("extract:"))
        .await
        .expect("the guest recorded the call's result");
    assert!(line.starts_with("extract:err("), "{line}");
    assert!(line.contains("denied"), "{line}");
    assert!(!h.dest.exists());
}
