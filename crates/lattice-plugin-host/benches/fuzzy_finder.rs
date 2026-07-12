//! PH7.4d — the guest→host call-overhead bench (the §7 picker-path baseline,
//! mirroring the PH7.3d host→guest trampoline bench).
//!
//! Measures the WARM `fuzzy-finder` `init` through the `PickerClient` bridge:
//! the actor is spawned once, then `init` is called in a tight loop, so the
//! number is the per-call overhead of the whole picker path — the channel hop,
//! the guest export, the guest→host `walk` round-trip, and the candidate-pair
//! marshalling back. Not a CI-gated ratchet yet (that is PH7.5); this is the
//! descriptive baseline. Skips when the `wasm32-wasip2` plugin wasn't built
//! (see build.rs).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::picker_task::{ActiveBufferSnapshot, PickerContext, Position};
use lattice_plugin_host::{Capability, PluginBudget, PluginHost, PluginManifest, TrustTier};

/// A WIT `PickerContext` with the given workspace root (the guest reads only
/// `workspace_root` / `args`).
fn wit_ctx(workspace_root: &str) -> PickerContext {
    PickerContext {
        active_buffer: ActiveBufferSnapshot {
            buffer_id: 0,
            path: None,
            language: None,
            cursor: Position { line: 0, byte: 0 },
            selection: None,
            syntax_symbols: Vec::new(),
        },
        workspace_root: workspace_root.to_string(),
        recent_files: Vec::new(),
        position_history: Vec::new(),
        buffers: Vec::new(),
        marks: Vec::new(),
        registers: Vec::new(),
    }
}

fn init_warm(c: &mut Criterion) {
    let path = env!("FUZZY_FINDER_WASM");
    if path.is_empty() {
        eprintln!("SKIP: fuzzy_finder bench — plugin not built (add wasm32-wasip2)");
        return;
    }

    // A modest tree so the walk does real work without dominating the per-call
    // overhead we're characterising.
    let tree = tempfile::tempdir().unwrap();
    for i in 0..50 {
        std::fs::write(tree.path().join(format!("f{i}.rs")), "").unwrap();
    }
    let root = std::fs::canonicalize(tree.path()).unwrap();
    let root_str = root.to_str().unwrap().to_string();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Keep the host's cache/data dirs alive for the bench's duration.
    let dirs = tempfile::tempdir().unwrap();
    let (client, actor, host) = rt.block_on(async {
        let host =
            PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
        let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
        let manifest = PluginManifest::new(
            "fuzzy-finder",
            vec![Capability::FsRead(root.clone())],
            CapabilitySet::empty(),
        );
        let (client, actor) = host
            .spawn_picker_source(
                &component,
                &manifest,
                TrustTier::Bundled,
                PluginBudget::default(),
            )
            .await
            .unwrap();
        (client, actor, host)
    });
    // Drive the actor for the bench's duration; leak the host so its engine +
    // epoch ticker outlive it (the actor task holds the Store).
    rt.spawn(actor.run());
    Box::leak(Box::new(host));

    let ctx = wit_ctx(&root_str);
    c.bench_function("fuzzy_finder_init_warm_50_files", |b| {
        b.iter(|| {
            let out = rt
                .block_on(client.init(black_box(ctx.clone()), black_box(vec![root_str.clone()])))
                .expect("init reaches the guest")
                .expect("guest produced candidates");
            black_box(out);
        })
    });
}

criterion_group!(benches, init_warm);
criterion_main!(benches);
