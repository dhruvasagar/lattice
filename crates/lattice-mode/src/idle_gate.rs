//! WK.3: idle-gate registry — the generic armed-deadline primitive.
//!
//! A subsystem registers a handler and an *armed deadline*; when the
//! deadline elapses the editor actor runs the handler and applies the
//! `Effect`s it returns. Which-key's "hold a prefix for 300 ms" is the
//! first consumer; "did you mean…" hints, idle-time prefetch and the
//! inline-diagnostic gate are the obvious others.
//!
//! ## Why a registry rather than a field
//!
//! This is the third instance of a shape this codebase has twice decided
//! is right. [`tick_callback`](crate::tick_callback)'s module doc names
//! the alternative as the smell it exists to kill:
//!
//! > rather than adding an `Editor::drain_<x>` method + an
//! > `Option<Receiver>` field per subsystem, a mode owns its channel and
//! > registers a closure that drains it.
//!
//! A deadline field per subsystem is that same smell in the time domain,
//! and `Editor::inline_diag_deadline` is the existing instance of it —
//! a bespoke `Option<Instant>` on the editor plus a hand-written
//! `select!` arm in the actor. One more subsystem wanting a delay would
//! mean a second field and a second arm.
//!
//! The inline-diagnostic gate is deliberately NOT migrated here (design
//! §9): its arm decision runs inside `publish_render_state`, reading
//! `config`, `modal` and `cursor.line`, and there is no cursor-moved
//! typed event to subscribe to. Finishing that migration means
//! publishing a `CursorSettled` event plus surgery on working code, and
//! gating a discoverability feature behind an LSP refactor is backwards.
//! This registry runs beside it.
//!
//! ## Timing
//!
//! The registry stores deadlines only; it owns no timer. The actor asks
//! for [`earliest`](IdleGateRegistry::earliest) once per loop iteration
//! and points its single pinned sleep there, then calls
//! [`fire_elapsed`](IdleGateRegistry::fire_elapsed) when it wakes. That
//! keeps every `tokio` concern in the actor and leaves this unit
//! testable with a plain clock.
//!
//! ## Lifecycle
//!
//! [`register`](IdleGateRegistry::register) returns an RAII
//! [`IdleGateHandle`], mirroring `TickCallbackRegistration`: a mode
//! aggregates the handle into its `Guard`, so a deactivated subsystem
//! contributes no timer at all.

use std::sync::Arc;
use std::sync::Mutex;

use lattice_grammar::effect::Effect;
use tokio::time::Instant;

/// A gate's body. Runs on the editor actor thread when the gate's
/// deadline elapses; returns the `Effect`s the host applies.
///
/// `FnMut` for the same reason a tick callback is: the canonical body
/// reads state the subsystem stashed when it armed.
pub type IdleGateHandler = Box<dyn FnMut() -> Vec<Effect> + Send + 'static>;

/// Typed handle for `ServiceRegistry` lookup. Per the Arc/TypeId rule,
/// register and look up with the same `T`; this alias guarantees it.
pub type IdleGateRegistryHandle = Arc<IdleGateRegistry>;

struct Gate {
    id: u64,
    /// Human-readable, for `debug!` lines when a gate fires.
    name: &'static str,
    /// `None` = disarmed. A disarmed gate costs nothing: it never
    /// contributes to `earliest`, so the actor's sleep stays parked.
    deadline: Option<Instant>,
    handler: IdleGateHandler,
}

struct Inner {
    next_id: u64,
    gates: Vec<Gate>,
}

/// Registry of subsystem-contributed idle gates.
pub struct IdleGateRegistry {
    inner: Mutex<Inner>,
}

impl IdleGateRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 0,
                gates: Vec::new(),
            }),
        }
    }

    /// Register a gate, disarmed. Returns an RAII handle; dropping it
    /// removes the gate.
    pub fn register(
        self: &Arc<Self>,
        name: &'static str,
        handler: IdleGateHandler,
    ) -> IdleGateHandle {
        let id = {
            let mut g = self.lock();
            let id = g.next_id;
            g.next_id += 1;
            g.gates.push(Gate {
                id,
                name,
                deadline: None,
                handler,
            });
            id
        };
        IdleGateHandle {
            registry: Arc::clone(self),
            id,
        }
    }

    /// The earliest armed deadline, or `None` when every gate is
    /// disarmed. The actor points its pinned sleep here; `None` means
    /// park it far out and let the guard keep the arm dormant.
    pub fn earliest(&self) -> Option<Instant> {
        self.lock().gates.iter().filter_map(|g| g.deadline).min()
    }

    /// Run every gate whose deadline is at or before `now`, disarming
    /// each as it fires, and return the concatenated `Effect`s.
    ///
    /// Disarm-then-run is deliberate: a gate that re-arms itself from
    /// inside its own handler must be able to, and it cannot if firing
    /// clears the deadline afterwards.
    pub fn fire_elapsed(&self, now: Instant) -> Vec<Effect> {
        let mut g = self.lock();
        let mut effects = Vec::new();
        for gate in g.gates.iter_mut() {
            if gate.deadline.is_some_and(|d| d <= now) {
                gate.deadline = None;
                tracing::debug!(gate = gate.name, "idle gate fired");
                effects.extend((gate.handler)());
            }
        }
        effects
    }

    fn arm(&self, id: u64, at: Instant) {
        if let Some(gate) = self.lock().gates.iter_mut().find(|g| g.id == id) {
            gate.deadline = Some(at);
        }
    }

    fn disarm(&self, id: u64) {
        if let Some(gate) = self.lock().gates.iter_mut().find(|g| g.id == id) {
            gate.deadline = None;
        }
    }

    fn unregister(&self, id: u64) {
        self.lock().gates.retain(|g| g.id != id);
    }

    /// Number of registered gates. Test affordance.
    #[doc(hidden)]
    pub fn registered_count(&self) -> usize {
        self.lock().gates.len()
    }

    /// Poison recovery, for the same reason `TickCallbackRegistry` does
    /// it: one panicking handler must not wedge every other subsystem's
    /// gate.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for IdleGateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for IdleGateRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleGateRegistry")
            .field("registered_count", &self.registered_count())
            .finish_non_exhaustive()
    }
}

/// RAII handle: arms and disarms one gate, and removes it on drop.
///
/// `Send + 'static` so it fits the `Mode::Guard: Send + 'static` bound.
pub struct IdleGateHandle {
    registry: Arc<IdleGateRegistry>,
    id: u64,
}

impl IdleGateHandle {
    /// Arm (or re-arm) this gate to fire at `at`. Re-arming an armed
    /// gate replaces its deadline — which is what makes a *growing*
    /// prefix pay the delay once per chord rather than once per key.
    pub fn arm(&self, at: Instant) {
        self.registry.arm(self.id, at);
    }

    /// Cancel a pending fire. Idempotent.
    pub fn disarm(&self) {
        self.registry.disarm(self.id);
    }
}

impl Drop for IdleGateHandle {
    fn drop(&mut self) {
        self.registry.unregister(self.id);
    }
}

impl std::fmt::Debug for IdleGateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleGateHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn counting_gate(count: Arc<AtomicUsize>) -> IdleGateHandler {
        Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        })
    }

    #[tokio::test]
    async fn an_unarmed_registry_parks_the_actor_sleep() {
        let r = Arc::new(IdleGateRegistry::new());
        let _h = r.register("test", counting_gate(Arc::new(AtomicUsize::new(0))));
        assert!(
            r.earliest().is_none(),
            "a registered-but-disarmed gate contributes no deadline"
        );
    }

    #[tokio::test]
    async fn the_earlier_of_two_armed_gates_is_the_one_the_actor_sleeps_to() {
        let r = Arc::new(IdleGateRegistry::new());
        let early_count = Arc::new(AtomicUsize::new(0));
        let late_count = Arc::new(AtomicUsize::new(0));
        let early = r.register("early", counting_gate(Arc::clone(&early_count)));
        let late = r.register("late", counting_gate(Arc::clone(&late_count)));

        let now = Instant::now();
        late.arm(now + Duration::from_millis(500));
        early.arm(now + Duration::from_millis(100));
        assert_eq!(
            r.earliest(),
            Some(now + Duration::from_millis(100)),
            "the minimum across gates, not registration order"
        );

        // Fire at a moment past the early deadline only.
        r.fire_elapsed(now + Duration::from_millis(200));
        assert_eq!(early_count.load(Ordering::SeqCst), 1);
        assert_eq!(late_count.load(Ordering::SeqCst), 0, "not yet due");
        assert_eq!(
            r.earliest(),
            Some(now + Duration::from_millis(500)),
            "a fired gate disarms itself; the later one remains"
        );

        r.fire_elapsed(now + Duration::from_millis(600));
        assert_eq!(late_count.load(Ordering::SeqCst), 1);
        assert!(r.earliest().is_none(), "both fired and disarmed");
    }

    #[tokio::test]
    async fn a_fired_gate_does_not_fire_again() {
        let r = Arc::new(IdleGateRegistry::new());
        let count = Arc::new(AtomicUsize::new(0));
        let h = r.register("once", counting_gate(Arc::clone(&count)));
        let now = Instant::now();
        h.arm(now);
        r.fire_elapsed(now);
        r.fire_elapsed(now + Duration::from_secs(1));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "firing disarms — otherwise a popup would reopen every loop \
             iteration forever"
        );
    }

    #[tokio::test]
    async fn disarm_cancels_a_pending_fire() {
        let r = Arc::new(IdleGateRegistry::new());
        let count = Arc::new(AtomicUsize::new(0));
        let h = r.register("cancelled", counting_gate(Arc::clone(&count)));
        let now = Instant::now();
        h.arm(now + Duration::from_millis(50));
        h.disarm();
        assert!(r.earliest().is_none());
        r.fire_elapsed(now + Duration::from_secs(1));
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "the chord resolved before the delay elapsed — no popup"
        );
    }

    #[tokio::test]
    async fn re_arming_replaces_the_deadline() {
        let r = Arc::new(IdleGateRegistry::new());
        let h = r.register("regrow", counting_gate(Arc::new(AtomicUsize::new(0))));
        let now = Instant::now();
        h.arm(now + Duration::from_millis(100));
        h.arm(now + Duration::from_millis(300));
        assert_eq!(
            r.earliest(),
            Some(now + Duration::from_millis(300)),
            "re-arm replaces rather than adding a second deadline"
        );
    }

    #[tokio::test]
    async fn dropping_the_handle_deregisters_the_gate() {
        let r = Arc::new(IdleGateRegistry::new());
        let count = Arc::new(AtomicUsize::new(0));
        let now = Instant::now();
        {
            let h = r.register("scoped", counting_gate(Arc::clone(&count)));
            h.arm(now);
            assert_eq!(r.registered_count(), 1);
        }
        assert_eq!(
            r.registered_count(),
            0,
            "a deactivated subsystem contributes no timer"
        );
        r.fire_elapsed(now + Duration::from_secs(1));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn only_due_gates_fire_when_several_are_armed() {
        let r = Arc::new(IdleGateRegistry::new());
        let now = Instant::now();
        let counts: Vec<Arc<AtomicUsize>> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
        let handles: Vec<IdleGateHandle> = counts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let h = r.register("multi", counting_gate(Arc::clone(c)));
                h.arm(now + Duration::from_millis(100 * (i as u64 + 1)));
                h
            })
            .collect();

        r.fire_elapsed(now + Duration::from_millis(250));
        assert_eq!(
            counts
                .iter()
                .map(|c| c.load(Ordering::SeqCst))
                .collect::<Vec<_>>(),
            vec![1, 1, 0],
            "the 100ms and 200ms gates fired; the 300ms one is still armed"
        );
        drop(handles);
    }
}
