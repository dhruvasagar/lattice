use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_vcs::{Repository, WorkingTree};

fn bench_status(c: &mut Criterion) {
    c.bench_function("git_status_p99_us", |b| {
        b.iter(|| {
            let repo = Repository::discover(".").unwrap();
            black_box(WorkingTree::statuses(&repo).unwrap());
        });
    });
}

criterion_group!(benches, bench_status);
criterion_main!(benches);
