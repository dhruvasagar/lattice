//! LH.0.4 end-to-end — a guest registers a language server with the editor.
//!
//! The supervisor's own tests (`lattice-lsp`) cover what a registration does to
//! the config list and which binary then runs. What needs a real guest is the
//! seam in front of it: that the config crosses the boundary intact, that the
//! trust tier decides who may register, and that a registration is withdrawn
//! when the plugin goes away — the last being the property that keeps an
//! unloaded server manager from leaving the editor pointed at binaries nobody
//! manages.
//!
//! The registrar here is a recorder. It stands where the LSP supervisor stands
//! in the editor, behind the same `lattice-mode` trait, which is the point of
//! the trait: the plugin host cannot tell the difference and does not depend on
//! `lattice-lsp` to find out.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_mode::{CapabilitySet, LanguageServerRegistrar, LanguageServerSpec};
use lattice_plugin_host::manifest::Capability;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_runtime::EventBus;
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("EVENTS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// What the editor was asked to do, in order.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Register(u64, LanguageServerSpec),
    Unregister(u64),
}

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<Call>>,
}

impl Recorder {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl LanguageServerRegistrar for Recorder {
    fn register(&self, spec: LanguageServerSpec) -> Result<u64, String> {
        let mut calls = self.calls.lock().unwrap();
        let token = 100 + calls.len() as u64;
        calls.push(Call::Register(token, spec));
        Ok(token)
    }

    fn unregister(&self, token: u64) {
        self.calls.lock().unwrap().push(Call::Unregister(token));
    }
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
    editor: Arc<Recorder>,
    actor: Option<tokio::task::JoinHandle<()>>,
}

impl Harness {
    /// Stop the plugin and wait for its instance to be gone.
    async fn unload(&mut self) {
        if let Some(actor) = self.actor.take() {
            actor.abort();
            let _ = actor.await;
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(actor) = &self.actor {
            actor.abort();
        }
    }
}

/// Spawn the fixture, whose manifest asks for `proc:spawn`, at `tier`, asking
/// it to register `zig` → `/managed/zls`. `wired` decides whether the host was
/// given a registrar at all; `withdraw` whether the guest unregisters at once.
async fn start(wasm: &str, tier: TrustTier, wired: bool, withdraw: bool) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        data_dir.join("server-request"),
        "zig\n/managed/zls\n*.zig\n",
    )
    .unwrap();
    if withdraw {
        std::fs::write(data_dir.join("server-withdraw"), "").unwrap();
    }

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let editor = Arc::new(Recorder::default());
    if wired {
        host.set_language_server_registrar(editor.clone());
        assert!(host.language_server_registrar_wired());
    }
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
        _dirs: dirs,
        data_dir,
        editor,
        actor: Some(tokio::spawn(actor.run())),
    }
}

/// The config crosses intact, and the registration ends with the plugin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bundled_plugin_registers_a_server_and_unloading_withdraws_it() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = start(wasm, TrustTier::Bundled, true, false).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("register:")).await;
    assert_eq!(line.as_deref(), Some("register:ok"));
    let calls = h.editor.calls();
    assert_eq!(
        calls,
        vec![Call::Register(
            100,
            LanguageServerSpec {
                id: "zig".into(),
                command: "/managed/zls".into(),
                args: vec!["--stdio".into()],
                env: Vec::new(),
                root_markers: vec![".git".into()],
                file_patterns: vec!["*.zig".into()],
                language_id: "zig".into(),
                initialization_options: None,
            }
        )],
        "registered once, every field as the guest wrote it, and still registered"
    );

    h.unload().await;
    assert_eq!(
        h.editor.calls().last(),
        Some(&Call::Unregister(100)),
        "the instance going away withdrew its server"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_can_withdraw_its_own_registration() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let mut h = start(wasm, TrustTier::Bundled, true, true).await;

    wait_for(&h.data_dir, |l| l == "unregister:done")
        .await
        .unwrap_or_else(|| panic!("no withdraw: {:?}", recorded(&h.data_dir)));
    let calls = h.editor.calls();
    assert!(matches!(
        calls.as_slice(),
        [Call::Register(100, _), Call::Unregister(100)]
    ));

    // Already withdrawn: unloading must not withdraw it a second time.
    h.unload().await;
    assert_eq!(h.editor.calls().len(), 2);
}

/// **The trust boundary.** Registering a server is telling the editor to run a
/// program, so it is refused to the tier that may not spawn one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_installed_plugin_cannot_register_a_server() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, TrustTier::UserInstalled, true, false).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("register:"))
        .await
        .expect("the guest recorded the call's result");
    assert!(line.starts_with("register:err("), "{line}");
    assert!(line.contains("proc:spawn"), "names the grant: {line}");
    assert!(h.editor.calls().is_empty(), "the editor was never asked");
}

/// An editor with no LSP subsystem says so, rather than accepting a
/// registration nothing will ever act on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_registrar_wired_the_call_is_refused_by_name() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, TrustTier::Bundled, false, false).await;

    let line = wait_for(&h.data_dir, |l| l.starts_with("register:"))
        .await
        .expect("the guest recorded the call's result");
    assert!(line.starts_with("register:err("), "{line}");
    assert!(line.contains("no language-server support"), "{line}");
}
