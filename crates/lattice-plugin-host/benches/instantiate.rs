//! PH7.0/7.1a instantiation smoke bench.
//!
//! Two measurements: (1) instantiate a pre-compiled component — the
//! per-invocation cost the lazy-instantiation model (PH7.1b) pays on a
//! plugin's first contribution call; (2) compile + instantiate cold — the
//! AOT + instantiate path a fresh component takes before the on-disk module
//! cache (PH7.1b) exists. Neither is a gated budget yet; the cold-start and
//! per-call ratchets land at PH7.1b / PH7.5. This bench exists so the surface
//! is measured from day one (four-artefact discipline).
//!
//! Instantiation is async as of PH7.1a, so the bench drives it on a small
//! tokio runtime (the measured work is CPU-bound instantiation, not the
//! runtime).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_plugin_host::PluginHost;
use tokio::runtime::Runtime;

const NOOP_WAT: &str = include_str!("../tests/fixtures/noop.wat");

fn instantiate(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime builds");
    let bytes = wat::parse_str(NOOP_WAT).expect("no-op component WAT assembles");
    let host = PluginHost::new().expect("host builds");
    let component = host.compile(&bytes).expect("component compiles");

    c.bench_function("plugin_instantiate_noop", |b| {
        b.iter(|| {
            let plugin = rt
                .block_on(host.instantiate(black_box(&component)))
                .expect("instantiates");
            black_box(plugin);
        });
    });

    c.bench_function("plugin_compile_instantiate_noop", |b| {
        b.iter(|| {
            let component = host.compile(black_box(&bytes)).expect("compiles");
            let plugin = rt
                .block_on(host.instantiate(&component))
                .expect("instantiates");
            black_box(plugin);
        });
    });
}

/// A distinct valid lifecycle component per index (the index rides in an
/// unused global, so each has distinct bytes → a distinct cache key).
fn synthetic_component(i: usize) -> Vec<u8> {
    let wat = format!(
        "(component\n\
         \t(core module $m\n\
         \t\t(global $v i64 (i64.const {i}))\n\
         \t\t(func (export \"activate\"))\n\
         \t\t(func (export \"deactivate\")))\n\
         \t(core instance $inst (instantiate $m))\n\
         \t(func (export \"activate\") (canon lift (core func $inst \"activate\")))\n\
         \t(func (export \"deactivate\") (canon lift (core func $inst \"deactivate\"))))"
    );
    wat::parse_str(&wat).expect("synthetic component WAT assembles")
}

/// Cold-start budget surface (design.md §8 / plugin-host.md §7: 50 plugins <
/// 30ms). Not a gated ratchet yet — that lands at PH7.5. Measures loading 50
/// distinct plugins from a warm on-disk cache (all hits, no recompile) and
/// instantiating 50 plugins from one compiled component.
fn cold_start(c: &mut Criterion) {
    const N: usize = 50;
    let cache = tempfile::tempdir().expect("cache tempdir");
    let host = PluginHost::with_cache_dir(cache.path()).expect("host builds");
    let components: Vec<Vec<u8>> = (0..N).map(synthetic_component).collect();

    // Warm the cache: first compile of each is a miss that writes the artifact.
    for bytes in &components {
        host.compile(bytes).expect("warm compile");
    }

    c.bench_function("load_50_plugins_warm_cache", |b| {
        b.iter(|| {
            for bytes in &components {
                black_box(host.compile(bytes).expect("cached compile"));
            }
        });
    });

    let rt = Runtime::new().expect("tokio runtime builds");
    let component = host.compile(&components[0]).expect("compile one");
    c.bench_function("instantiate_50_plugins", |b| {
        b.iter(|| {
            rt.block_on(async {
                for _ in 0..N {
                    black_box(host.instantiate(&component).await.expect("instantiates"));
                }
            });
        });
    });
}

criterion_group!(benches, instantiate, cold_start);
criterion_main!(benches);
