//! LH.0.1 end-to-end — a guest asks for a file and is told when it is there,
//! with nobody pressing a key.
//!
//! The unit tests in `download_host.rs` cover the transfer itself: the grant,
//! redirects, the digest, cancellation, what is left on disk. What they cannot
//! cover is the claim the seam's shape exists for — that the outcome reaches
//! the **guest**, through the plugin's own event actor, with no action
//! dispatched afterwards. A seam that only delivered when something else
//! happened to run would pass every one of them.
//!
//! So, as `watch_seam.rs` does: spawn the actor, and then do **nothing** but
//! wait for the guest's own log to grow. The guest starts its download from
//! inside `register-events`, which makes this the test of the registration
//! window too — the transfer is over loopback and would otherwise finish
//! before the guest's subscription is on the bus.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::manifest::Capability;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_runtime::EventBus;
use tempfile::TempDir;

const BODY: &[u8] = b"a language server, or near enough\n";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Serve `BODY` for every request, on loopback. Returns the port.
fn serve_body() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                BODY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(BODY);
        }
    });
    port
}

fn data_dir(data_base: &Path, plugin: &str) -> PathBuf {
    data_base.join(plugin).join("data")
}

fn recorded(data_base: &Path, plugin: &str) -> Vec<String> {
    match std::fs::read_to_string(data_dir(data_base, plugin).join("received.log")) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Poll the guest's log until a line matching `want` appears, or give up.
/// **Polling, and nothing else** — see the module doc.
async fn wait_for(
    data_base: &Path,
    plugin: &str,
    want: impl Fn(&str) -> bool,
    budget: Duration,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if let Some(line) = recorded(data_base, plugin).into_iter().find(|l| want(l)) {
            return Some(line);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

struct Harness {
    dirs: TempDir,
    data_base: PathBuf,
    host: PluginHost,
    bus: Arc<EventBus>,
    port: u16,
    actors: Vec<tokio::task::JoinHandle<()>>,
}

impl Harness {
    fn new() -> Self {
        let dirs = TempDir::new().unwrap();
        let data_base = dirs.path().join("data");
        let host =
            PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
        Self {
            dirs,
            data_base,
            host,
            bus: Arc::new(EventBus::new()),
            port: serve_body(),
            actors: Vec::new(),
        }
    }

    /// Where `plugin`'s download lands.
    fn dest(&self, plugin: &str) -> PathBuf {
        self.dirs.path().join("managed").join(plugin).join("server")
    }

    /// Spawn the events fixture as `plugin`, asking it to download `BODY`
    /// pinned to `sha256`, under the capabilities `requested`.
    async fn spawn(&mut self, wasm: &str, plugin: &str, sha256: &str, requested: Vec<Capability>) {
        let dest = self.dest(plugin);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::create_dir_all(data_dir(&self.data_base, plugin)).unwrap();
        std::fs::write(
            data_dir(&self.data_base, plugin).join("download-request"),
            format!(
                "http://127.0.0.1:{}/file\n{sha256}\n{}\n",
                self.port,
                dest.display()
            ),
        )
        .unwrap();

        let component = self.host.compile(&std::fs::read(wasm).unwrap()).unwrap();
        let manifest = PluginManifest::new(plugin, requested, CapabilitySet::empty());
        let (_subs, actor) = self
            .host
            .spawn_event_plugin(
                &component,
                &manifest,
                TrustTier::UserInstalled,
                PluginBudget::event(),
                &self.bus,
                None,
            )
            .await
            .expect("spawn events plugin");
        self.actors.push(tokio::spawn(actor.run()));
    }

    /// Both grants a download needs.
    fn granted(&self, plugin: &str) -> Vec<Capability> {
        vec![
            Capability::NetHttp("127.0.0.1".into()),
            Capability::FsWrite(self.dest(plugin).parent().unwrap().to_path_buf()),
        ]
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for actor in &self.actors {
            actor.abort();
        }
    }
}

/// The digest to hand the guest. The host hashes for itself; this only has to
/// be the right answer.
fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const A: &str = "events-fixture";
const B: &str = "events-fixture-b";

/// **The test that matters.** The file lands and the guest hears about it with
/// nothing else running — and it is a user-tier plugin, so the grant is
/// honoured identically there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_reaches_the_guest_without_a_keypress() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = Harness::new();
    let caps = h.granted(A);
    h.spawn(wasm, A, &sha(BODY), caps).await;

    let line = wait_for(
        &h.data_base,
        A,
        |l| l.starts_with("8:job-finished"),
        Duration::from_secs(15),
    )
    .await;
    let log = recorded(&h.data_base, A);
    assert_eq!(
        line.as_deref(),
        Some("8:job-finished:ok"),
        "the outcome reached the guest with no action dispatched: {log:?}"
    );
    assert!(
        log.iter().any(|l| l == "download:started"),
        "the call itself returned an id at once: {log:?}"
    );
    assert_eq!(std::fs::read(h.dest(A)).unwrap(), BODY);
    assert!(
        log.iter()
            .any(|l| l.starts_with("download-denied:err(") && l.contains("not-granted.invalid")),
        "and an ungranted host was refused at the call, by name: {log:?}"
    );
}

/// The digest the guest pinned is not the body's: the guest is told so, and
/// there is no file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tampered_download_reaches_the_guest_as_a_failure() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = Harness::new();
    let caps = h.granted(A);
    h.spawn(wasm, A, &sha(b"what was pinned"), caps).await;

    let line = wait_for(
        &h.data_base,
        A,
        |l| l.starts_with("8:job-finished"),
        Duration::from_secs(15),
    )
    .await
    .unwrap_or_else(|| panic!("no outcome: {:?}", recorded(&h.data_base, A)));
    assert!(line.contains("sha256 mismatch"), "{line}");
    assert!(!h.dest(A).exists(), "nothing was installed");
}

/// No `net:http` grant: refused at the call, nothing fetched, no event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_without_the_net_grant_is_refused_at_the_call() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = Harness::new();
    let fs_only = vec![Capability::FsWrite(
        h.dest(A).parent().unwrap().to_path_buf(),
    )];
    h.spawn(wasm, A, &sha(BODY), fs_only).await;

    let line = wait_for(
        &h.data_base,
        A,
        |l| l.starts_with("download:"),
        Duration::from_secs(5),
    )
    .await
    .expect("the guest recorded the call's result");
    assert!(line.starts_with("download:err("), "{line}");
    assert!(line.contains("net:http"), "names the grant to fix: {line}");
    assert!(!h.dest(A).exists());
}

/// `net:http` without `fs:write` over the destination is refused too — the
/// network grant does not imply somewhere to put the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_without_the_write_grant_is_refused_at_the_call() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = Harness::new();
    h.spawn(
        wasm,
        A,
        &sha(BODY),
        vec![Capability::NetHttp("127.0.0.1".into())],
    )
    .await;

    let line = wait_for(
        &h.data_base,
        A,
        |l| l.starts_with("download:"),
        Duration::from_secs(5),
    )
    .await
    .expect("the guest recorded the call's result");
    assert!(line.starts_with("download:err("), "{line}");
    assert!(line.contains("writable paths"), "{line}");
    assert!(!h.dest(A).exists());
}

/// **Addressing.** Two plugins on one bus, both subscribed to the download
/// kinds. Only A downloads; B must hear nothing — which URL a plugin fetches,
/// and where to, is not another plugin's to learn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_plugin_subscribed_to_downloads_hears_nothing() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = Harness::new();
    // B first, so it is subscribed before A's download runs. It asks for a
    // download it is not granted, so it subscribes and starts nothing.
    h.spawn(wasm, B, &sha(BODY), Vec::new()).await;
    let caps = h.granted(A);
    h.spawn(wasm, A, &sha(BODY), caps).await;

    wait_for(
        &h.data_base,
        A,
        |l| l == "8:job-finished:ok",
        Duration::from_secs(15),
    )
    .await
    .unwrap_or_else(|| panic!("A's download finished: {:?}", recorded(&h.data_base, A)));
    // A's outcome has been delivered; give a leaked copy time to reach B.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let b = recorded(&h.data_base, B);
    assert!(
        b.iter().any(|l| l.starts_with("download:err(")),
        "B subscribed and was refused its own: {b:?}"
    );
    assert!(
        !b.iter().any(|l| l.starts_with("8:")),
        "B heard nothing of A's download: {b:?}"
    );
}
