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

criterion_group!(benches, instantiate);
criterion_main!(benches);
