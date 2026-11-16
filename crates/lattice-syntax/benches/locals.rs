#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion bench for what compiling `locals.scm` costs (RF.4h).
//!
//! The registry compiles its other queries once per process, at boot
//! (`LangRegistry::standard`, ~1.2 s for the whole set). Compiled the same
//! way, the ten locals queries would add the `all_ten` figure to every
//! startup. That number is why they compile on first use instead
//! (`LazyQuery`), and the per-language figures are what the first
//! refactoring in a language pays, once, on the plugin's thread. Running
//! the query is per-keypress work over one statement, so it is not measured
//! here.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_syntax::locals;
use lattice_syntax::registry::LangRegistry;
use tree_sitter::Query;

fn bench_locals_compile(c: &mut Criterion) {
    let registry = LangRegistry::standard().unwrap();
    let languages: Vec<_> = locals::LANGUAGES
        .iter()
        .map(|name| {
            (
                *name,
                registry.tree_sitter_language(name).unwrap(),
                locals::source(name).unwrap(),
            )
        })
        .collect();

    let mut group = c.benchmark_group("locals_compile");
    for (name, language, source) in &languages {
        group.bench_function(*name, |b| {
            b.iter(|| Query::new(black_box(language), black_box(source)).unwrap())
        });
    }
    group.bench_function("all_ten", |b| {
        b.iter(|| {
            for (_, language, source) in &languages {
                black_box(Query::new(language, source).unwrap());
            }
        })
    });
    group.finish();
}

criterion_group!(benches, bench_locals_compile);
criterion_main!(benches);
