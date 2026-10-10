//! LH.1.1 — `:lsp-install` through the real lighthouse component.
//!
//! The install state machine is unit-tested inside the plugin against a fake
//! host (`plugins/lighthouse/src/install.rs`), and those tests prove nothing
//! about whether the seams fire. This drives the shipped component the way
//! the editor does — the ex-command on a grammar instance, the work on an
//! events instance, sharing only a bus, a store and a data directory — and
//! asserts what only that can show:
//!
//! * the command's request crosses from one instance to the other;
//! * a real download, checked against a real SHA-256, lands where the plugin
//!   said, through `http-download` → `extract-archive` → `set-executable`,
//!   with **no `fs:` capability** in the manifest;
//! * the plugin's own filesystem calls (`/data/…`) and the host's (the real
//!   path) are talking about the same files — the final rename is a guest
//!   call on a tree the host wrote;
//! * every step is in the output buffer's store, in order;
//! * a digest that does not match installs nothing and leaves nothing.
//!
//! The server it installs is a shell script served from loopback, named in an
//! overlay registry — the same `registry.toml` a user would write to add a
//! server of their own.
//!
//! Skips when the component was not built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_core::BufferId;
use lattice_grammar::{Args, CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::output::{OutputSnapshot, OutputState, PluginOutput, PluginOutputHandle};
use lattice_plugin_host::{Capability, PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Must match `plugins/lighthouse/plugin.toml` — the store and the data
/// directory are keyed by it.
const PLUGIN_ID: &str = "lighthouse";

/// What the "server" is: enough of a program to prove it arrived intact and
/// runnable.
const SERVER_SCRIPT: &[u8] = b"#!/bin/sh\necho fake-ls ready\n";

fn plugin_wasm() -> Option<&'static str> {
    let path = env!("LIGHTHOUSE_PLUGIN_WASM");
    (!path.is_empty()).then_some(path)
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Serve `body` for every request, on loopback. Returns the port.
fn serve(body: Vec<u8>) -> u16 {
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
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    port
}

/// One overlay entry for this machine's platform.
fn entry(name: &str, port: u16, sha256: &str) -> String {
    format!(
        r#"
[[server]]
name = "{name}"
lsp-id = "fake"
language-id = "fake"
version = "1.0"
file-patterns = ["*.fake"]

[server.platform.{os}-{arch}]
url = "http://127.0.0.1:{port}/{name}.gz"
sha256 = "{sha256}"
archive = "gz"
binary = "{name}"
"#,
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
    )
}

struct Editor {
    _dirs: TempDir,
    data_dir: PathBuf,
    host: PluginHost,
    output: PluginOutputHandle,
    commands: CommandRegistry,
    actor: tokio::task::JoinHandle<()>,
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// Stand lighthouse up as the loader does — a grammar instance and an events
/// instance of one component on one bus — with `registry` as its overlay and
/// `before` run on the data directory first.
async fn boot(wasm: &str, registry: &str, before: impl FnOnce(&Path)) -> Editor {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("registry.toml"), registry).unwrap();
    before(&data_dir);

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let output: PluginOutputHandle = Arc::new(PluginOutput::new());
    host.set_plugin_output(output.clone());
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    // The shipped manifest's capabilities, with loopback standing in for the
    // release hosts. Note what is absent: any `fs:` grant.
    let manifest = PluginManifest::new(
        PLUGIN_ID,
        vec![
            Capability::NetHttp("127.0.0.1".to_string()),
            Capability::ProcSpawn,
            Capability::StateWrite,
        ],
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
        .expect("the events instance spawns");
    let actor = tokio::spawn(actor.run());

    let grammar = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("the grammar instance instantiates");
    let mut commands = CommandRegistry::new();
    grammar.register_all(&mut commands);

    Editor {
        _dirs: dirs,
        data_dir,
        host,
        output,
        commands,
        actor,
    }
}

impl Editor {
    /// Run `:lsp-install <arg>` through the real sync trampoline.
    fn lsp_install(&self, arg: &str) -> lattice_grammar::effect::Effect {
        let id = self
            .commands
            .id_by_name("lsp-install")
            .expect(":lsp-install is registered");
        let mut document = lattice_core::Document::from_text("x\n");
        let cancel = CancellationToken::never();
        let args = if arg.is_empty() {
            Args::None
        } else {
            Args::String(arg.to_string())
        };
        // On the editor this runs on the dispatch thread, outside any async
        // runtime; `block_in_place` gives the test's worker the same standing.
        tokio::task::block_in_place(|| self.run(id, args, &mut document, &cancel))
    }

    fn run(
        &self,
        id: lattice_grammar::CommandId,
        args: Args,
        document: &mut lattice_core::Document,
        cancel: &CancellationToken,
    ) -> lattice_grammar::effect::Effect {
        lattice_grammar::execute_with_env(
            &self.commands,
            document,
            BufferId(1),
            Position { line: 0, byte: 0 },
            CommandInvocation::of(id).with_args(args),
            cancel,
            GrammarEnv::default(),
        )
        .expect("the command dispatches")
    }

    fn log(&self, server: &str) -> OutputSnapshot {
        self.output
            .snapshot(&format!("*lsp-install:{server}*"))
            .unwrap_or_default()
    }

    /// Wait for the install to end, one way or the other.
    async fn settled(&self, server: &str) -> OutputSnapshot {
        self.settled_after(server, None).await
    }

    /// Wait for an install to end on a page LATER than `previous`. A second
    /// attempt starts by resetting the buffer, and until that reset lands the
    /// buffer still shows the first attempt's verdict — which is "ended", but
    /// not the ending being waited for.
    async fn settled_after(&self, server: &str, previous: Option<u64>) -> OutputSnapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let snap = self.log(server);
            let done = previous != Some(snap.epoch)
                && snap
                    .status
                    .as_ref()
                    .is_some_and(|s| s.state != OutputState::Running);
            if done || tokio::time::Instant::now() >= deadline {
                return snap;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Everything under the install root, relative to it, sorted.
    fn tree(&self) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.push(path.strip_prefix(root).unwrap().display().to_string());
                }
            }
        }
        let root = self.data_dir.join("lsp");
        let mut out = Vec::new();
        walk(&root, &root, &mut out);
        out.sort();
        out
    }

    fn installed_record(&self, server: &str) -> Option<String> {
        self.host
            .plugin_store_get(PLUGIN_ID, &format!("installed/{server}"))
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }
}

/// **The test that matters.**
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_install_downloads_verifies_and_installs_a_server() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let archive = gzip(SERVER_SCRIPT);
    let port = serve(archive.clone());
    let editor = boot(wasm, &entry("fake-ls", port, &sha256_hex(&archive)), |_| {}).await;

    // The command returns at once, and what it returns is the buffer.
    let effect = editor.lsp_install("fake-ls");
    match effect {
        lattice_grammar::effect::Effect::OpenSyntheticBuffer { name, mode_id, .. } => {
            assert_eq!(name, "*lsp-install:fake-ls*");
            assert_eq!(mode_id, "plugin-output-mode");
        }
        other => panic!("expected the output buffer to be opened, got {other:?}"),
    }

    let log = editor.settled("fake-ls").await;
    let status = log.status.clone().expect("a status was set");
    assert_eq!(
        status.state,
        OutputState::Succeeded,
        "the install succeeded: {log:?}"
    );
    assert_eq!(status.text, "fake-ls 1.0 installed");

    // One file, in the versioned tree, and nothing else — no archive, no
    // staging directory.
    assert_eq!(editor.tree(), vec!["fake-ls/1.0/fake-ls"]);
    let binary = editor.data_dir.join("lsp/fake-ls/1.0/fake-ls");
    assert_eq!(std::fs::read(&binary).unwrap(), SERVER_SCRIPT);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "the server is executable: {mode:o}");
        let out = std::process::Command::new(&binary).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "fake-ls ready\n");
    }
    assert_eq!(
        editor.installed_record("fake-ls").as_deref(),
        Some("1.0\nfake-ls")
    );

    // The story, in the order it happened.
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    assert_eq!(
        log.lines,
        vec![
            format!("Installing fake-ls 1.0 for {platform}"),
            format!("Downloading http://127.0.0.1:{port}/fake-ls.gz"),
            "Downloaded; SHA-256 verified".to_string(),
            "Unpacking".to_string(),
            "Installed fake-ls 1.0".to_string(),
        ]
    );
}

/// The supply-chain property: what is served does not match the pinned
/// digest, so nothing is installed and nothing is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_that_does_not_match_its_digest_installs_nothing() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let port = serve(gzip(b"#!/bin/sh\necho something else entirely\n"));
    let pinned = sha256_hex(&gzip(SERVER_SCRIPT));
    let editor = boot(wasm, &entry("fake-ls", port, &pinned), |_| {}).await;

    editor.lsp_install("fake-ls");
    let log = editor.settled("fake-ls").await;

    let status = log.status.clone().expect("a status was set");
    assert_eq!(status.state, OutputState::Failed, "{log:?}");
    assert!(
        log.lines
            .iter()
            .any(|l| l.starts_with("error: ") && l.to_lowercase().contains("sha")),
        "the buffer says why: {:?}",
        log.lines
    );
    assert!(
        !log.lines.iter().any(|l| l == "Unpacking"),
        "an unverified download is never unpacked"
    );
    assert_eq!(editor.tree(), Vec::<String>::new(), "no partial tree");
    assert_eq!(editor.installed_record("fake-ls"), None);
}

/// A server that is not there at all — the failure most people will actually
/// meet (offline, or a release that was withdrawn).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_download_fails_in_the_buffer_and_can_be_retried() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    // A port nothing listens on: bound, read, and released.
    let dead = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let archive = gzip(SERVER_SCRIPT);
    let digest = sha256_hex(&archive);
    let editor = boot(wasm, &entry("fake-ls", dead, &digest), |_| {}).await;

    editor.lsp_install("fake-ls");
    let log = editor.settled("fake-ls").await;
    assert_eq!(
        log.status.as_ref().map(|s| s.state),
        Some(OutputState::Failed),
        "{log:?}"
    );
    assert_eq!(editor.tree(), Vec::<String>::new());

    // Fix the registry, run it again: the second attempt starts on a clean
    // page and succeeds. Nothing about the failure is sticky.
    let port = serve(archive);
    std::fs::write(
        editor.data_dir.join("registry.toml"),
        entry("fake-ls", port, &digest),
    )
    .unwrap();
    let first_page = log.epoch;
    editor.lsp_install("fake-ls");
    let log = editor.settled_after("fake-ls", Some(first_page)).await;
    assert_eq!(
        log.status.as_ref().map(|s| s.state),
        Some(OutputState::Succeeded),
        "{log:?}"
    );
    assert!(
        !log.lines.iter().any(|l| l.starts_with("error: ")),
        "the first attempt's error is gone from the page: {:?}",
        log.lines
    );
    assert_eq!(editor.tree(), vec!["fake-ls/1.0/fake-ls"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_server_is_refused_with_the_names_that_exist() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let editor = boot(wasm, &entry("fake-ls", 1, &"0".repeat(64)), |_| {}).await;

    for arg in ["gopls-nightly", ""] {
        match editor.lsp_install(arg) {
            lattice_grammar::effect::Effect::Echo { text, .. } => {
                assert!(
                    text.contains("fake-ls") && text.contains("rust-analyzer"),
                    "{arg:?}: names the overlay's server and the bundled one: {text}"
                );
            }
            other => panic!("{arg:?}: expected an echo, got {other:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(editor.tree(), Vec::<String>::new());
    assert!(
        editor
            .output
            .snapshot("*lsp-install:gopls-nightly*")
            .is_none(),
        "no buffer was written for a server that does not exist"
    );
}

/// What an editor exit mid-install leaves behind is removed the next time the
/// plugin starts — and an installed server beside it is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starting_up_removes_the_scratch_files_of_an_interrupted_install() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let editor = boot(wasm, &entry("fake-ls", 1, &"0".repeat(64)), |data| {
        let lsp = data.join("lsp/fake-ls");
        std::fs::create_dir_all(lsp.join("0.9")).unwrap();
        std::fs::write(lsp.join("0.9/fake-ls"), "installed").unwrap();
        std::fs::create_dir_all(lsp.join("1.0.partial")).unwrap();
        std::fs::write(lsp.join("1.0.partial/fake-ls"), "half").unwrap();
        std::fs::write(lsp.join("1.0.download"), "half").unwrap();
        std::fs::write(lsp.join("1.0.download.part"), "half").unwrap();
    })
    .await;

    // `register-events` has returned by the time `boot` does.
    assert_eq!(editor.tree(), vec!["fake-ls/0.9/fake-ls"]);
}
