//! PH7.1a runtime-core coverage: a runaway plugin traps *cleanly* on its
//! fuel budget without touching a concurrent well-behaved plugin, and plugin
//! work lands off the actor thread. Two-plugins-on-two-cores lives in
//! `runtime_parallel.rs`, alone, because it measures wall-clock time.

use std::sync::Arc;

use lattice_plugin_host::{PluginBudget, PluginHost, PluginHostError, TrapKind};

fn bytes(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("fixture WAT assembles to component bytes")
}

const NOOP_WAT: &str = include_str!("fixtures/noop.wat");
const SPIN_WAT: &str = include_str!("fixtures/spin.wat");

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
