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
//! * a digest that does not match installs nothing and leaves nothing;
//! * the editor is told to run the installed binary by its real path, is
//!   told again after a restart, is moved to the new version by an update
//!   before the old one is withdrawn, and is told to stop by an uninstall.
//!
//! "The editor" here is a recorder behind `LanguageServerRegistrar`, where
//! the LSP supervisor stands in the real one. What a registration then does —
//! which binary a matching buffer runs — is the supervisor's own test
//! (`lattice-lsp`, LH.0.4).
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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_core::BufferId;
use lattice_grammar::{Args, CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::{CapabilitySet, LanguageServerRegistrar, LanguageServerSpec};
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

/// What the editor was told, in order.
#[derive(Debug, Clone, PartialEq)]
enum Told {
    Register(u64, LanguageServerSpec),
    Unregister(u64),
}

#[derive(Default)]
struct Recorder {
    told: Mutex<Vec<Told>>,
}

impl LanguageServerRegistrar for Recorder {
    fn register(&self, spec: LanguageServerSpec) -> Result<u64, String> {
        let mut told = self.told.lock().unwrap();
        let token = 100 + told.len() as u64;
        told.push(Told::Register(token, spec));
        Ok(token)
    }

    fn unregister(&self, token: u64) {
        self.told.lock().unwrap().push(Told::Unregister(token));
    }
}

/// One overlay entry for this machine's platform, at version `1.0`.
fn entry(name: &str, port: u16, sha256: &str) -> String {
    entry_at(name, "1.0", port, sha256)
}

fn entry_at(name: &str, version: &str, port: u16, sha256: &str) -> String {
    format!(
        r#"
[[server]]
name = "{name}"
lsp-id = "fake"
language-id = "fake"
version = "{version}"
args = ["--stdio"]
file-patterns = ["*.fake"]
root-markers = [".git"]

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
    /// `Option` so a restart can carry the directories into the next editor.
    dirs: Option<TempDir>,
    data_dir: PathBuf,
    host: PluginHost,
    output: PluginOutputHandle,
    lsp: Arc<Recorder>,
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
    let data_dir = dirs.path().join("data").join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("registry.toml"), registry).unwrap();
    before(&data_dir);
    boot_in(wasm, dirs).await
}

/// Start an editor over directories that may already hold a previous
/// session's installs.
async fn boot_in(wasm: &str, dirs: TempDir) -> Editor {
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let output: PluginOutputHandle = Arc::new(PluginOutput::new());
    host.set_plugin_output(output.clone());
    let lsp = Arc::new(Recorder::default());
    host.set_language_server_registrar(lsp.clone());
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
        dirs: Some(dirs),
        data_dir,
        host,
        output,
        lsp,
        commands,
        actor,
    }
}

impl Editor {
    /// Quit, and start again on the same disk: a new host, new plugin
    /// instances, nothing in memory carried over.
    async fn restart(mut self, wasm: &str) -> Editor {
        self.actor.abort();
        let dirs = self.dirs.take().expect("the directories are still held");
        drop(self);
        boot_in(wasm, dirs).await
    }

    fn told(&self) -> Vec<Told> {
        self.lsp.told.lock().unwrap().clone()
    }

    fn lsp_install(&self, arg: &str) -> lattice_grammar::effect::Effect {
        self.command("lsp-install", arg)
    }

    /// Run `:<name> <arg>` through the real sync trampoline.
    fn command(&self, name: &str, arg: &str) -> lattice_grammar::effect::Effect {
        let id = self
            .commands
            .id_by_name(name)
            .unwrap_or_else(|| panic!(":{name} is registered"));
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

    /// Run a row action of `*lsp-servers*` as the chord would: on a buffer
    /// holding the list's text, with the cursor on `line`.
    fn row_action(&self, action: &str, text: &str, line: u32) -> lattice_grammar::effect::Effect {
        let id = self
            .commands
            .id_by_name(action)
            .unwrap_or_else(|| panic!("the action `{action}` is registered"));
        let mut document = lattice_core::Document::from_text(text);
        let cancel = CancellationToken::never();
        tokio::task::block_in_place(|| {
            lattice_grammar::execute_with_env(
                &self.commands,
                &mut document,
                BufferId(1),
                Position { line, byte: 0 },
                CommandInvocation::of(id),
                &cancel,
                GrammarEnv::default(),
            )
            .expect("the action dispatches")
        })
    }

    /// `*lsp-servers*` as the output store has it.
    fn list(&self) -> OutputSnapshot {
        self.output.snapshot("*lsp-servers*").unwrap_or_default()
    }

    /// Wait until some row of the list contains `want`.
    async fn list_shows(&self, want: &str) -> OutputSnapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let snap = self.list();
            if snap.lines.iter().any(|l| l.contains(want))
                || tokio::time::Instant::now() >= deadline
            {
                return snap;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
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
            "Registered with the editor: files opened from now on use it.".to_string(),
        ]
    );

    // And the editor was told to run it — by the path the HOST knows it at,
    // with everything else as the registry wrote it.
    let told = editor.told();
    let [Told::Register(_, spec)] = told.as_slice() else {
        panic!("exactly one registration: {told:?}");
    };
    assert_eq!(
        std::fs::canonicalize(&spec.command).unwrap(),
        std::fs::canonicalize(&binary).unwrap()
    );
    assert!(spec.command.is_absolute());
    assert_eq!(spec.id, "fake");
    assert_eq!(spec.language_id, "fake");
    assert_eq!(spec.args, vec!["--stdio"]);
    assert_eq!(spec.file_patterns, vec!["*.fake"]);
    assert_eq!(spec.root_markers, vec![".git"]);
}

fn echoed(effect: lattice_grammar::effect::Effect) -> String {
    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => text,
        other => panic!("expected an echo, got {other:?}"),
    }
}

/// A registration lasts as long as the plugin instance that made it, so an
/// install outlives the session only if startup makes it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_installed_server_is_registered_again_after_a_restart() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let archive = gzip(SERVER_SCRIPT);
    let port = serve(archive.clone());
    let editor = boot(wasm, &entry("fake-ls", port, &sha256_hex(&archive)), |_| {}).await;
    editor.lsp_install("fake-ls");
    assert_eq!(
        editor.settled("fake-ls").await.status.map(|s| s.state),
        Some(OutputState::Succeeded)
    );
    let binary = std::fs::canonicalize(editor.data_dir.join("lsp/fake-ls/1.0/fake-ls")).unwrap();

    let editor = editor.restart(wasm).await;

    // No command was run in this session. `register-events` has returned by
    // the time `boot` does, and that is where the registration is made.
    let told = editor.told();
    let [Told::Register(_, spec)] = told.as_slice() else {
        panic!("registered once at startup, with nothing asked: {told:?}");
    };
    assert_eq!(std::fs::canonicalize(&spec.command).unwrap(), binary);
    assert_eq!(spec.id, "fake");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_uninstall_withdraws_the_server_and_removes_its_files() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let archive = gzip(SERVER_SCRIPT);
    let port = serve(archive.clone());
    let editor = boot(wasm, &entry("fake-ls", port, &sha256_hex(&archive)), |_| {}).await;

    // Not installed yet: refused on the spot, nothing published.
    let text = echoed(editor.command("lsp-uninstall", "fake-ls"));
    assert!(text.contains("not installed"), "{text}");

    editor.lsp_install("fake-ls");
    let installed = editor.settled("fake-ls").await;
    assert_eq!(
        installed.status.as_ref().map(|s| s.state),
        Some(OutputState::Succeeded)
    );
    let told = editor.told();
    let [Told::Register(token, _)] = told.as_slice() else {
        panic!("registered once: {told:?}");
    };
    let token = *token;

    editor.command("lsp-uninstall", "fake-ls");
    let log = editor.settled_after("fake-ls", Some(installed.epoch)).await;

    assert_eq!(
        log.status.as_ref().map(|s| s.text.as_str()),
        Some("fake-ls uninstalled"),
        "{log:?}"
    );
    assert_eq!(
        editor.told().last(),
        Some(&Told::Unregister(token)),
        "the editor was told to stop using it"
    );
    assert_eq!(editor.tree(), Vec::<String>::new());
    assert!(
        !editor.data_dir.join("lsp/fake-ls").exists(),
        "the server's directory is gone, not just emptied"
    );
    assert_eq!(editor.installed_record("fake-ls"), None);

    // And it stays gone: a restart finds nothing to register.
    let editor = editor.restart(wasm).await;
    assert_eq!(editor.told(), Vec::new());
}

/// An update is an install of the registry's new pin, and the order is the
/// point: fetch and verify beside the old version, register the new one,
/// withdraw the old, and only then delete it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_update_moves_to_the_new_pin_and_removes_the_old_version() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let archive = gzip(SERVER_SCRIPT);
    let digest = sha256_hex(&archive);
    let port = serve(archive);
    let editor = boot(wasm, &entry("fake-ls", port, &digest), |_| {}).await;

    // Nothing installed: nothing to update, and it says what to do instead.
    let text = echoed(editor.command("lsp-update", "fake-ls"));
    assert!(text.contains(":lsp-install fake-ls"), "{text}");

    editor.lsp_install("fake-ls");
    let first = editor.settled("fake-ls").await;
    assert_eq!(
        first.status.as_ref().map(|s| s.state),
        Some(OutputState::Succeeded)
    );

    // Installed at the pin: up to date, for one and for all.
    let text = echoed(editor.command("lsp-update", "fake-ls"));
    assert!(text.contains("fake-ls 1.0 is up to date"), "{text}");
    let text = echoed(editor.command("lsp-update-all", ""));
    assert!(text.contains("up to date"), "{text}");

    // The registry moves to 1.1.
    std::fs::write(
        editor.data_dir.join("registry.toml"),
        entry_at("fake-ls", "1.1", port, &digest),
    )
    .unwrap();
    let text = echoed(editor.command("lsp-update-all", ""));
    assert!(text.contains("updating fake-ls"), "{text}");
    let log = editor.settled_after("fake-ls", Some(first.epoch)).await;
    assert_eq!(
        log.status.as_ref().map(|s| s.text.as_str()),
        Some("fake-ls 1.1 installed"),
        "{log:?}"
    );
    assert!(
        log.lines[0].ends_with("(replacing 1.0)"),
        "the first line says what it replaces: {:?}",
        log.lines
    );

    let told = editor.told();
    let [
        Told::Register(old, _),
        Told::Register(_, new),
        Told::Unregister(withdrawn),
    ] = told.as_slice()
    else {
        panic!("register 1.0, register 1.1, THEN withdraw 1.0: {told:?}");
    };
    assert_eq!(
        withdrawn, old,
        "it is the old registration that is withdrawn"
    );
    assert!(
        new.command.ends_with("lsp/fake-ls/1.1/fake-ls"),
        "{:?}",
        new.command
    );
    assert_eq!(editor.tree(), vec!["fake-ls/1.1/fake-ls"], "1.0 is gone");
    assert_eq!(
        editor.installed_record("fake-ls").as_deref(),
        Some("1.1\nfake-ls")
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
        let text = echoed(editor.lsp_install(arg));
        assert!(
            text.contains("fake-ls") && text.contains("rust-analyzer"),
            "{arg:?}: names the overlay's server and the bundled one: {text}"
        );
    }
    assert_eq!(editor.told(), Vec::new(), "the editor was told nothing");
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

/// The line number of `server`'s row in a rendered list.
fn row_of(list: &OutputSnapshot, server: &str) -> u32 {
    list.lines
        .iter()
        .position(|l| l.trim_start().starts_with(server))
        .unwrap_or_else(|| panic!("no row for {server}: {:?}", list.lines)) as u32
}

fn text_of(list: &OutputSnapshot) -> String {
    let mut text = list.lines.join("\n");
    text.push('\n');
    text
}

/// `:lsp-servers` opens the list with the mode that owns its chords, and the
/// list is drawn from the registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_servers_lists_the_registry_in_a_buffer_with_its_own_mode() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let editor = boot(wasm, &entry("fake-ls", 1, &"0".repeat(64)), |_| {}).await;

    match editor.command("lsp-servers", "") {
        lattice_grammar::effect::Effect::OpenSyntheticBuffer {
            name,
            mode_id,
            activate_minor,
            ..
        } => {
            assert_eq!(name, "*lsp-servers*");
            assert_eq!(mode_id, "plugin-output-mode");
            assert_eq!(
                activate_minor.as_deref(),
                Some("lighthouse-servers-mode"),
                "the chords ride the output mode on this one buffer"
            );
        }
        other => panic!("expected the list to be opened, got {other:?}"),
    }

    let list = editor.list_shows("fake-ls").await;
    assert_eq!(
        list.lines,
        vec![
            "  Server         Version     Status",
            "  fake-ls        1.0         not installed",
            "  rust-analyzer  2026-10-05  not installed",
            "",
            "i install   u update   x uninstall   <CR> show log   gr refresh",
        ],
        "the overlay's server and the bundled one, aligned"
    );
    assert_eq!(
        list.status.map(|s| s.text),
        Some("2 servers \u{b7} 0 installed".to_string())
    );
}

/// The manager loop: `i` on a row installs that server, and the row changes
/// by itself when the install finishes — nobody asks for a redraw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_row_chord_acts_on_its_server_and_the_list_redraws_itself() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: lighthouse component not built");
        return;
    };
    let archive = gzip(SERVER_SCRIPT);
    let port = serve(archive.clone());
    let editor = boot(wasm, &entry("fake-ls", port, &sha256_hex(&archive)), |_| {}).await;
    editor.command("lsp-servers", "");
    let list = editor.list_shows("fake-ls").await;
    let text = text_of(&list);

    // On the heading, or the keys line: nothing to act on, and it says so.
    for line in [0, list.lines.len() as u32 - 1] {
        let said = echoed(editor.row_action("lsp-servers-install", &text, line));
        assert!(said.contains("no server on this line"), "{said}");
    }
    // `u` and `x` on a server that is not installed are answered on the spot.
    let row = row_of(&list, "fake-ls");
    let said = echoed(editor.row_action("lsp-servers-update", &text, row));
    assert!(said.contains(":lsp-install fake-ls"), "{said}");
    let said = echoed(editor.row_action("lsp-servers-uninstall", &text, row));
    assert!(said.contains("not installed"), "{said}");
    assert_eq!(editor.tree(), Vec::<String>::new(), "nothing happened yet");

    // `i` stays in the list — it answers in the echo area, not by opening
    // the log — and the row is what shows the result.
    let said = echoed(editor.row_action("lsp-servers-install", &text, row));
    assert!(said.contains("installing fake-ls"), "{said}");

    let list = editor
        .list_shows("fake-ls        1.0         installed")
        .await;
    assert!(
        list.lines
            .iter()
            .any(|l| l == "  fake-ls        1.0         installed"),
        "the row became `installed` with no redraw requested: {:?}",
        list.lines
    );
    assert_eq!(
        list.status.as_ref().map(|s| s.text.as_str()),
        Some("2 servers \u{b7} 1 installed")
    );
    assert_eq!(editor.tree(), vec!["fake-ls/1.0/fake-ls"]);

    // `<CR>` opens that server's log.
    let text = text_of(&list);
    let row = row_of(&list, "fake-ls");
    match editor.row_action("lsp-servers-log", &text, row) {
        lattice_grammar::effect::Effect::OpenSyntheticBuffer { name, mode_id, .. } => {
            assert_eq!(name, "*lsp-install:fake-ls*");
            assert_eq!(mode_id, "plugin-output-mode");
        }
        other => panic!("expected the log to be opened, got {other:?}"),
    }

    // `x` removes it, and the row goes back.
    let said = echoed(editor.row_action("lsp-servers-uninstall", &text, row));
    assert!(said.contains("uninstalling fake-ls"), "{said}");
    let list = editor
        .list_shows("fake-ls        1.0         not installed")
        .await;
    assert!(
        list.lines
            .iter()
            .any(|l| l == "  fake-ls        1.0         not installed"),
        "{:?}",
        list.lines
    );
    assert_eq!(editor.tree(), Vec::<String>::new());
}
