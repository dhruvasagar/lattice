//! PH7.1a runtime-core coverage: a runaway plugin traps *cleanly* on its
//! fuel budget without touching a concurrent well-behaved plugin, a busy
//! plugin does not hold up another one, and plugin work lands off the actor
//! thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lattice_plugin_host::{PluginBudget, PluginHost, PluginHostError, TrapKind};

fn bytes(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("fixture WAT assembles to component bytes")
}

const BUSY_WAT: &str = include_str!("fixtures/busy.wat");
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

/// The async ABI runs plugin CPU work on the caller's multi-thread pool, so
/// a plugin in the middle of a long call occupies one worker and no more: a
/// second plugin starts and finishes on another while the first is still
/// going.
///
/// This asserts the ORDER the two finish in, not how long either took. It
/// used to compare wall-clock time (two busy plugins together against one
/// alone, expecting under 1.8x), which measured the runner's cores as much
/// as the host: Windows CI reported 1.87x twice, once with the test alone in
/// its own binary. Ordering does not care how fast a core is or who else is
/// using it. A host that serialised plugin calls -- one executor thread, a
/// host-wide lock -- would finish the quick plugin second, and fail here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_plugin_does_not_hold_up_another() {
    let host = Arc::new(PluginHost::new().expect("host builds"));
    let busy = host.compile(&bytes(BUSY_WAT)).expect("busy compiles");
    let noop = host.compile(&bytes(NOOP_WAT)).expect("noop compiles");
    // Generous enough for the 1e8-iteration loop (well above its fuel draw)
    // and a 60s epoch ceiling it never approaches.
    let budget = PluginBudget {
        fuel: 5_000_000_000,
        epoch_deadline: 60_000,
    };

    let (entering, entered) = tokio::sync::oneshot::channel();
    let busy_done = Arc::new(AtomicBool::new(false));
    let busy_task = {
        let (host, busy_done) = (host.clone(), busy_done.clone());
        tokio::spawn(async move {
            let mut p = host
                .instantiate_with_budget(&busy, budget)
                .await
                .expect("busy instantiates");
            // Nothing awaits between this and the loop, so the quick plugin
            // below is only started once this worker is committed to it.
            entering.send(()).expect("the test is waiting");
            p.activate().await.expect("busy activate completes");
            busy_done.store(true, Ordering::SeqCst);
        })
    };
    entered.await.expect("busy task reaches its activate");

    let busy_was_done = {
        let (host, busy_done) = (host.clone(), busy_done.clone());
        tokio::spawn(async move {
            let mut p = host.instantiate(&noop).await.expect("noop instantiates");
            p.activate().await.expect("noop activate completes");
            busy_done.load(Ordering::SeqCst)
        })
        .await
        .expect("noop task joins")
    };
    busy_task.await.expect("busy task joins");

    assert!(
        !busy_was_done,
        "the quick plugin finished only after the busy one: plugin calls are being serialised",
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
