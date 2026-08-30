//! PH7.6 — the completion `generate` call-overhead bench.
//!
//! Warm `generate` through the `CompletionClient` bridge: the channel hop + the
//! guest export + the candidate-pair marshalling back (no `walk` — the fixture
//! produces a fixed keyword set, so this isolates the bridge + produce cost, vs.
//! the fuzzy-finder bench which also pays the fs walk). Descriptive baseline;
//! the picker/typed-call ratchets (PH7.5) already gate the shared bridge path.
//! Skips when the `wasm32-wasip2` plugin wasn't built (see build.rs).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::completion_task::GenerateContext;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

fn ctx() -> GenerateContext {
    GenerateContext {
        prefix: "al".to_string(),
        case_sensitive: false,
        line_before_cursor: "al".to_string(),
        language: "rust".to_string(),
    }
}

fn generate_warm(c: &mut Criterion) {
    let path = env!("COMPLETION_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: completion bench — plugin not built (add wasm32-wasip2)");
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
        let manifest =
            PluginManifest::new("completion-fixture", Vec::new(), CapabilitySet::empty());
        let (client, actor) = host
            .spawn_completion_source(
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
    // Leak the host so its engine + epoch ticker outlive the bench.
    Box::leak(Box::new(host));

    let ctx = ctx();
    c.bench_function("completion_generate_warm", |b| {
        b.iter(|| {
            let out = rt
                .block_on(client.generate(black_box(ctx.clone())))
                .expect("generate reaches the guest")
                .expect("guest produced candidates");
            black_box(out);
        })
    });
}

criterion_group!(benches, generate_warm);
criterion_main!(benches);
