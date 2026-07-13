//! PH7.6 — a WASM completion source, end-to-end through the async-produce path.
//!
//! Drives the `completion-guest` fixture through the `CompletionActor` +
//! `WasmCompletionSource` adapter: `spec` (the source id/doc), then `generate`
//! (async produce) yielding native `RawCandidate`s — proving the plugin seam +
//! the boundary crossing (incl. the `candidate-data.extension` hatch). Then it
//! feeds those candidates through the NATIVE `match_and_rank` (the option-A
//! design — matching stays native), asserting the fuzzy matcher filters to the
//! `"al"`-matching keywords.
//!
//! Validation only: the plugin isn't registered in the shipping editor. Skips
//! when the `wasm32-wasip2` guest wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_completion::candidate::CandidateData;
use lattice_completion::{CompletionPipeline, CompletionRegistry, populate};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{
    PluginBudget, PluginHost, PluginManifest, TrustTier, WasmCompletionSource,
};
use tempfile::TempDir;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("COMPLETION_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn host_in(dir: &TempDir) -> PluginHost {
    PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs")
}

/// Connect a `WasmCompletionSource` over a freshly-spawned fixture actor.
async fn connect(host: &PluginHost) -> WasmCompletionSource {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .unwrap();
    // No capabilities — the fixture produces a fixed set, walks nothing.
    let manifest = PluginManifest::new("completion-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_completion_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());
    WasmCompletionSource::connect(client)
        .await
        .expect("connect fetches + converts the spec")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_completion_source_produces_candidates_and_native_match_ranks_them() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: completion_source — fixture guest not built (add wasm32-wasip2)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let source = connect(&host).await;

    assert_eq!(source.id(), "keywords");
    assert!(source.doc().contains("keyword"));

    // Async produce (the generator export). The fixture returns the full set;
    // matching is native.
    let raw = source
        .generate("al", false)
        .await
        .expect("generate reaches the guest + produces candidates");
    let mut texts: Vec<&str> = raw.iter().map(|c| c.text.as_str()).collect();
    texts.sort_unstable();
    assert_eq!(texts, ["alpha", "alphabet", "beta", "gamma"]);
    // The plugin candidate-data rides the `extension` hatch.
    assert!(
        raw.iter()
            .all(|c| matches!(c.data, CandidateData::Extension { .. })),
        "plugin candidates carry CandidateData::Extension"
    );

    // Feed the produced candidates through the NATIVE pipeline (option A). The
    // generator in the pipeline is irrelevant — `match_and_rank` takes the raw
    // set directly; we just need the default fuzzy matcher + score ranker.
    let mut registry = CompletionRegistry::new();
    let builtins = populate(&mut registry);
    let pipeline = CompletionPipeline::for_generator(&registry, builtins.gen_commands)
        .expect("default matcher + rankers are populated");
    let rendered = pipeline.match_and_rank("al", &raw);

    let mut matched: Vec<&str> = rendered.iter().map(|c| c.raw.text.as_str()).collect();
    matched.sort_unstable();
    assert_eq!(
        matched,
        ["alpha", "alphabet"],
        "native fuzzy matcher keeps only the `al`-matching keywords from the plugin's set"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_source_calls_after_the_actor_ends_are_typed_plugin_gone() {
    use lattice_plugin_host::PluginHostError;
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .unwrap();
    let manifest = PluginManifest::new("completion-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_completion_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .unwrap();
    let handle = tokio::spawn(actor.run());
    handle.abort();
    let _ = handle.await;

    match client.spec().await {
        Err(PluginHostError::PluginGone { func }) => assert_eq!(func, "spec"),
        other => panic!("expected PluginGone, got {other:?}"),
    }
}
