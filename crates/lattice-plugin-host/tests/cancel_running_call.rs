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

    // CG.12: the call snapshots the interrupt token as it starts; nothing
    // needs arming. Instantiate FIRST, then press.
    let mut plugin = host
        .instantiate_with_budget(&spin, unbounded_ish())
        .await
        .expect("spin instantiates");

    // Pressed REPEATEDLY until the call returns. A single press can land
    // before `activate` begins on a busy machine; the call then snapshots
    // the fresh token and, by design, runs to its budget — ~16 minutes of
    // one core and (cargo stops at the first failing binary) every later
    // test binary unrun. Seen three times in one day under load with the
    // single press this used to make.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let firing = {
        let (cancel, done) = (cancel.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(50));
                cancel.cancel();
            }
        })
    };

    let started = Instant::now();
    let err = plugin
        .activate()
        .await
        .expect_err("the spin loop must trap");
    let elapsed = started.elapsed();
    done.store(true, std::sync::atomic::Ordering::Release);
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

/// CG.12: only `<C-g>` pressed WHILE a call runs reaches it. A press before
/// it started does not — the interrupt token was replaced — and no task
/// beginning or superseding during the call cancels it. Before CG.12 a call
/// adopted whatever was armed, so a new project search killed it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_press_during_the_call_reaches_it() {
    let (host, cancel, _dirs) = host_with_cancel();
    let spin = host.compile(SPIN_WAT.as_bytes()).unwrap();

    // A short epoch budget so the test finishes: the point is WHICH
    // outcome, not how long.
    let budget = PluginBudget {
        fuel: u64::MAX,
        epoch_deadline: 50,
    };
    // Pressed before the call: it must not carry over.
    cancel.cancel();

    let firing = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            // Tasks starting and superseding while the call runs.
            let _a = cancel
                .begin(lattice_mode::TaskKey::supersede("search", "view").with_inputs("a"))
                .unwrap();
            let _b = cancel
                .begin(lattice_mode::TaskKey::supersede("search", "view").with_inputs("b"))
                .unwrap();
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
        "neither an earlier press nor other tasks reach a running call; it \
         ends on its own budget. Got: {err}"
    );
}
