//! OB.2 end-to-end — a USER-INSTALLED plugin runs exactly the programs its
//! option-bound grant names, and nothing else.
//!
//! `spawn_seam.rs` proves the unbound `proc:spawn` is bundled-only. This is
//! the other half: `proc:spawn:{events-fixture.langs:command}` lets a user
//! plugin spawn the `command` of each record the user configured, decided by
//! a real grant computation against a real config registry, through the real
//! seam. The refusal must name the option, so the user knows where to look.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_config::{ConfigRegistry, ConfigValue, HeldSource};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_runtime::EventBus;
use tempfile::TempDir;

const PLUGIN_ID: &str = "events-fixture";
const OPTION: &str = "events-fixture.langs";

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

fn language(command: &str) -> ConfigValue {
    ConfigValue::record([
        ("name".to_string(), ConfigValue::Str("x".into())),
        ("command".to_string(), ConfigValue::Str(command.into())),
    ])
}

/// Spawn the fixture as a user-installed plugin whose grant follows `OPTION`,
/// with `configured` held for it, asking it to run `<program> -c <script>`.
async fn start(wasm: &str, configured: Vec<ConfigValue>, program: &str, script: &str) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let data_dir = data_base.join(PLUGIN_ID).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let script = script.replace("{dir}", dirs.path().to_str().unwrap());
    std::fs::write(
        data_dir.join("spawn-request"),
        format!("{program}\n-c\n{script}\n"),
    )
    .unwrap();

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let config = Arc::new(ConfigRegistry::default());
    config.hold_tree(
        OPTION,
        ConfigValue::List(configured),
        HeldSource::ConfigFile,
    );
    host.set_config_registry(config);
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::from_toml_str(&format!(
        "id = \"{PLUGIN_ID}\"\ncapabilities = [\"proc:spawn:{{{OPTION}:command}}\"]\n"
    ))
    .unwrap();
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
        dirs,
        data_dir,
        actor: tokio::spawn(actor.run()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_plugin_spawns_a_listed_command() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, vec![language("sh")], "sh", "echo hi").await;
    let line = wait_for(&h.data_dir, |l| l.starts_with("10:exit:")).await;
    let log = recorded(&h.data_dir);
    assert_eq!(line.as_deref(), Some("10:exit:ok"), "{log:?}");
    assert!(log.iter().any(|l| l == "10:out:hi"), "{log:?}");
}

/// Refused at the seam, naming the option; and nothing ran.
async fn assert_refused(configured: Vec<ConfigValue>, program: &str) {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, configured, program, "touch {dir}/ran").await;
    let line = wait_for(&h.data_dir, |l| l.starts_with("spawn:"))
        .await
        .expect("the guest recorded the call's result");
    assert!(line.starts_with("spawn:err("), "{line}");
    assert!(line.contains(OPTION), "names the option to change: {line}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!h.dirs.path().join("ran").exists(), "and nothing ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unlisted_command_is_refused_naming_the_option() {
    assert_refused(vec![language("python3")], "sh").await;
}

/// Review focus #3: the grant is the configured string, not a resolved path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_absolute_path_does_not_match_a_bare_name() {
    assert_refused(vec![language("sh")], "/bin/sh").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_list_permits_nothing() {
    assert_refused(vec![], "sh").await;
}
