//! LH.0 — host jobs: the shape every long-running host-service takes.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.0.
//!
//! A host-service whose work outlasts a guest call — a download, an unpack, a
//! subprocess — does not return when the work is done. It validates, returns an
//! **id**, and runs on a thread of its own; the guest hears the rest as
//! `Event::JobProgress` (and `Event::JobOutput`, for work that prints) and
//! exactly one `Event::JobFinished`, addressed to
//! the plugin that asked. This module is that shape, once, so each such seam is
//! only its own work.
//!
//! ## Why the events are generic
//!
//! One `job-finished` for every kind of job, rather than a `download-finished`
//! and an `extract-finished` and so on. An arm added to the WIT `event` variant
//! is an ABI break — every guest matching on it stops compiling, and the
//! package version has to move — so a seam that brought its own arms would cost
//! a generation each. A guest keys its state by id and already knows what it
//! started; the kind would tell it nothing.
//!
//! ## Two lifetimes, on purpose
//!
//! The [`JobGuard`] lives on the `PluginState` that started the job and cancels
//! it on drop — unload and quarantine stop the work with no teardown wiring to
//! forget, as they stop a watch. Cancelling *by id* goes through the
//! process-wide [`ACTIVE`] table instead, because the instance that cancels (a
//! chord, on the grammar seam) is routinely not the instance that started the
//! job; each seam is its own `Store`.
//!
//! ## Cancellation is cooperative
//!
//! A flag the work polls ([`Job::check_cancelled`]) between units of work. It
//! cannot interrupt a blocking call in progress, so a job's responsiveness to a
//! cancel is its own longest blocking step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lattice_protocol::Event as NativeEvent;
use lattice_runtime::EventBus;

/// Quiet interval between [`NativeEvent::JobProgress`] deliveries for one job.
///
/// Ten a second: a progress line that visibly moves, and a guest called a
/// bounded number of times however fast the work goes.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// What a cancelled job reports. One spelling, so a guest can recognise it.
pub(crate) const CANCELLED: &str = "cancelled";

/// Host-global, because the instance that hears about a job is usually not the
/// one that started it, and two instances each handing out `1` would make the
/// id meaningless to both.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Running jobs, by id: the plugin that owns each and its cancel flag.
type ActiveJobs = HashMap<u64, (u32, Arc<AtomicBool>)>;

/// An entry exists from [`PendingJob::start`] until the job publishes its
/// outcome.
static ACTIVE: LazyLock<Mutex<ActiveJobs>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The table, surviving a poisoned lock: it holds flags and ids, nothing a
/// panicking holder could have left half-written.
fn active() -> MutexGuard<'static, ActiveJobs> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The work a job does, handed its own [`Job`] to report and poll through.
type Work = Box<dyn FnOnce(&mut Job) -> Result<(), String> + Send>;

/// A running job, as its work sees it.
pub(crate) struct Job {
    id: u64,
    plugin: u32,
    bus: Arc<EventBus>,
    cancel: Arc<AtomicBool>,
    last_progress: Instant,
}

impl Job {
    /// `Err` once the job has been cancelled — written so a work loop can `?`
    /// it between steps.
    pub(crate) fn check_cancelled(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(CANCELLED.to_string())
        } else {
            Ok(())
        }
    }

    /// Report progress, coalesced: at most one delivery per
    /// [`PROGRESS_INTERVAL`], so this is safe to call per chunk. A job that
    /// finishes inside one interval reports none.
    pub(crate) fn progress(&mut self, done: u64, total: Option<u64>) {
        if self.last_progress.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last_progress = Instant::now();
        self.bus.publish(NativeEvent::JobProgress {
            plugin: self.plugin,
            id: self.id,
            done,
            total,
        });
    }

    /// Deliver a batch of output lines. Not coalesced here — the caller
    /// batches, since only it knows what a quiet interval of its work is.
    pub(crate) fn output(&self, lines: Vec<String>) {
        self.bus.publish(NativeEvent::JobOutput {
            plugin: self.plugin,
            id: self.id,
            lines,
        });
    }
}

/// A validated job that has not started.
///
/// Validation and starting are separate because one caller needs them apart:
/// inside `register-events` a guest's subscriptions are not on the bus yet, so
/// a job started there could finish before anyone is listening. The spawn holds
/// the pending job and starts it once they are (PH7.8c's reasoning, applied to
/// what a guest can *start* rather than what it can *send*).
pub(crate) struct PendingJob {
    id: u64,
    plugin: u32,
    bus: Arc<EventBus>,
    /// Thread-name stem (`download`, `extract`, …) — what a stack dump shows.
    kind: &'static str,
    work: Work,
}

impl std::fmt::Debug for PendingJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingJob")
            .field("id", &self.id)
            .field("plugin", &self.plugin)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl PendingJob {
    /// A job for `plugin` that will run `work` and report on `bus`. The id is
    /// allocated here, so it can be returned to the guest before the job starts.
    pub(crate) fn new(
        plugin: u32,
        bus: Arc<EventBus>,
        kind: &'static str,
        work: impl FnOnce(&mut Job) -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            plugin,
            bus,
            kind,
            work: Box::new(work),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Start the work on its own thread.
    ///
    /// Never fails without saying so on the bus: if the thread cannot be
    /// spawned the job still publishes its `JobFinished`, because the guest
    /// already holds the id and is waiting on it.
    pub(crate) fn start(self) -> JobGuard {
        let Self {
            id,
            plugin,
            bus,
            kind,
            work,
        } = self;
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        active().insert(id, (plugin, Arc::clone(&cancel)));

        let mut job = Job {
            id,
            plugin,
            bus: Arc::clone(&bus),
            cancel: Arc::clone(&cancel),
            last_progress: Instant::now(),
        };
        let thread_done = Arc::clone(&done);
        let spawned = std::thread::Builder::new()
            .name(format!("lattice-plugin-{kind}-{id}"))
            .spawn(move || {
                let result = work(&mut job);
                finish(&job.bus, plugin, id, result);
                thread_done.store(true, Ordering::Relaxed);
            });
        if let Err(e) = spawned {
            finish(
                &bus,
                plugin,
                id,
                Err(format!("{kind} failed: cannot spawn its thread: {e}")),
            );
            done.store(true, Ordering::Relaxed);
        }
        JobGuard { cancel, done }
    }

    /// Finish without running: the job reports [`CANCELLED`]. For a job
    /// cancelled while still held for the registration window — it never
    /// started, but the guest holds its id and is owed exactly one outcome.
    pub(crate) fn cancel_unstarted(self) {
        finish(&self.bus, self.plugin, self.id, Err(CANCELLED.to_string()));
    }
}

/// A running job, as its owner holds it. Dropping it cancels the job.
#[derive(Debug)]
pub(crate) struct JobGuard {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl JobGuard {
    /// Whether the job has published its outcome — the owner prunes on this,
    /// so a long-lived instance does not accumulate a guard per job.
    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Retire `id` and publish its one outcome.
fn finish(bus: &EventBus, plugin: u32, id: u64, result: Result<(), String>) {
    active().remove(&id);
    match &result {
        Ok(()) => tracing::debug!(plugin, id, "plugin job finished"),
        Err(error) => tracing::debug!(plugin, id, %error, "plugin job failed"),
    }
    bus.publish(NativeEvent::JobFinished { plugin, id, result });
}

/// Cancel job `id` if — and only if — `plugin` owns it.
///
/// The ownership test is what stops one plugin cancelling another's job by
/// guessing ids, which are sequential. Anything else is silently nothing.
pub(crate) fn cancel(plugin: u32, id: u64) {
    if let Some((owner, flag)) = active().get(&id)
        && *owner == plugin
    {
        flag.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! What a job-shaped seam's tests need: a bus that hears job events, and a
    //! wait for one job's outcome.
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_protocol::EventKind;
    use lattice_runtime::{EventFilter, SubscriptionTarget};
    use tokio::sync::mpsc::UnboundedReceiver;

    pub(crate) fn job_bus(bus: &EventBus) -> UnboundedReceiver<NativeEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for kind in [
            EventKind::JobProgress,
            EventKind::JobOutput,
            EventKind::JobFinished,
        ] {
            bus.subscribe(
                EventFilter::kind(kind),
                SubscriptionTarget::Channel(tx.clone()),
            );
        }
        rx
    }

    /// Everything published for one job, up to and including its outcome.
    pub(crate) struct Outcome {
        pub(crate) plugin: u32,
        pub(crate) result: Result<(), String>,
        pub(crate) progress: Vec<(u64, Option<u64>)>,
        /// Every output line, in order, across all deliveries.
        pub(crate) output: Vec<String>,
        /// How many `JobOutput` deliveries those lines arrived in.
        pub(crate) output_batches: usize,
    }

    pub(crate) fn outcome_of(rx: &mut UnboundedReceiver<NativeEvent>, id: u64) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut progress = Vec::new();
        let mut output = Vec::new();
        let mut output_batches = 0;
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(NativeEvent::JobProgress {
                    id: got,
                    done,
                    total,
                    ..
                }) if got == id => progress.push((done, total)),
                Ok(NativeEvent::JobOutput { id: got, lines, .. }) if got == id => {
                    output.extend(lines);
                    output_batches += 1;
                }
                Ok(NativeEvent::JobFinished {
                    plugin,
                    id: got,
                    result,
                }) if got == id => {
                    return Outcome {
                        plugin,
                        result,
                        progress,
                        output,
                        output_batches,
                    };
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("job {id} never reported an outcome");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::test_support::*;
    use super::*;

    fn bus() -> (
        Arc<EventBus>,
        tokio::sync::mpsc::UnboundedReceiver<NativeEvent>,
    ) {
        let bus = Arc::new(EventBus::new());
        let rx = job_bus(&bus);
        (bus, rx)
    }

    #[test]
    fn a_job_reports_exactly_its_outcome_addressed_to_its_plugin() {
        let (bus, mut rx) = bus();
        let pending = PendingJob::new(7, bus, "test", |_| Err("no good".into()));
        let id = pending.id();
        let guard = pending.start();
        let outcome = outcome_of(&mut rx, id);
        assert_eq!(outcome.plugin, 7);
        assert_eq!(outcome.result, Err("no good".to_string()));
        // `done` is set just after the publish, on the job's thread.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !guard.is_done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(guard.is_done());
        assert!(active().get(&id).is_none(), "retired from the table");
    }

    /// A loop that polls until cancelled, for the three ways of cancelling.
    fn spin(bus: Arc<EventBus>, plugin: u32) -> PendingJob {
        PendingJob::new(plugin, bus, "test", |job| {
            for _ in 0..400 {
                job.check_cancelled()?;
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })
    }

    #[test]
    fn its_owner_can_cancel_it_by_id() {
        let (bus, mut rx) = bus();
        let pending = spin(bus, 3);
        let id = pending.id();
        let _guard = pending.start();
        cancel(3, id);
        assert_eq!(outcome_of(&mut rx, id).result, Err(CANCELLED.to_string()));
    }

    /// Ids are sequential, so the ownership check is the only thing between
    /// one plugin and another's job.
    #[test]
    fn another_plugin_cannot_cancel_it() {
        let (bus, mut rx) = bus();
        let pending = PendingJob::new(3, bus, "test", |job| {
            std::thread::sleep(Duration::from_millis(150));
            job.check_cancelled()
        });
        let id = pending.id();
        let _guard = pending.start();
        cancel(4, id);
        assert_eq!(outcome_of(&mut rx, id).result, Ok(()));
    }

    /// Unload and quarantine both reduce to this.
    #[test]
    fn dropping_the_guard_cancels_it() {
        let (bus, mut rx) = bus();
        let pending = spin(bus, 3);
        let id = pending.id();
        drop(pending.start());
        assert_eq!(outcome_of(&mut rx, id).result, Err(CANCELLED.to_string()));
    }

    #[test]
    fn a_job_cancelled_before_it_starts_still_reports() {
        let (bus, mut rx) = bus();
        let pending = PendingJob::new(3, bus, "test", |_| panic!("must not run"));
        let id = pending.id();
        pending.cancel_unstarted();
        assert_eq!(outcome_of(&mut rx, id).result, Err(CANCELLED.to_string()));
    }

    #[test]
    fn cancelling_an_unknown_id_is_nothing() {
        cancel(1, u64::MAX);
    }

    /// Called per chunk by a seam; delivered per interval.
    #[test]
    fn progress_is_coalesced_and_carries_the_total() {
        let (bus, mut rx) = bus();
        let pending = PendingJob::new(3, bus, "test", |job| {
            for n in 0..300u64 {
                job.progress(n, Some(300));
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(())
        });
        let id = pending.id();
        let started = std::time::Instant::now();
        let _guard = pending.start();
        let outcome = outcome_of(&mut rx, id);
        let elapsed = started.elapsed();
        let p = &outcome.progress;
        assert!(!p.is_empty(), "~600 ms of work reported progress");
        // The claim is "at most one delivery per interval", so the bound is
        // the number of intervals that actually passed — NOT a constant. This
        // asserted `< 30`, which holds when 300 two-millisecond sleeps take
        // the ~600 ms they ask for and fails on a loaded runner where they
        // take seconds: CI saw 42 deliveries, correctly coalesced, over a run
        // several times longer than the constant assumed.
        let intervals = (elapsed.as_millis() / 100) as usize + 2;
        assert!(
            p.len() <= intervals,
            "coalesced to one per 100 ms: {} deliveries in {elapsed:?} ({intervals} intervals)",
            p.len()
        );
        assert!(
            p.len() < 300,
            "and never one per call: {} deliveries for 300 calls",
            p.len()
        );
        assert!(p.iter().all(|(_, total)| *total == Some(300)));
        assert!(p.windows(2).all(|w| w[0].0 <= w[1].0), "monotonic: {p:?}");
    }
}
