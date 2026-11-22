//! OB.3 end-to-end — effects posted from the EVENTS seam, which returns
//! nothing, reach the editor's sink, checked as returned effects are.
//!
//! The events fixture posts from `on-event` (triggered by its own event), so
//! the call happens on the seam this exists for, with no action dispatched.
//! What the sink does with them — the inbound bus that wakes the editor — is
//! `lattice-mode`'s, pinned by `boot_context`'s inbound test and the boot pin
//! `plugin_loader_captures_every_drain_service`.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_grammar::{EchoLevel, Effect};
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
    posted: Arc<Mutex<Vec<Effect>>>,
    actor: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.actor.abort();
    }
}

/// Spawn the fixture with `request` in its post-request file and, when
/// `wired`, a sink that records what arrives.
async fn start(wasm: &str, request: &str, wired: bool, capabilities: &str) -> Harness {
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let posted = Arc::new(Mutex::new(Vec::new()));
    if wired {
        let into = Arc::clone(&posted);
        host.set_effect_sink(Arc::new(move |effect| into.lock().unwrap().push(effect)));
    }
    let (data_dir, actor) =
        spawn_on(&host, &data_base, wasm, PLUGIN_ID, request, capabilities).await;
    Harness {
        _dirs: dirs,
        data_dir,
        posted,
        actor: tokio::spawn(actor.run()),
    }
}

/// One instance of the fixture, as plugin `id`, on `host`.
async fn spawn_on(
    host: &PluginHost,
    data_base: &Path,
    wasm: &str,
    id: &str,
    request: &str,
    capabilities: &str,
) -> (PathBuf, lattice_plugin_host::event_task::EventActor) {
    let data_dir = data_base.join(id).join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("post-request"), request).unwrap();
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest =
        PluginManifest::from_toml_str(&format!("id = \"{id}\"\ncapabilities = [{capabilities}]\n"))
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
    (data_dir, actor)
}

/// The posted batch, flattened.
fn posted(h: &Harness) -> Vec<Effect> {
    h.posted
        .lock()
        .unwrap()
        .iter()
        .flat_map(|e| match e {
            Effect::Many(inner) => inner.clone(),
            other => vec![other.clone()],
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_posted_edit_reaches_the_editor_with_nothing_dispatched() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, "edit 7", true, "").await;
    let line = wait_for(&h.data_dir, |l| l.starts_with("post:")).await;
    assert_eq!(
        line.as_deref(),
        Some("post:ok"),
        "{:?}",
        recorded(&h.data_dir)
    );
    let effects = posted(&h);
    assert_eq!(effects.len(), 1, "{effects:?}");
    let Effect::ApplyEdit { target, edit, .. } = &effects[0] else {
        panic!("an apply-edit: {effects:?}");
    };
    assert_eq!(target.0, 7);
    assert_eq!(
        edit.kind,
        lattice_protocol::EditKind::Replace {
            text: "posted".to_string()
        }
    );
}

/// A posted effect is checked like a returned one: a write outside the
/// plugin's `fs:write` grant arrives as a warning, never as the write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_posted_write_outside_the_grant_is_refused() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let elsewhere = TempDir::new().unwrap();
    let target = elsewhere.path().join("x.org");
    let h = start(wasm, &format!("write {}", target.display()), true, "").await;
    wait_for(&h.data_dir, |l| l.starts_with("post:"))
        .await
        .expect("the guest posted");
    let effects = posted(&h);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::WriteToFile { .. })),
        "the write must not pass: {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Echo {
                level: EchoLevel::Warn,
                ..
            }
        )),
        "the refusal is shown: {effects:?}"
    );
}

/// Unwired, the plugin is told — nothing is silently dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unwired_sink_is_an_err_to_the_guest() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let h = start(wasm, "edit 7", false, "").await;
    let line = wait_for(&h.data_dir, |l| l.starts_with("post:"))
        .await
        .expect("the guest recorded the result");
    assert!(line.starts_with("post:err("), "{line}");
}

/// OB.3a: two events plugins posting the same range number name two ranges.
/// The events seam used to leave every instance in the unassigned space, so
/// plugin `b`'s `track-range 1` replaced plugin `a`'s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn posted_ranges_from_two_plugins_do_not_collide() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    let dirs = TempDir::new().unwrap();
    let data_base = dirs.path().join("data");
    let host = PluginHost::with_dirs(dirs.path().join("cache"), &data_base).expect("host builds");
    let posted = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&posted);
    host.set_effect_sink(Arc::new(move |effect| into.lock().unwrap().push(effect)));

    let mut actors = Vec::new();
    for id in ["fixture-a", "fixture-b"] {
        let (data_dir, actor) = spawn_on(&host, &data_base, wasm, id, "track 7 1", "").await;
        actors.push(tokio::spawn(actor.run()));
        wait_for(&data_dir, |l| l == "post:ok")
            .await
            .unwrap_or_else(|| panic!("{id} posted: {:?}", recorded(&data_dir)));
    }
    let ids: Vec<_> = posted
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Effect::TrackRange { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    for actor in actors {
        actor.abort();
    }
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1], "each plugin's range 1 is its own");
    // And neither is in the unassigned space, which no plugin is given.
    for id in ids {
        assert_ne!((id.0 >> 32) & 0x7fff_ffff, 0x7fff_ffff, "{id:?}");
    }
}

/// OB.3a: a post is unsolicited, so the effects a background job has no
/// business sending arrive as a warning naming them — never as the effect.
/// Each is posted from the events seam for real, through the WIT conversion.
/// Two are stopped a step earlier, by the command gate every effect passes
/// (a closed command, a terminal command without `proc:spawn`); the warning
/// then names what was refused rather than the variant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_posted_effect_outside_the_background_set_is_refused() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: events fixture guest not built");
        return;
    };
    for (request, name) in [
        ("quit", "QuitEditor"),
        ("set-option org.babel.languages=sh", "SetOption"),
        ("set-global-option org.directory=/", "SetGlobalOption"),
        ("invoke shell-command touch /tmp/pwned", "`shell-command`"),
        ("terminal sh -c id", "proc:spawn"),
    ] {
        let h = start(wasm, request, true, "").await;
        let line = wait_for(&h.data_dir, |l| l.starts_with("post:")).await;
        assert_eq!(line.as_deref(), Some("post:ok"), "{request}");
        let effects = posted(&h);
        assert_eq!(effects.len(), 1, "{request}: {effects:?}");
        let Effect::Echo {
            level: EchoLevel::Warn,
            text,
        } = &effects[0]
        else {
            panic!("{request}: a warning, not the effect: {effects:?}");
        };
        assert!(text.contains(name), "{request}: {text}");
    }
}
