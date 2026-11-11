//! AT.5c — a plugin tool's call round trip.
//!
//! Warm `call` through the `LlmToolsClient` bridge: the channel hop to the
//! plugin's actor, the guest export, and the result back. That is the whole
//! cost the plugin seam adds to a model tool call, which itself waits on the
//! model for hundreds of milliseconds, so this is descriptive rather than a
//! ratchet: it shows the seam is noise beside the request, and catches the
//! bridge growing a cost it should not have. Skips when the `wasm32-wasip2`
//! fixture wasn't built (see build.rs).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

fn call_warm(c: &mut Criterion) {
    let path = env!("LLM_TOOLS_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: llm-tools bench — fixture not built (add wasm32-wasip2)");
        return;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let dirs = tempfile::tempdir().unwrap();
    let (client, actor, host) = rt.block_on(async {
        let host =
            PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
        let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
        let manifest = PluginManifest::new("fixture", Vec::new(), CapabilitySet::empty());
        let (client, actor) = host
            .spawn_llm_tools(
                &component,
                &manifest,
                TrustTier::Bundled,
                PluginBudget::default(),
                &std::sync::Arc::new(lattice_runtime::EventBus::new()),
                None,
            )
            .await
            .unwrap();
        (client, actor, host)
    });
    rt.spawn(actor.run());
    // Leak the host so its engine and epoch ticker outlive the bench.
    Box::leak(Box::new(host));

    let args = r#"{"q":"work"}"#.to_string();
    c.bench_function("llm_tools_call_warm", |b| {
        b.iter(|| {
            let out = rt
                .block_on(client.call("outline".to_string(), black_box(args.clone())))
                .expect("call reaches the guest")
                .expect("the guest answers");
            black_box(out);
        })
    });
}

criterion_group!(benches, call_warm);
criterion_main!(benches);
