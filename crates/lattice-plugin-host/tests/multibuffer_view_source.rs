//! MV.1 — the multibuffer-view seam, driven through a real guest.
//!
//! Design: `docs/dev/architecture/plugin-multibuffer-views.md`.
//!
//! These assert the *boundary*, not the view: that a guest can declare N views,
//! that `build` receives which view and which args, that a guest decline is
//! distinct from a host trap, and that a malformed spec costs the plugin only
//! that spec. The view itself — buffers, excerpts, headerline — is MV.1b's, in
//! `lattice-multibuffer`.
//!
//! Skips when the fixture was not built: `cargo test` builds this crate for the
//! HOST, and the guest is a separate `--target wasm32-wasip2 --release`
//! artefact.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::multibuffer_view_task::MultibufferViewClient;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

fn guest_wasm() -> Option<Vec<u8>> {
    std::fs::read(env!("VIEW_GUEST_WASM")).ok()
}

fn host_in(tmp: &tempfile::TempDir) -> PluginHost {
    PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).expect("host builds")
}

/// Spawn the fixture and drive its actor, returning the client.
async fn connect(host: &PluginHost) -> MultibufferViewClient {
    let bytes = guest_wasm().expect("caller checked");
    let component = host.compile(&bytes).expect("the fixture compiles");
    let manifest = PluginManifest::new("view-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_multibuffer_view_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());
    client
}

/// The registry property: ONE component declares SEVERAL views.
///
/// This is the whole reason the seam is registry-shaped rather than
/// "the component IS one view" — the shape `picker-source` had to be changed
/// out of the moment org wanted more than one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_component_declares_several_views() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: multibuffer_view_source — fixture guest not built");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = connect(&host).await;

    let specs = client.register_views().await.expect("registration crosses");
    let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["fixture-pull", "fixture-scan"]);
}

/// A spec with no id or no buffer name is refused, and the plugin keeps its
/// other views.
///
/// Both are names the host looks a view up by, so an empty one is unreachable
/// rather than merely odd. The fixture declares one deliberately, third of
/// three: dropping the whole contribution over it is the failure this asserts
/// against.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_spec_costs_only_itself() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = connect(&host).await;

    let specs = client.register_views().await.unwrap();
    assert_eq!(
        specs.len(),
        2,
        "the unnamed third view is dropped, the first two survive"
    );
    assert!(specs.iter().all(|s| !s.id.trim().is_empty()));
}

/// The spec's fields cross intact — including the two that make a view
/// *ownable*: its buffer name and its mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_views_identity_crosses() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = connect(&host).await;

    let specs = client.register_views().await.unwrap();
    let pull = specs.iter().find(|s| s.id == "fixture-pull").unwrap();
    assert_eq!(pull.buffer_name, "*fixture-pull*");
    assert_eq!(pull.view_mode.as_deref(), Some("fixture-view-mode"));
    assert!(pull.reuse);
    assert!(matches!(
        pull.input,
        lattice_plugin_host::lattice::plugin_host::types::MultibufferViewInput::Pull
    ));

    let scan = specs.iter().find(|s| s.id == "fixture-scan").unwrap();
    assert!(!scan.reuse, "a view may ask for a fresh buffer each time");
    match &scan.input {
        lattice_plugin_host::lattice::plugin_host::types::MultibufferViewInput::Scan(exts) => {
            assert_eq!(exts, &["txt".to_string()]);
        }
        other => panic!("expected a scan input, got {other:?}"),
    }
}

/// `build` is told WHICH view and WITH what — the inputs a view that serves
/// several purposes from one actor cannot work without.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_receives_the_view_and_its_args() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = connect(&host).await;
    let _ = client.register_views().await.unwrap();

    let result = client
        .build("fixture-pull".to_string(), vec!["x".into(), "y".into()])
        .await
        .expect("build reaches the guest")
        .expect("the guest did not decline");

    assert_eq!(result.excerpts.len(), 2);
    assert_eq!(result.excerpts[0].header, "view:fixture-pull");
    assert_eq!(result.excerpts[1].header, "args:x,y");
    assert_eq!(result.summary, "2 excerpts");
    // The excerpt's shape, which the provider turns into a real `Excerpt`.
    assert_eq!(result.excerpts[0].path, "a.txt");
    assert_eq!(result.excerpts[0].start_line, 0);
    assert_eq!(result.excerpts[0].end_line, 1);
    assert_eq!(result.excerpts[0].match_count, Some(2));
    assert_eq!(result.excerpts[1].match_count, None);
}

/// A guest DECLINE is a typed `err`, not a trap — and the distinction is the
/// difference between "nothing to show, here is why" and "this plugin is
/// broken". The actor stays usable afterwards, which is what proves it was not
/// a trap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decline_is_not_a_trap() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = connect(&host).await;
    let _ = client.register_views().await.unwrap();

    let declined = client
        .build("fixture-pull".to_string(), vec!["fail".into()])
        .await
        .expect("the host call itself succeeded");
    let message = declined.expect_err("the guest declined");
    assert!(
        message.contains("declined"),
        "the guest words its own refusal: {message:?}"
    );

    // Still alive: a decline must not quarantine the plugin.
    let after = client
        .build("fixture-pull".to_string(), vec!["ok".into()])
        .await
        .expect("actor still serving")
        .expect("and still building");
    assert_eq!(after.excerpts.len(), 2);
}
