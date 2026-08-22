//! CG.4 — `<C-g>` interrupts a guest call **mid-flight**.
//!
//! Design: `docs/dev/architecture/cancellation.md`. Before CG.4 a running
//! plugin call could only be stopped by its own budget: fuel exhaustion
//! or the epoch deadline. A user pressing `<C-g>` on a slow plugin
//! waited for the budget, up to a second.
//!
//! Uses the `spin` fixture — a WAT component whose `activate` loops
//! forever — with a **huge fuel budget**, so the only thing that can end
//! the call is time or cancellation. That inversion is the test: the
//! sibling test in `runtime.rs` gives spin a tiny fuel budget and a huge
//! epoch to prove fuel traps; this one does the reverse.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_mode::{ForegroundCancel, ForegroundCancelHandle};
use lattice_plugin_host::{PluginBudget, PluginHost, TrapKind};

const SPIN_WAT: &str = include_str!("fixtures/spin.wat");

/// Fuel high enough that the loop cannot exhaust it before the test's
/// timing window closes; epoch high enough that the deadline is not what
/// ends the call. Whatever stops it, it is not the budget.
fn unbounded_ish() -> PluginBudget {
    PluginBudget {
        fuel: u64::MAX,
        epoch_deadline: 1_000_000,
    }
}

fn host_with_cancel() -> (PluginHost, ForegroundCancelHandle, tempfile::TempDir) {
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let cancel: ForegroundCancelHandle = Arc::new(ForegroundCancel::default());
    host.set_foreground_cancel(cancel.clone());
    (host, cancel, dirs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_guest_call_is_cancelled_mid_flight() {
    let (host, cancel, _dirs) = host_with_cancel();
    let spin = host.compile(SPIN_WAT.as_bytes()).unwrap();

    // Armed BEFORE the call: `arm_store` snapshots what is armed when the
    // call starts.
    let _token = cancel.arm();

    let firing = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        })
    };

    let started = Instant::now();
    let mut plugin = host
        .instantiate_with_budget(&spin, unbounded_ish())
        .await
        .expect("spin instantiates");
    let err = plugin
        .activate()
        .await
        .expect_err("the spin loop must trap");
    let elapsed = started.elapsed();
    firing.join().unwrap();

    assert!(
        matches!(
            &err,
            lattice_plugin_host::PluginHostError::Trap {
                kind: TrapKind::Cancelled,
                ..
            }
        ),
        "a cancelled call reports Cancelled, not Epoch — the user stopped \
         it, the plugin did not misbehave. Got: {err}"
    );
    // Generous, like the sibling ratchets: the claim is "promptly", not a
    // timing assertion that flaps on a loaded runner. Without CG.4 this
    // would spin for the full 1_000_000-tick epoch budget (~16 minutes),
    // so any sane ceiling discriminates.
    assert!(
        elapsed < Duration::from_secs(10),
        "cancellation should land within a tick or two of the cancel, took {elapsed:?}"
    );
}

/// A call started when nothing is armed is not cancellable, and must not
/// be affected by a *later* arm+cancel belonging to a different
/// operation. The snapshot in `arm_store` is what guarantees this.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_started_with_nothing_armed_runs_to_its_budget() {
    let (host, cancel, _dirs) = host_with_cancel();
    let spin = host.compile(SPIN_WAT.as_bytes()).unwrap();

    // Nothing armed. A short epoch budget so the test finishes: the point
    // is WHICH outcome, not how long.
    let budget = PluginBudget {
        fuel: u64::MAX,
        epoch_deadline: 50,
    };

    let firing = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            // Arms and cancels AFTER the call is already running.
            let _t = cancel.arm();
            cancel.cancel();
        })
    };

    let mut plugin = host
        .instantiate_with_budget(&spin, budget)
        .await
        .expect("spin instantiates");
    let err = plugin
        .activate()
        .await
        .expect_err("the spin loop must trap");
    firing.join().unwrap();

    assert!(
        matches!(
            &err,
            lattice_plugin_host::PluginHostError::Trap {
                kind: TrapKind::Epoch,
                ..
            }
        ),
        "an operation armed after this call started must not reach back \
         into it; the call ends on its own budget. Got: {err}"
    );
}
