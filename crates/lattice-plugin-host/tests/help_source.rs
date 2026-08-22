//! CR.3 — the plugin-contributed `:help` seam, through a real guest.
//!
//! The fixture bakes two markdown files into its own component with
//! `include_str!` and declares four topics: one under an empty name, one
//! ordinary, one deliberately colliding with a builtin topic name, and one
//! with an empty body. What each proves is documented on the test.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::help_host::HelpTopicSpec;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

const PLUGIN_ID: &str = "help-guest";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("HELP_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

async fn topics(dir: &TempDir) -> Option<Vec<HelpTopicSpec>> {
    let wasm = guest_wasm()?;
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile help fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let (_id, specs) = host
        .spawn_help_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
        )
        .await
        .expect("spawn help plugin");
    Some(specs)
}

fn find<'a>(specs: &'a [HelpTopicSpec], name: &str) -> Option<&'a HelpTopicSpec> {
    specs.iter().find(|s| s.name == name)
}

/// The seam's premise: markdown compiled INTO the component arrives intact on
/// the host side, with no filesystem involved anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugins_embedded_markdown_crosses_the_boundary() {
    let dir = TempDir::new().unwrap();
    let Some(specs) = topics(&dir).await else {
        eprintln!("SKIP: help fixture guest not built");
        return;
    };

    let usage = find(&specs, "help-guest.usage").expect("the usage topic registered");
    assert!(
        usage.body.contains("ships **inside**") || usage.body.contains("inside"),
        "the body must be the fixture's real markdown, got: {:?}",
        &usage.body[..usage.body.len().min(80)]
    );
    assert!(
        usage.body.contains("](help:index)"),
        "cross-links to builtin topics survive the crossing verbatim"
    );
    assert_eq!(usage.summary, "How the help-guest fixture ships its docs.");
    assert_eq!(
        usage.related_command_patterns,
        vec!["help-guest".to_string()]
    );
}

/// A one-page plugin answers to `:help <plugin>`, not `:help <plugin>.<plugin>`.
/// This is the refinement on plain prefixing, and the only thing that keeps the
/// namespaced surface looking like vim's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unnamed_topic_lands_at_the_bare_plugin_id() {
    let dir = TempDir::new().unwrap();
    let Some(specs) = topics(&dir).await else {
        eprintln!("SKIP: help fixture guest not built");
        return;
    };

    assert!(
        find(&specs, "help-guest").is_some(),
        "an empty name must land at the bare id, got: {:?}",
        specs.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert!(
        find(&specs, "help-guest.help-guest").is_none(),
        "the bare-id case must not double up the prefix"
    );
}

/// The property namespacing exists for. The fixture registers `buffers`, which
/// is a real builtin topic; the host must have turned it into
/// `help-guest.buffers`, so a plugin cannot shadow a core page even when it
/// tries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_cannot_shadow_a_builtin_topic() {
    let dir = TempDir::new().unwrap();
    let Some(specs) = topics(&dir).await else {
        eprintln!("SKIP: help fixture guest not built");
        return;
    };

    assert!(
        find(&specs, "buffers").is_none(),
        "a guest must not be able to claim a bare builtin name"
    );
    let impostor = find(&specs, "help-guest.buffers").expect("namespaced instead");
    assert!(impostor.body.contains("Impostor"));

    // And the builtin is genuinely a real topic, so the collision this guards
    // against is not hypothetical — a fixture colliding with a name nothing
    // uses would pass against a broken namespace too.
    let builtins = lattice_help::topics::builtin_topics();
    assert!(
        builtins.lookup("buffers").is_some(),
        "`buffers` must be a real builtin for this test to mean anything"
    );
}

/// A malformed topic costs itself and nothing else: the load succeeds and the
/// plugin's other pages register. The fixture's fourth topic has an empty body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_topic_does_not_cost_the_load_or_its_siblings() {
    let dir = TempDir::new().unwrap();
    let Some(specs) = topics(&dir).await else {
        eprintln!("SKIP: help fixture guest not built");
        return;
    };

    assert!(
        find(&specs, "help-guest.empty").is_none(),
        "an empty body is rejected host-side"
    );
    assert_eq!(
        specs.len(),
        3,
        "the other three topics registered: {:?}",
        specs.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}
