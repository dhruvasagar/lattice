//! M.6.X (2026-06-01) — actor-latency-during-scan bench.
//!
//! Companion to `tests/ui_responsive_during_scan.rs`. That
//! test asserts a pass/fail threshold; this bench reports
//! the actual p99 probe-gap on a current_thread runtime
//! while a real `spawn_scan_task` runs against a 1k-file
//! synthetic corpus. CI tracks the number so a regression
//! is visible as a perf-budget breach, not just a binary
//! test failure.
//!
//! Paramount-goal-1 says keystroke → glyph within the one-frame ceiling (≤ 8.3 ms at 120 Hz).
//! The probe gap measured here is the worst-case latency a
//! command queued on the actor's `cmd_rx` would experience
//! at the runtime layer (does not include actor processing
//! cost — that's the existing per-command benches).
//!
//! Pre-M.6.X: probe gaps blow past 100 ms because the
//! current_thread runtime is starved by the sync walker.
//! Post-M.6.X: probe gaps stay within sleep budget +
//! tokio scheduling jitter.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};

use lattice_core::BufferId;
use lattice_multibuffer::providers::search::{
    InMemoryProjectSearchService, ProjectSearchOptions, ProjectSearchServiceHandle,
    ProjectSearchState, spawn_scan_task,
};
use lattice_runtime::EventBus;

fn synth_corpus(n: usize) -> (PathBuf, CorpusGuard) {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("lattice-m6x-bench-{pid}-{nanos}"));
    std::fs::create_dir_all(&root).expect("create bench corpus dir");
    for i in 0..n {
        let path = root.join(format!("file_{i:04}.txt"));
        std::fs::write(&path, "line one\nneedle here\nline three\n")
            .expect("write bench file");
    }
    (root.clone(), CorpusGuard(root))
}

struct CorpusGuard(PathBuf);

impl Drop for CorpusGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One iteration: spin up a fresh current_thread runtime,
/// launch the scan, run a probe loop, return the max gap.
fn one_iter(root: &PathBuf) -> Duration {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current_thread runtime");

    let service: ProjectSearchServiceHandle = Arc::new(InMemoryProjectSearchService::new());
    let events = Arc::new(EventBus::default());
    let view = BufferId(1);
    let options = ProjectSearchOptions {
        root: root.clone(),
        case_sensitive: true,
        max_files: None,
        max_hits_per_file: 100,
        regex: false,
        context_lines: 0,
    };
    service.set_state(
        view,
        ProjectSearchState::scanning("needle".to_string(), options.clone()),
    );

    rt.block_on(async move {
        let scan = spawn_scan_task(
            view,
            "needle".to_string(),
            options,
            service.clone(),
            events.clone(),
        );

        let probe_iters = 100;
        let sleep_budget = Duration::from_millis(5);
        let mut max_gap = Duration::ZERO;
        let mut last = Instant::now();
        for _ in 0..probe_iters {
            tokio::time::sleep(sleep_budget).await;
            let now = Instant::now();
            let actual = now.duration_since(last);
            if actual > max_gap {
                max_gap = actual;
            }
            last = now;
        }
        let _ = scan.await;
        max_gap
    })
}

fn bench_actor_latency_during_scan(c: &mut Criterion) {
    let (root, _guard) = synth_corpus(1000);
    c.bench_function("actor_max_probe_gap_ms_during_1k_scan", |b| {
        b.iter_custom(|iters| {
            // Bench framework expects a total duration across
            // `iters` runs; we use the *worst observed* gap
            // per iteration as the metric, so the number
            // criterion reports tracks the p100 of probe-
            // gap across the sampling window (criterion
            // turns that into its own stats).
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += one_iter(&root);
            }
            total
        })
    });
}

criterion_group!(benches, bench_actor_latency_during_scan);
criterion_main!(benches);
