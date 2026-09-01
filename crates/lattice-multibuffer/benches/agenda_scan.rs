//! OM.A1 (2026-08-25) bench: `agenda_scan_per_file_us`.
//!
//! `org-mode.md` §7 names "agenda scan throughput per file" as the third
//! benched path. It is off the keystroke path — but it is on a **producer's**
//! critical path, and a slow scan backs up the agenda the way a slow
//! `error-parser` backs up a build.
//!
//! What is measured is the HOST half: the `ignore::Walk`, the extension test,
//! the batched reads, the cross-file stable sort, and the excerpt build. The
//! producer is a native fake doing a trivial line scan, so the number tracks
//! the host's per-file cost rather than any one guest's parsing. The guest
//! round-trip itself is already ratcheted by the grammar gate (< 5 µs p99,
//! `perf_ratchet.rs`) and is independent of which guest answers.
//!
//! The corpus is synthetic: 200 files of ~40 lines under a tempdir, every
//! tenth line an agenda row, plus an equal number of unclaimed `.rs` files so
//! the "a file no source claims is never read" path is in the measurement
//! rather than benched away.

#![cfg(feature = "agenda")]

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_core::BufferId;
use lattice_mode::{AgendaBeginFuture, AgendaFuture, ScannedExcerpt, ScannedExcerptSource};
use lattice_multibuffer::providers::agenda::{AgendaOptions, spawn_agenda_scan};
use lattice_multibuffer::{
    HeaderlineStatus, InMemoryMultibufferRegistry, MultibufferDocumentHandle,
    MultibufferRegistryHandle,
};

const FILES: usize = 200;
const LINES_PER_FILE: usize = 40;

#[derive(Debug)]
struct BenchSource {
    exts: Vec<String>,
}

impl ScannedExcerptSource for BenchSource {
    fn source_id(&self) -> u64 {
        1
    }
    fn extensions(&self) -> &[String] {
        &self.exts
    }
    fn begin(&self, _args: &[String]) -> AgendaBeginFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn scan(&self, _path: PathBuf, text: String) -> AgendaFuture<'_> {
        Box::pin(async move {
            Ok(lattice_mode::ScanResult::rows(
                text.lines()
                    .enumerate()
                    .filter_map(|(i, line)| {
                        let rest = line.strip_prefix("* TODO ")?;
                        let key: i64 = rest.trim().parse().ok()?;
                        Some(ScannedExcerpt {
                            line: i as u32,
                            end_line: i as u32,
                            group: format!("day-{key}"),
                            label: format!("Day {key}"),
                            sort_key: key,
                            spans: Vec::new(),
                        })
                    })
                    .collect(),
            ))
        })
    }
}

fn corpus() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lattice-agenda-bench-{nanos}"));
    std::fs::create_dir_all(&dir).expect("bench corpus dir");
    for f in 0..FILES {
        let mut body = String::new();
        for l in 0..LINES_PER_FILE {
            if l % 10 == 0 {
                // Keys deliberately NOT in walk order, so the sort has work.
                body.push_str(&format!("* TODO {}\n", (f * 7 + l) % 365));
            } else {
                body.push_str("some prose about the thing\n");
            }
        }
        std::fs::write(dir.join(format!("notes-{f}.org")), &body).expect("write org");
        // Unclaimed, so the extension filter is in the measurement.
        std::fs::write(dir.join(format!("mod-{f}.rs")), "fn main() {}\n").expect("write rs");
    }
    dir
}

fn view(registry: &MultibufferRegistryHandle) -> BufferId {
    let handle = Arc::new(MultibufferDocumentHandle::empty(Arc::new(
        arc_swap::ArcSwap::from_pointee(lattice_grammar::CommandRegistry::new()),
    )));
    let id = handle.buffer_id();
    registry.insert(id, handle);
    id
}

fn bench_agenda_scan(c: &mut Criterion) {
    let dir = corpus();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("bench runtime");

    let mut group = c.benchmark_group("agenda_scan");
    group.sample_size(10);
    group.bench_function("scan_200_files", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let registry = InMemoryMultibufferRegistry::handle();
                let view_id = view(&registry);
                spawn_agenda_scan(
                    view_id,
                    AgendaOptions {
                        roots: vec![dir.clone()],
                        max_files: None,
                        // OA.11a: the default scan, which is what this bench
                        // has always measured — args parameterise the guest,
                        // not the walk, so they must not move this number.
                        scan_args: Vec::new(),
                    },
                    vec![Arc::new(BenchSource {
                        exts: vec!["org".to_string()],
                    })],
                    registry.clone(),
                    None,
                    // OA.5: no span sink in a bench — the scan
                    // publishes nothing and the walk is what is timed.
                    None,
                );
                loop {
                    if let Some(h) = registry.handle(view_id)
                        && matches!(*h.headerline(), HeaderlineStatus::Complete { .. })
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            });
        });
    });
    group.finish();

    let _ = std::fs::remove_dir_all(&dir);
}

criterion_group!(benches, bench_agenda_scan);
criterion_main!(benches);
