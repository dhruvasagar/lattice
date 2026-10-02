//! PH7.1a: the async ABI runs plugin CPU work on the caller's multi-thread
//! pool, so two busy plugins run on two cores at once.
//!
//! Its own test binary, deliberately. The assertion compares wall-clock time
//! against a single run, so it needs two cores nobody else is using. Cargo
//! runs test binaries one after another, but the tests *inside* a binary in
//! parallel, and in `runtime.rs` this shared the process with the
//! fuel-exhaustion test, whose plugin spins on two workers of its own. On a
//! loaded Windows runner the two busy plugins were time-sliced and measured
//! 231ms against a single run's 123ms. Alone, the cores are its own.

use std::sync::Arc;
use std::time::Instant;

use lattice_plugin_host::{PluginBudget, PluginHost};

const BUSY_WAT: &str = include_str!("fixtures/busy.wat");

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_busy_plugins_run_in_parallel() {
    // Needs >=2 cores; GitHub-hosted runners have 2-4.
    let host = Arc::new(PluginHost::new().expect("host builds"));
    let component = host
        .compile(&wat::parse_str(BUSY_WAT).expect("fixture WAT assembles to component bytes"))
        .expect("busy component compiles");
    // Budget generous enough for the 1e8-iteration loop (well above its fuel
    // draw) and a 60s epoch ceiling it never approaches.
    let budget = PluginBudget {
        fuel: 5_000_000_000,
        epoch_deadline: 60_000,
    };

    // Baseline: one plugin's activate.
    let single = {
        let t = Instant::now();
        let mut p = host
            .instantiate_with_budget(&component, budget)
            .await
            .expect("instantiates");
        p.activate().await.expect("busy activate completes");
        t.elapsed()
    };

    // Two plugins spawned onto the pool run their CPU loops on two workers.
    let parallel = {
        let t = Instant::now();
        let a = {
            let (host, component) = (host.clone(), component.clone());
            tokio::spawn(async move {
                let mut p = host
                    .instantiate_with_budget(&component, budget)
                    .await
                    .expect("instantiates");
                p.activate().await.expect("busy activate completes");
            })
        };
        let b = {
            let (host, component) = (host.clone(), component.clone());
            tokio::spawn(async move {
                let mut p = host
                    .instantiate_with_budget(&component, budget)
                    .await
                    .expect("instantiates");
                p.activate().await.expect("busy activate completes");
            })
        };
        a.await.expect("task a joins");
        b.await.expect("task b joins");
        t.elapsed()
    };

    // If the two ran serially, `parallel` would be ~2x `single`. Real overlap
    // keeps it well under. Loose threshold to absorb scheduling noise.
    assert!(
        parallel < single.mul_f64(1.8),
        "two busy plugins did not overlap: parallel={parallel:?} single={single:?} \
         (this assertion needs >=2 cores)",
    );
}
