//! M.6.X (2026-06-01) — UI-responsiveness retrofit test.
//!
//! Proves the property paramount-goal-1 promises: that
//! `:search` does not block the editor's command dispatch.
//!
//! Failure mode this catches (M.6.0 → M.6.3 shipped with it
//! latent): `run_scan` walked `ignore::Walk` synchronously
//! inside an `async fn` that was spawned onto the editor
//! actor's `current_thread` tokio runtime. The actor's
//! command loop got starved between yields, freezing the
//! editor for the duration of the scan. The fix
//! (`tokio::task::spawn_blocking`) moves the fs work onto
//! tokio's blocking pool, leaving the current_thread runtime
//! free.
//!
//! This test mirrors `editor_actor.rs:575`'s runtime
//! configuration (current_thread) and asserts that an
//! on-runtime `tokio::time::sleep` retains its budget while
//! a 500-file scan runs concurrently. Pre-fix: probe gaps
//! grow into hundreds of ms (the freeze). Post-fix: gaps
//! stay within a few ms of the sleep budget plus scheduling
//! jitter. Threshold is set generously (50 ms) so debug-mode
//! CI noise doesn't flake the test; the failure-mode signal
//! is orders of magnitude above this floor.
//!
//! Together with the criterion bench
//! `benches/actor_latency_during_scan.rs`, this is the
//! bench+test pair that should accompany every provider that
//! touches fs / net / blocking work — see memory
//! `feedback_no_ui_thread_work`.

#![cfg(feature = "search")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_core::BufferId;
use lattice_multibuffer::providers::search::{
    InMemoryProjectSearchService, ProjectSearchOptions, ProjectSearchServiceHandle,
    ProjectSearchState, spawn_scan_task,
};
use lattice_runtime::EventBus;

/// Build a unique temp directory with `n` small files, each
/// containing the literal "needle". Returns the root path and
/// a `Drop` guard that removes the directory.
fn synth_corpus(n: usize) -> (PathBuf, impl Drop) {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("lattice-m6x-probe-{pid}-{nanos}"));
    std::fs::create_dir_all(&root).expect("create temp corpus dir");
    for i in 0..n {
        let path = root.join(format!("file_{i:04}.txt"));
        std::fs::write(&path, "line one\nneedle here\nline three\n")
            .expect("write probe file");
    }

    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    (root.clone(), Guard(root))
}

#[test]
fn actor_runtime_stays_responsive_during_scan() {
    let (root, _guard) = synth_corpus(500);

    // Mirror the editor actor's runtime configuration
    // (`editor_actor.rs:575` -> `Builder::new_current_thread()`).
    // The whole point of this test is that the production
    // runtime config does not starve.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current_thread runtime");

    let service: ProjectSearchServiceHandle = Arc::new(InMemoryProjectSearchService::new());
    let events = Arc::new(EventBus::default());
    let view = BufferId(1);
    let options = ProjectSearchOptions {
        root: root.clone(),
        case_sensitive: true,
        max_files: None,
        max_hits_per_file: 100,
        regex: false,
    };
    service.set_state(
        view,
        ProjectSearchState::scanning("needle".to_string(), options.clone()),
    );

    let (max_gap, ticks) = rt.block_on(async move {
        let scan = spawn_scan_task(
            view,
            "needle".to_string(),
            options,
            service.clone(),
            events.clone(),
        );

        // Probe: sleep in a tight loop, measure realised
        // gap. Each sleep yields the runtime; if the
        // current_thread is starved by a blocking scan,
        // these gaps balloon. Post-fix they stay tight.
        let probe_iters = 50;
        let sleep_budget = Duration::from_millis(5);
        let mut max_gap = Duration::ZERO;
        let mut ticks = 0usize;
        let mut last = Instant::now();
        for _ in 0..probe_iters {
            tokio::time::sleep(sleep_budget).await;
            let now = Instant::now();
            let actual = now.duration_since(last);
            if actual > max_gap {
                max_gap = actual;
            }
            ticks += 1;
            last = now;
        }

        // Let the scan finish so its drop tidies up cleanly.
        let _ = scan.await;
        (max_gap, ticks)
    });

    assert_eq!(ticks, 50, "all probe iterations ran");

    // Generous threshold: 5 ms sleep + debug-mode tokio
    // scheduling jitter. The pre-fix freeze produces gaps
    // in the hundreds of ms range — orders of magnitude
    // above this floor. If a future change starves the
    // runtime again, this test fails loudly.
    assert!(
        max_gap < Duration::from_millis(50),
        "max probe gap was {max_gap:?}; expected < 50 ms — \
         current_thread runtime is being starved during scan \
         (paramount-goal-1 regression). \
         See `feedback_no_ui_thread_work` and \
         `slice-plans/multibuffer-views.md` M.6.X retro."
    );
}
