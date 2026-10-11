//! LH.0.6 end-to-end — a guest learns the host platform and the real path of
//! its own data directory, and can hand that path to a host-side call.
//!
//! The grant arithmetic is unit-tested in `host_services.rs`. What needs a
//! real guest is the join: that the path `data-dir` returns is the directory
//! WASI mounted at `/data` (a file the guest wrote there is the file the host
//! then acts on), that a host-side call accepts it with NO `fs:` capability in
//! the manifest, and that the reach stops at the directory's edge.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lattice_mode::CapabilitySet;
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

async fn wait_for(data_dir: &Path, prefix: &str) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Some(line) = recorded(data_dir)
            .into_iter()
            .find(|l| l.starts_with(prefix))
        {
            return Some(line);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_names_its_data_dir_to_the_host_with_no_fs_capability() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let home = data_base.join(PLUGIN_ID);
    let data_dir = home.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("host-info-request"), "").unwrap();
    // A real file one level above the data dir, so a refusal there is the
    // grant talking and not "no such file".
    let decoy = home.join("plugin.toml");
    std::fs::write(&decoy, "id = \"events-fixture\"\n").unwrap();

    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    // No capabilities at all — in particular no `fs:write`.
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
    let actor = tokio::spawn(actor.run());

    let platform = wait_for(&data_dir, "platform:").await;
    assert_eq!(
        platform,
        Some(format!(
            "platform:{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )),
        "the platform the editor runs on, not `wasm32`"
    );

    let reported = wait_for(&data_dir, "data-dir:")
        .await
        .expect("the guest reported");
    let reported = reported.strip_prefix("data-dir:").unwrap();
    assert_eq!(
        std::fs::canonicalize(reported).unwrap(),
        std::fs::canonicalize(&data_dir).unwrap(),
        "`data-dir` is the host path of the guest's `/data`"
    );
    // The guest wrote `/data/tool` through WASI and marked `<data-dir>/tool`
    // executable through the host: one file, reached both ways.
    #[cfg(unix)]
    assert!(
        is_executable(&data_dir.join("tool")),
        "the host-side call acted on the file the guest wrote"
    );

    assert_eq!(
        wait_for(&data_dir, "escape:").await.as_deref(),
        Some("escape:denied"),
        "the directory above the data dir is not the plugin's to touch"
    );
    #[cfg(unix)]
    assert!(!is_executable(&decoy), "and it was not touched");

    actor.abort();
}
