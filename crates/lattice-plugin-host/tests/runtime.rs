//! PH7.1a runtime-core coverage: the async ABI runs plugin CPU work on the
//! caller's multi-thread pool (two plugins → two cores), a runaway plugin
//! traps *cleanly* on its fuel budget without touching a concurrent
//! well-behaved plugin, and plugin work lands off the actor thread.

use std::sync::Arc;
use std::time::Instant;

use lattice_plugin_host::{PluginBudget, PluginHost, PluginHostError, TrapKind};

fn bytes(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("fixture WAT assembles to component bytes")
}

const NOOP_WAT: &str = include_str!("fixtures/noop.wat");
const BUSY_WAT: &str = include_str!("fixtures/busy.wat");
const SPIN_WAT: &str = include_str!("fixtures/spin.wat");

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_busy_plugins_run_in_parallel() {
    // Needs >=2 cores; GitHub-hosted runners have 2-4.
    let host = Arc::new(PluginHost::new().expect("host builds"));
    let component = host
        .compile(&bytes(BUSY_WAT))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuel_exhaustion_traps_cleanly_and_is_isolated() {
    let host = Arc::new(PluginHost::new().expect("host builds"));
    let spin = host.compile(&bytes(SPIN_WAT)).expect("spin compiles");
    let noop = host.compile(&bytes(NOOP_WAT)).expect("noop compiles");

    // Tiny fuel so the infinite loop trips the fuel trap almost immediately;
    // a huge epoch deadline so the trap is unambiguously *fuel*, not epoch.
    let tiny = PluginBudget {
        fuel: 200_000,
        epoch_deadline: 1_000_000,
    };

    let spin_task = {
        let (host, spin) = (host.clone(), spin.clone());
        tokio::spawn(async move {
            let mut p = host
                .instantiate_with_budget(&spin, tiny)
                .await
                .expect("spin instantiates");
            p.activate().await
        })
    };
    let noop_task = {
        let (host, noop) = (host.clone(), noop.clone());
        tokio::spawn(async move {
            let mut p = host.instantiate(&noop).await.expect("noop instantiates");
            p.activate().await
        })
    };

    let spin_res = spin_task.await.expect("spin task joins");
    let noop_res = noop_task.await.expect("noop task joins");

    assert!(
        matches!(
            spin_res,
            Err(PluginHostError::Trap {
                kind: TrapKind::Fuel,
                ..
            })
        ),
        "runaway plugin should trap on fuel, got {spin_res:?}",
    );
    assert!(
        noop_res.is_ok(),
        "the concurrent well-behaved plugin must be unaffected, got {noop_res:?}",
    );
}

#[test]
fn plugin_work_runs_off_the_actor_thread() {
    // The editor actor is a `current_thread` runtime pinned to one OS thread;
    // plugin work must never execute on it. Model that thread as this test's
    // thread, captured before any runtime exists.
    let actor_thread = std::thread::current().id();

    let plugin_pool = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("plugin pool builds");

    let exec_thread = plugin_pool.block_on(async {
        let host = PluginHost::new().expect("host builds");
        let component = host.compile(&bytes(NOOP_WAT)).expect("noop compiles");
        // Spawn onto the pool so the work runs on a worker OS thread.
        tokio::spawn(async move {
            let mut p = host.instantiate(&component).await.expect("instantiates");
            p.activate().await.expect("activate runs");
            std::thread::current().id()
        })
        .await
        .expect("worker task joins")
    });

    assert_ne!(
        exec_thread, actor_thread,
        "plugin work must land off the actor thread (ran on {exec_thread:?}, actor is {actor_thread:?})",
    );
}
