//! The foreground-cancellation seam, as a registered service (CG.2).
//!
//! Design: `docs/dev/architecture/cancellation.md`; sequencing:
//! `docs/dev/operations/slice-plans/cancellation.md`.
//!
//! # Why a service rather than a method on `Editor`
//!
//! CG.1 put `arm_cancel()` on `Editor`, which needs `&mut`. That is
//! reachable from a provider's *initial* trigger — `open_search_view`
//! holds a `&mut dyn ModeActivator` — but not from the places that
//! respawn the same work later. Project search's `gr` refresh is an
//! `ActionHandlerRegistration` closure holding only `&self` services,
//! and it spawns a replacement scan; LSP requests (CG.3) and plugin
//! calls (CG.4) sit behind the same wall.
//!
//! Enrolling only where `&mut` happens to be available is the
//! half-migration this project keeps re-discovering: `<C-g>` would
//! cancel a *fresh* search and silently do nothing to a refreshed one,
//! and nothing about the code would say so. So the arming surface takes
//! `&self` and lives in the `ServiceRegistry`, where every subsystem
//! already looks things up.
//!
//! # Contract
//!
//! `<C-g>` ([`ForegroundCancel::cancel`]) reaches three things:
//!
//! - **keyed tasks** that may be killed ([`ForegroundCancel::begin`]);
//! - **enrolled** tokens — work with a supersede discipline of its own,
//!   such as LSP's per-feature requests ([`ForegroundCancel::enrol`]);
//! - **the interrupt token** ([`ForegroundCancel::current_token`]), which
//!   the plugin host snapshots per guest call: `<C-g>` flips it and installs
//!   a fresh one, so it stops whatever plugin call is running then — and
//!   nothing else ever cancels a plugin call (CG.12, decided with Dhruva
//!   2026-10-20).
//!
//! CG.12 retired the single armed slot (`arm()`), which superseded
//! *across* tasks: arming a search cancelled whatever was armed before,
//! whatever it was, and a plugin call that had adopted it.
//!
//! # Keyed supersede (CG.6)
//!
//! The editor-wide rule depends on what the re-trigger asks for
//! (`cancellation.md` §9): **the same task with the same inputs leaves the
//! run in flight alone** — restarting would throw its progress away — and
//! **the same task with new inputs supersedes it**, since the run in
//! flight is computing an answer nobody wants any more. That is
//! [`ForegroundCancel::begin`]: a [`TaskKey`] names the task and its
//! inputs, a refused trigger is [`AlreadyRunning`], and the returned
//! [`TaskTicket`] tells the delivery path whether its run is still the
//! one that counts.
//!
//! # Registration
//!
//! Register and look up under [`ForegroundCancelHandle`], never under
//! `ForegroundCancel`. `ServiceRegistry::register::<T>` keys on
//! `TypeId::of::<T>()`, so registering an `Arc<ForegroundCancel>` and
//! asking for a `ForegroundCancel` silently returns `None`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use lattice_protocol::CancellationToken;

/// Whether a run of a task may be killed — by a re-trigger with new
/// inputs, or by `<C-g>` (`cancellation.md` §9.2). Declared by the
/// producer on its [`TaskKey`], because only the producer knows whether
/// its work survives a kill.
///
/// A re-trigger with the SAME inputs never kills anything, whatever the
/// policy: the run in flight is already doing what was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RePolicy {
    /// New inputs cancel the run in flight and start a new one. For work
    /// whose effect is atomic or redone by its replacement: a push, a
    /// fetch, a build, a scan, a search, a formatter. `<C-g>` reaches it.
    Supersede,
    /// Every re-trigger is refused while a run is in flight. For work a
    /// kill could leave half-done — `git commit`, `rebase`, `stash`,
    /// `reset`: a stale `.git/index.lock` or a half-applied change is
    /// worse than asking the user to wait. Out of `<C-g>`'s reach for the
    /// same reason.
    RunToCompletion,
}

/// A user-triggered task, and what this trigger asks of it
/// (`cancellation.md` §9.1).
///
/// `kind` names the operation (`"magit:push"`) and `scope` the thing it
/// acts on (a repository and a destination, a view, a buffer); together
/// they are the task's *identity* — two triggers with the same pair are
/// the same task. `inputs` fingerprints what this trigger asks for: the
/// push's arguments, the search query, the buffer version a format was
/// computed from. Whether a re-trigger leaves the run in flight alone or
/// replaces it is decided by comparing inputs.
///
/// The producer that spawns the work builds the key — identity and inputs
/// are decided where the work is understood, never guessed from a label by
/// the host. A key with no inputs treats every re-trigger as identical.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskKey {
    kind: &'static str,
    scope: String,
    inputs: String,
    policy: RePolicy,
}

impl TaskKey {
    /// A task that new inputs supersede ([`RePolicy::Supersede`]).
    pub fn supersede(kind: &'static str, scope: impl Into<String>) -> Self {
        Self {
            kind,
            scope: scope.into(),
            inputs: String::new(),
            policy: RePolicy::Supersede,
        }
    }

    /// A task no re-trigger may interrupt ([`RePolicy::RunToCompletion`]).
    pub fn run_to_completion(kind: &'static str, scope: impl Into<String>) -> Self {
        Self {
            kind,
            scope: scope.into(),
            inputs: String::new(),
            policy: RePolicy::RunToCompletion,
        }
    }

    /// What this trigger asks for. A re-trigger whose inputs equal the run
    /// in flight's leaves that run alone; different inputs supersede it.
    pub fn with_inputs(mut self, inputs: impl Into<String>) -> Self {
        self.inputs = inputs.into();
        self
    }

    /// What this trigger asks for, as [`with_inputs`](Self::with_inputs)
    /// set it.
    pub fn inputs(&self) -> &str {
        &self.inputs
    }

    /// The operation, as the producer named it.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// What the operation acts on.
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// What a re-trigger does to a run in flight.
    pub fn policy(&self) -> RePolicy {
        self.policy
    }
}

/// [`ForegroundCancel::begin`] refused the trigger, and the run in flight
/// carries on: it was asked for the same inputs, or it is a
/// [`RePolicy::RunToCompletion`] task. The producer says so in its own
/// words — "*push is already running in lattice*" — rather than starting a
/// second copy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{kind}` is already running ({scope})")]
pub struct AlreadyRunning {
    /// The running task's kind.
    pub kind: &'static str,
    /// The running task's scope.
    pub scope: String,
}

/// One task's in-flight run.
#[derive(Debug)]
struct Slot {
    generation: u64,
    token: CancellationToken,
    inputs: String,
    policy: RePolicy,
}

type TaskId = (&'static str, String);

/// A run of a keyed task, from [`ForegroundCancel::begin`].
///
/// Hand [`token`](Self::token) to the work, and keep the ticket with the
/// *result*: the path that applies it checks [`is_current`](Self::is_current)
/// first, so a run that has since been superseded or cancelled lands
/// nowhere. Dropping the ticket ends the run — the task's slot is freed
/// for the next trigger, unless a newer run already holds it.
#[derive(Debug)]
#[must_use = "a dropped ticket ends the run at once: keep it with the work"]
pub struct TaskTicket {
    owner: Weak<ForegroundCancel>,
    id: TaskId,
    generation: u64,
    token: CancellationToken,
}

impl TaskTicket {
    /// The token the work polls, or hands to
    /// `lattice_core::process::run_cancellable`.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Whether this run's result should still be applied: it has not
    /// been cancelled, and no newer run of the same task has begun.
    ///
    /// The stale-result check. Without it a run that started first and
    /// finished last overwrites the newer one — a refresh repainting old
    /// state, a formatter's edits landing on text that moved on.
    pub fn is_current(&self) -> bool {
        if self.token.is_cancelled() {
            return false;
        }
        let Some(owner) = self.owner.upgrade() else {
            return false;
        };
        owner
            .tasks()
            .get(&self.id)
            .is_some_and(|slot| slot.generation == self.generation)
    }
}

impl Drop for TaskTicket {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut tasks = owner.tasks();
        if tasks
            .get(&self.id)
            .is_some_and(|slot| slot.generation == self.generation)
        {
            tasks.remove(&self.id);
        }
    }
}

/// What `<C-g>` reaches, shared between the host (which cancels) and every
/// subsystem that spawns cancellable work (which begins or enrols).
///
/// `Mutex` rather than `ArcSwap`: each change is a compare-and-swap in
/// spirit — cancel the old, install the new — and happens once per user
/// action, not per keystroke or per frame. There is no hot-path read: the
/// *token* is what gets polled in tight loops, and that is a plain atomic
/// the caller already holds by then.
#[derive(Debug, Default)]
pub struct ForegroundCancel {
    /// CG.12: what a running plugin call is cancelled by. `cancel` flips it
    /// and installs a fresh one, so a call snapshots the token current when
    /// it starts and only a `<C-g>` pressed while it runs reaches it.
    interrupt: Mutex<CancellationToken>,
    enrolled: Mutex<Vec<CancellationToken>>,
    /// CG.6: the in-flight run of each keyed task. Entries leave when
    /// their ticket drops, so this holds what is running, not what ran.
    tasks: Mutex<HashMap<TaskId, Slot>>,
    generations: AtomicU64,
}

/// Register and look up under this alias — see the module docs on the
/// `TypeId` pitfall.
pub type ForegroundCancelHandle = Arc<ForegroundCancel>;

impl ForegroundCancel {
    /// Begin a run of the task `key` names (CG.6, `cancellation.md` §9).
    ///
    /// With a run of the same task in flight:
    ///
    /// - **same inputs** — refused with [`AlreadyRunning`]; the run in
    ///   flight is already doing what was asked, and restarting it would
    ///   only throw its progress away;
    /// - **new inputs**, [`RePolicy::Supersede`] — the run in flight is
    ///   cancelled and this one begins;
    /// - **new inputs**, [`RePolicy::RunToCompletion`] — refused; killing
    ///   it is unsafe.
    ///
    /// A run that has been cancelled (by `<C-g>`, or superseded) counts as
    /// gone even while it unwinds, so "`<C-g>`, then trigger again" always
    /// restarts. Tasks with different identities never touch each other.
    ///
    /// Takes `&Arc<Self>` — the [`ForegroundCancelHandle`] every caller
    /// already holds — because the ticket frees its slot when dropped,
    /// and must not keep the service alive to do it.
    ///
    /// # Errors
    ///
    /// [`AlreadyRunning`] when a run-to-completion run of this task is in
    /// flight.
    pub fn begin(self: &Arc<Self>, key: TaskKey) -> Result<TaskTicket, AlreadyRunning> {
        let id = (key.kind, key.scope);
        let mut tasks = self.tasks();
        if let Some(running) = tasks.get(&id)
            && !running.token.is_cancelled()
        {
            let refuse =
                running.inputs == key.inputs || running.policy == RePolicy::RunToCompletion;
            if refuse {
                return Err(AlreadyRunning {
                    kind: id.0,
                    scope: id.1,
                });
            }
            running.token.cancel();
        }
        let generation = self.generations.fetch_add(1, Ordering::Relaxed) + 1;
        let token = CancellationToken::new();
        tasks.insert(
            id.clone(),
            Slot {
                generation,
                token: token.clone(),
                inputs: key.inputs,
                policy: key.policy,
            },
        );
        Ok(TaskTicket {
            owner: Arc::downgrade(self),
            id,
            generation,
            token,
        })
    }

    /// How many keyed tasks are in flight. For tests and for a future
    /// status indicator (`cancellation.md` §7).
    pub fn running_tasks(&self) -> usize {
        self.tasks().len()
    }

    /// The keyed-task map. A poisoned lock is recovered, as everywhere
    /// in this module: losing one run's bookkeeping beats a panic.
    fn tasks(&self) -> MutexGuard<'_, HashMap<TaskId, Slot>> {
        match self.tasks.lock() {
            Ok(tasks) => tasks,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The interrupt token, for a plugin call to snapshot as it starts
    /// (CG.4, CG.12).
    ///
    /// The plugin host's epoch callback fires every millisecond inside a
    /// running guest call and cannot take a lock there, so it takes this
    /// ONCE per call and polls the returned token (a plain atomic)
    /// thereafter. `<C-g>` flips it and installs a fresh one: the call that
    /// was running stops, and a call started afterwards is not reached by
    /// that earlier press. No other task can cancel a plugin call.
    ///
    /// Always `Some`; an `Option` because the plugin host stores it as one.
    pub fn current_token(&self) -> Option<CancellationToken> {
        Some(
            match self.interrupt.lock() {
                Ok(token) => token,
                // Losing cancellability for one call beats a panic.
                Err(poisoned) => poisoned.into_inner(),
            }
            .clone(),
        )
    }

    /// Join the foreground set **without** superseding anything (CG.3).
    ///
    /// For work that already has its own cancellation discipline and
    /// only needs `<C-g>` to reach it. Every user-triggered LSP command
    /// is like this: hover, rename, format and code-actions each hold a
    /// per-feature token so a second hover supersedes the first, and
    /// that is the right granularity — a hover has no business
    /// cancelling a rename.
    ///
    /// A keyed [`begin`](Self::begin) would be wrong here: those
    /// requests already supersede per feature, and `K` is a reflexive
    /// inspect key that must not stop anything but a previous hover.
    ///
    /// **Automatic requests must not come through here at all** —
    /// completion, signature-help and the `maybe_request_*` family fire
    /// on keystrokes, cursor moves and ticks. They are not
    /// user-triggered, so by §3 of the design they are not foreground;
    /// enrolling them would make `<C-g>` cancel whatever the editor
    /// happened to be doing on its own behalf.
    ///
    /// Already-cancelled entries are pruned on each call, which is what
    /// keeps the set from growing without bound across a session: a
    /// completed request's token is never cancelled, so pruning cannot
    /// rely on it — instead the set is small by construction (one live
    /// request per feature) and every `cancel()` empties it.
    pub fn enrol(&self, token: CancellationToken) {
        let mut set = match self.enrolled.lock() {
            Ok(set) => set,
            Err(poisoned) => poisoned.into_inner(),
        };
        set.retain(|t| !t.is_cancelled());
        set.push(token);
    }

    /// Cancel everything foreground: the interrupt token (and install a
    /// fresh one), every enrolled token, and every keyed task that may be
    /// killed. A keyed run's slot stays until its ticket drops, and its
    /// [`TaskTicket::is_current`] is false from here on.
    ///
    /// A [`RePolicy::RunToCompletion`] task is not touched: if killing it
    /// on a re-trigger is unsafe, killing it on a key press is too.
    ///
    /// Idempotent — cancelling nothing is the common case, since the
    /// binding is pressed far more often than an operation is running.
    pub fn cancel(&self) {
        let interrupted = {
            let mut current = match self.interrupt.lock() {
                Ok(token) => token,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::take(&mut *current)
        };
        interrupted.cancel();
        let enrolled = match self.enrolled.lock() {
            Ok(mut set) => std::mem::take(&mut *set),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for token in enrolled {
            token.cancel();
        }
        for slot in self.tasks().values() {
            if slot.policy == RePolicy::Supersede {
                slot.token.cancel();
            }
        }
    }

    /// How many tokens are currently enrolled. For tests and for a
    /// future status indicator (`cancellation.md` §7).
    pub fn enrolled_len(&self) -> usize {
        match self.enrolled.lock() {
            Ok(set) => set.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// Enrolled tokens are independent of each other — a rename does
    /// not stop a hover. Per-feature supersede stays the LSP layer's
    /// job, at the granularity it already gets right.
    #[test]
    fn enrolled_tokens_do_not_cancel_each_other() {
        let fc = ForegroundCancel::default();
        let hover = CancellationToken::new();
        fc.enrol(hover.clone());
        fc.enrol(CancellationToken::new());

        assert!(!hover.is_cancelled());
    }

    /// The set must not grow without bound across a session. Cancelled
    /// entries are pruned as new ones arrive.
    #[test]
    fn cancelled_entries_are_pruned_on_enrol() {
        let fc = ForegroundCancel::default();
        for _ in 0..100 {
            let t = CancellationToken::new();
            fc.enrol(t.clone());
            t.cancel();
        }
        let live = CancellationToken::new();
        fc.enrol(live.clone());
        assert_eq!(fc.enrolled_len(), 1, "only the live token should remain");
    }

    /// CG.12: `<C-g>` stops the plugin call running when it is pressed —
    /// the token that call snapshotted flips — and a call that starts
    /// afterwards gets a fresh one that earlier press cannot reach.
    #[test]
    fn cancel_interrupts_the_call_running_then_and_no_later_one() {
        let fc = ForegroundCancel::default();
        let running = fc.current_token().unwrap();
        assert!(!running.is_cancelled());
        fc.cancel();
        assert!(running.is_cancelled(), "the call in flight is stopped");
        let later = fc.current_token().unwrap();
        assert!(!later.is_cancelled(), "a call started after the press runs");
    }

    /// The binding is pressed far more often than work is running.
    #[test]
    fn cancelling_when_idle_is_a_noop() {
        let fc = ForegroundCancel::default();
        fc.cancel();
        fc.cancel();
        assert_eq!(fc.enrolled_len(), 0);
    }

    /// `<C-g>` reaches every kind of foreground work at once: the plugin
    /// call running, enrolled requests, and keyed tasks that may be killed.
    #[test]
    fn cancel_reaches_the_interrupt_every_enrolled_token_and_keyed_tasks() {
        let fc = service();
        let plugin_call = fc.current_token().unwrap();
        let hover = CancellationToken::new();
        let rename = CancellationToken::new();
        fc.enrol(hover.clone());
        fc.enrol(rename.clone());
        let search = fc.begin(TaskKey::supersede("search", "view 1")).unwrap();

        fc.cancel();

        assert!(plugin_call.is_cancelled());
        assert!(hover.is_cancelled());
        assert!(rename.is_cancelled());
        assert!(search.token().is_cancelled());
    }

    /// CG.12: no task cancels a plugin call — beginning one, keyed or not,
    /// leaves the interrupt token alone. `arm()` used to: a search armed
    /// the shared slot and killed any call that had adopted it.
    #[test]
    fn beginning_a_task_never_cancels_a_plugin_call() {
        let fc = service();
        let plugin_call = fc.current_token().unwrap();
        let _a = fc
            .begin(TaskKey::supersede("search", "view 1").with_inputs("a"))
            .unwrap();
        let _b = fc
            .begin(TaskKey::supersede("search", "view 1").with_inputs("b"))
            .unwrap();
        fc.enrol(CancellationToken::new());
        assert!(!plugin_call.is_cancelled());
    }

    /// Shared through an `Arc` across threads, which is how the host
    /// and a guest call actually hold it.
    #[test]
    fn the_handle_is_shareable_across_threads() {
        let fc: ForegroundCancelHandle = Arc::new(ForegroundCancel::default());
        let token = fc.current_token().unwrap();
        let other = Arc::clone(&fc);
        std::thread::spawn(move || other.cancel())
            .join()
            .expect("canceller thread");
        assert!(token.is_cancelled());
    }

    fn service() -> ForegroundCancelHandle {
        Arc::new(ForegroundCancel::default())
    }

    fn push(inputs: &str) -> TaskKey {
        TaskKey::supersede("magit:push", "lattice origin").with_inputs(inputs)
    }

    /// CG.6, the case that started it: a double-pressed push. The run in
    /// flight is already doing what was asked — it carries on, and the
    /// second press is refused rather than throwing that progress away.
    #[test]
    fn the_same_inputs_again_leave_the_run_in_flight_alone() {
        let fc = service();
        let first = fc.begin(push("push origin feature")).unwrap();
        let again = fc.begin(push("push origin feature"));

        assert_eq!(
            again.unwrap_err(),
            AlreadyRunning {
                kind: "magit:push",
                scope: "lattice origin".into()
            }
        );
        assert!(
            !first.token().is_cancelled(),
            "the original is not restarted"
        );
        assert!(first.is_current());
    }

    /// New inputs mean the run in flight computes an answer nobody wants:
    /// it is superseded, and the new run is the current one.
    #[test]
    fn new_inputs_supersede_the_run_in_flight() {
        let fc = service();
        let first = fc.begin(push("push origin feature")).unwrap();
        let forced = fc
            .begin(push("push --force-with-lease origin feature"))
            .unwrap();

        assert!(first.token().is_cancelled(), "the run in flight is stopped");
        assert!(!first.is_current(), "and its result would land nowhere");
        assert!(forced.is_current());
        assert!(!forced.token().is_cancelled());
    }

    /// "`<C-g>`, then trigger again" restarts identical work — the
    /// cancelled run counts as gone while it unwinds, so the re-trigger is
    /// not told it is "already running" by the run just stopped.
    #[test]
    fn a_cancelled_run_does_not_block_its_restart() {
        let fc = service();
        let first = fc.begin(push("push origin feature")).unwrap();
        fc.cancel();
        let restarted = fc.begin(push("push origin feature")).unwrap();

        assert!(restarted.is_current());
        assert!(!first.is_current());
    }

    /// …and ONLY that task. The whole difference from `arm()`.
    #[test]
    fn a_different_task_is_left_alone() {
        let fc = service();
        let push = fc
            .begin(TaskKey::supersede("magit:push", "lattice origin"))
            .unwrap();
        let _fetch = fc
            .begin(TaskKey::supersede("magit:fetch", "lattice origin"))
            .unwrap();
        let _fork = fc
            .begin(TaskKey::supersede("magit:push", "lattice fork"))
            .unwrap();

        assert!(
            !push.token().is_cancelled(),
            "another kind or scope is another task"
        );
        assert!(push.is_current());
        assert_eq!(fc.running_tasks(), 3);
    }

    /// §9.2: a task a kill could leave half-done refuses a re-trigger,
    /// and the run in flight is untouched.
    #[test]
    fn a_run_to_completion_task_refuses_a_second_trigger() {
        let fc = service();
        let stash = fc
            .begin(TaskKey::run_to_completion("magit:stash", "lattice").with_inputs("stash push"))
            .unwrap();
        // Even with different inputs: killing a stash midway is unsafe.
        let refused = fc.begin(
            TaskKey::run_to_completion("magit:stash", "lattice").with_inputs("stash push -u"),
        );

        assert_eq!(
            refused.unwrap_err(),
            AlreadyRunning {
                kind: "magit:stash",
                scope: "lattice".into()
            }
        );
        assert!(!stash.token().is_cancelled());
        assert!(stash.is_current());

        // Once it finishes, the next trigger runs.
        drop(stash);
        assert!(
            fc.begin(TaskKey::run_to_completion("magit:stash", "lattice"))
                .is_ok()
        );
    }

    /// A finished run leaves nothing behind — and a superseded run's
    /// ticket, dropped late, does not free the slot its successor holds.
    #[test]
    fn a_dropped_ticket_frees_only_its_own_slot() {
        let fc = service();
        let first = fc
            .begin(TaskKey::supersede("search", "view 1").with_inputs("foo"))
            .unwrap();
        let second = fc
            .begin(TaskKey::supersede("search", "view 1").with_inputs("bar"))
            .unwrap();

        drop(first);
        assert!(
            second.is_current(),
            "the superseded ticket must not evict its successor"
        );
        assert_eq!(fc.running_tasks(), 1);

        drop(second);
        assert_eq!(
            fc.running_tasks(),
            0,
            "nothing lingers once the work is done"
        );
    }

    /// `<C-g>` stops keyed work that may be killed, and makes its result
    /// stale — but never a run-to-completion task.
    #[test]
    fn cancel_reaches_supersede_tasks_and_spares_run_to_completion_ones() {
        let fc = service();
        let build = fc
            .begin(TaskKey::supersede("plugin:update", "org"))
            .unwrap();
        let commit = fc
            .begin(TaskKey::run_to_completion("magit:commit", "lattice"))
            .unwrap();

        fc.cancel();

        assert!(build.token().is_cancelled());
        assert!(!build.is_current());
        assert!(
            !commit.token().is_cancelled(),
            "killing a commit midway is unsafe"
        );
        assert!(commit.is_current());

        // A re-trigger after `<C-g>` starts cleanly.
        let again = fc
            .begin(TaskKey::supersede("plugin:update", "org"))
            .unwrap();
        assert!(again.is_current());
    }

    /// A ticket outliving the service (shutdown) reports stale and drops
    /// cleanly rather than panicking.
    #[test]
    fn a_ticket_outliving_the_service_is_stale() {
        let fc = service();
        let ticket = fc.begin(TaskKey::supersede("search", "view 1")).unwrap();
        drop(fc);
        assert!(!ticket.is_current());
        drop(ticket);
    }
}
