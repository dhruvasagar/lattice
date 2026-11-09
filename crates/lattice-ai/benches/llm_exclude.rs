//! GR.2 -- `llm.exclude` matching cost.
//!
//! Every read-tool call, every listed or searched file and every captured span
//! is checked against the exclude list, some of them on the editor thread (an
//! ask's capture). The list is compiled once per change (`PathRules`); this
//! measures one check against the default list, for a path inside the project
//! that matches nothing (the common case: the whole list is walked) and one
//! that matches.

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_ai::llm::options::DEFAULT_EXCLUDE;
use lattice_ai::llm::path_rules::PathRules;
use std::hint::black_box;
use std::path::Path;

fn bench(c: &mut Criterion) {
    let root = Path::new("/home/u/project");
    let patterns: Vec<String> = DEFAULT_EXCLUDE.iter().map(|p| p.to_string()).collect();
    let rules = PathRules::new(root, &patterns).expect("the default list compiles");
    let miss = root.join("crates/lattice-ai/src/llm/tools/lattice.rs");
    let hit = root.join("deploy/prod/.env");
    let mut group = c.benchmark_group("llm_exclude");
    group.bench_function("miss_default_list", |b| {
        b.iter(|| black_box(rules.matching(black_box(&miss))))
    });
    group.bench_function("hit_default_list", |b| {
        b.iter(|| black_box(rules.matching(black_box(&hit))))
    });
    group.bench_function("compile_default_list", |b| {
        b.iter(|| black_box(PathRules::new(root, black_box(&patterns))))
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
