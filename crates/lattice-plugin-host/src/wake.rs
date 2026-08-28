//! OC.2 — the periodic wake seam: `wake-every` / `cancel-wake` / `on-wake`.
//!
//! ## Why a plugin needs one
//!
//! A plugin whose *display* changes without the buffer changing has no way to
//! say so. Org's running clock is the motivating case: the file it wrote is
//! already correct and nothing edits it again, but the modeline segment
//! (`◷ 0:14 …`) has to re-render once a minute. Nothing in the editor fires on
//! "a minute passed", so before this the segment could only advance on the next
//! keystroke — which is the "it works, but only after I hit something" failure
//! the boot-composition rules exist to design out.
//!
//! The host could have owned an `elapsed-since(T)` element and ticked it with
//! zero WASM calls. That was rejected (design D7): it moves duration semantics
//! into the host, and the general wake is a primitive `design.md` Appendix B
//! already wants for idle hooks. One typed call per minute against a <500 ns p99
//! budget is negligible on magnitude.
//!
//! ## Where the time comes from, and why it is injected
//!
//! `lattice-plugin-host` owns no runtime — `tokio` is a dev-dependency and
//! `futures` was chosen over `tokio::sync` specifically to keep it that way, so
//! the lib stays executor-agnostic and the caller spawns every actor. A timer is
//! the first thing that would have broken that, so the timer is injected: the
//! host holds a [`SleeperHandle`], the loader supplies one backed by
//! `tokio::time::sleep`, and a harness that supplies none leaves `wake-every`
//! answering `0` — the same honest degradation every other unwired context here
//! uses.
//!
//! ## Where a wake fires
//!
//! On the plugin's **own actor task**, in the same `select` as `on-event`
//! (`event_task.rs`). That is the whole reason this shape was chosen over a
//! host-owned scheduler thread: the wake inherits the actor's budget, its
//! quarantine, and its teardown for free, and there is no cross-thread hop
//! between the timer and the guest call. Aborting the actor task — what the
//! loader does on unload — drops the pending sleeps with it, so "cancelled en
//! masse on deactivate" is structural rather than a step someone must remember.
//!
//! The `events` interface is **not** on the sync grammar linker, so a wake is
//! unreachable from the keystroke path by construction (paramount #4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

/// The timer the wake seam sleeps on, injected so this crate needs no runtime.
///
/// One method, deliberately: a wake needs to know *that* an interval passed, not
/// what time it is. Anything richer (deadlines, cancellation tokens, an interval
/// stream) would be an executor's API smuggled back in through the trait.
pub trait Sleeper: Send + Sync + 'static {
    /// A future that completes no sooner than `dur` from now. Being late is
    /// allowed and expected; being early is not.
    fn sleep(&self, dur: Duration) -> BoxFuture<'static, ()>;
}

/// The shared handle a [`PluginHost`](crate::PluginHost) is given once at boot.
pub type SleeperHandle = Arc<dyn Sleeper>;

/// The shortest interval a guest can arm, in milliseconds.
///
/// A wake is a full guest call, so `wake-every(0)` is a request to re-enter the
/// guest as fast as the executor will let it — a busy loop wearing a timer's
/// clothes, and one that would starve the plugin's own event deliveries since
/// they share the actor. Clamping rather than refusing keeps a slightly-too-eager
/// plugin working instead of silently never waking; the clamp is logged and
/// documented in `events.wit` so it is not a surprise.
pub(crate) const MIN_WAKE_MS: u32 = 50;

/// Per-plugin wake bookkeeping, held on the plugin's `PluginState` and therefore
/// owned by its actor.
///
/// No lock: the store has exactly one owner at a time. The guest arms and cancels
/// from inside a guest call (the actor is blocked on it); the actor reads the
/// registry only between calls. The two never overlap, and saying so here is
/// cheaper than a mutex that would exist purely to restate it.
pub(crate) struct WakeCtx {
    sleeper: SleeperHandle,
    /// The next id to hand out. Monotonic and never reused — a cancelled id that
    /// came round again would let a stale in-flight sleep re-arm a wake the guest
    /// had already disowned.
    next: u32,
    /// Armed wakes, id → period. `cancel-wake` removes the entry; a sleep that
    /// completes for an id no longer here is dropped instead of delivered, which
    /// is how cancellation reaches a timer already in flight.
    live: HashMap<u32, Duration>,
    /// Ids armed since the actor last looked. The actor drains this after every
    /// guest call and turns each into a pending sleep.
    newly_armed: Vec<u32>,
}

impl WakeCtx {
    pub(crate) fn new(sleeper: SleeperHandle) -> Self {
        Self {
            sleeper,
            next: 1, // `0` is the refusal value; never hand it out.
            live: HashMap::new(),
            newly_armed: Vec::new(),
        }
    }

    /// Arm a periodic wake; returns its host-allocated id.
    pub(crate) fn arm(&mut self, plugin: u32, ms: u32) -> u32 {
        let ms = if ms < MIN_WAKE_MS {
            tracing::warn!(
                plugin,
                requested_ms = ms,
                clamped_ms = MIN_WAKE_MS,
                "wake-every interval below the floor; clamped"
            );
            MIN_WAKE_MS
        } else {
            ms
        };
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        self.live.insert(id, Duration::from_millis(u64::from(ms)));
        self.newly_armed.push(id);
        tracing::debug!(plugin, wake = id, ms, "wake armed");
        id
    }

    /// Disarm a wake. Idempotent — an unknown or already-cancelled id is a no-op,
    /// so a guest never has to mirror host state to avoid a trap.
    pub(crate) fn cancel(&mut self, plugin: u32, id: u32) {
        if self.live.remove(&id).is_some() {
            tracing::debug!(plugin, wake = id, "wake cancelled");
        }
    }

    /// Disarm everything. Called when the plugin is quarantined: its store is
    /// dead, so re-entering it once a minute forever would be pure waste.
    pub(crate) fn cancel_all(&mut self) {
        self.live.clear();
        self.newly_armed.clear();
    }

    /// The period of a still-armed wake, or `None` if it was cancelled while its
    /// sleep was in flight.
    pub(crate) fn period(&self, id: u32) -> Option<Duration> {
        self.live.get(&id).copied()
    }

    /// Take the ids armed since the last call, for the actor to turn into sleeps.
    pub(crate) fn take_newly_armed(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.newly_armed)
    }

    /// A sleep of `dur` that resolves to `id` — the shape the actor's
    /// `FuturesUnordered` wants.
    pub(crate) fn sleep_for(&self, id: u32, dur: Duration) -> BoxFuture<'static, u32> {
        let fut = self.sleeper.sleep(dur);
        Box::pin(async move {
            fut.await;
            id
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    struct NeverSleeper;
    impl Sleeper for NeverSleeper {
        fn sleep(&self, _dur: Duration) -> BoxFuture<'static, ()> {
            Box::pin(std::future::pending())
        }
    }

    fn ctx() -> WakeCtx {
        WakeCtx::new(Arc::new(NeverSleeper))
    }

    #[test]
    fn ids_start_at_one_so_zero_stays_the_refusal_value() {
        let mut c = ctx();
        assert_eq!(c.arm(0, 1000), 1);
        assert_eq!(c.arm(0, 1000), 2);
    }

    #[test]
    fn an_interval_below_the_floor_is_clamped_not_refused() {
        let mut c = ctx();
        let id = c.arm(0, 0);
        assert_eq!(
            c.period(id),
            Some(Duration::from_millis(u64::from(MIN_WAKE_MS))),
            "a zero interval arms at the floor rather than spinning the actor"
        );
    }

    #[test]
    fn cancel_is_idempotent_and_drops_the_period() {
        let mut c = ctx();
        let id = c.arm(0, 1000);
        c.cancel(0, id);
        assert_eq!(
            c.period(id),
            None,
            "a cancelled wake has no period to re-arm"
        );
        c.cancel(0, id); // second cancel: no panic, no effect
        c.cancel(0, 9999); // never-issued id: same
    }

    #[test]
    fn a_cancelled_id_is_never_reissued() {
        let mut c = ctx();
        let first = c.arm(0, 1000);
        c.cancel(0, first);
        let second = c.arm(0, 1000);
        assert_ne!(
            first, second,
            "reuse would let an in-flight sleep re-arm a disowned wake"
        );
    }

    #[test]
    fn newly_armed_drains_once() {
        let mut c = ctx();
        c.arm(0, 1000);
        c.arm(0, 2000);
        assert_eq!(c.take_newly_armed().len(), 2);
        assert!(
            c.take_newly_armed().is_empty(),
            "a second drain must not re-arm the same sleeps"
        );
    }

    #[test]
    fn cancel_all_clears_pending_arms_too() {
        let mut c = ctx();
        let id = c.arm(0, 1000);
        c.cancel_all();
        assert_eq!(c.period(id), None);
        assert!(
            c.take_newly_armed().is_empty(),
            "a quarantined plugin must not have a sleep armed after the fact"
        );
    }
}
