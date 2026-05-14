//! `GuardStore`: type-erased storage for per-`(buffer, mode)`
//! Guards returned by [`Mode::on_activate`](crate::Mode::on_activate).
//!
//! The dispatcher stashes each successful activation's Guard in
//! this store, keyed by `(BufferId, ModeId)`. On deactivation,
//! the dispatcher removes the entry and drops the boxed Guard;
//! the Guard's `Drop` impl performs cleanup (unsubscribe,
//! restore prior option value, drop supervisor handle, ...).
//!
//! Storage is `Box<dyn Any + Send>` because each mode's Guard
//! type is different. The dispatcher never downcasts -- dropping
//! through the `dyn Any` trait object correctly invokes the
//! original type's `Drop` via the vtable.
//!
//! M-async.2: the store is accessed from two threads -- the App
//! thread (synchronous deactivate path; activation's sync
//! prefix) and the tokio worker that runs the spawned lifecycle
//! future (inserts the Guard when `on_activate` resolves). The
//! [`GuardStoreHandle`] wraps the store in `Arc<Mutex<...>>` so
//! both threads can lock briefly without `&mut` lifetime
//! gymnastics. The App owns one handle; the dispatcher clones
//! it into each spawned task.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_protocol::ids::BufferId;

use crate::mode::ModeId;

/// Type-erased per-`(buffer, mode)` Guard storage.
///
/// Construct one per App (the dispatcher takes `&mut GuardStore`
/// on every activation / deactivation call). Default is empty.
///
/// Not `Clone` -- `Box<dyn Any>` is not `Clone`. The App owns
/// exactly one, passes it `&mut` to the dispatcher.
///
/// **M-async.4 epoch counter:** each `(buffer, mode)` key
/// carries a `u64` epoch that monotonically increments on every
/// activate begin + every deactivate. The dispatcher's spawn
/// task captures the epoch when it queues, then validates
/// against the current epoch via [`Self::try_insert`] before
/// stashing its Guard. A mismatch means a deactivate (or a
/// later activate) arrived while the spawn was in flight; the
/// returned `Err(stale_guard)` lets the spawn drop the Guard
/// (firing its Drop for out-of-band cleanup) instead of
/// stashing it in a logically-inactive store slot.
#[derive(Default)]
pub struct GuardStore {
    map: HashMap<(BufferId, ModeId), Box<dyn Any + Send>>,
    epochs: HashMap<(BufferId, ModeId), u64>,
}

impl std::fmt::Debug for GuardStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardStore")
            .field("count", &self.map.len())
            .finish_non_exhaustive()
    }
}

impl GuardStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stash a Guard for the given `(buffer, mode)`. Replaces
    /// any existing entry (the old Guard is dropped, firing its
    /// `Drop` impl). Used in tests + the rare reload path; the
    /// production dispatcher routes through [`Self::try_insert`]
    /// to respect the epoch invariant.
    pub fn insert(&mut self, buffer: BufferId, mode: ModeId, guard: Box<dyn Any + Send>) {
        self.map.insert((buffer, mode), guard);
    }

    /// Bump the epoch for `(buffer, mode)` and return the new
    /// value. The dispatcher's sync prefix calls this when
    /// queueing a step; the spawn task captures the returned
    /// value and passes it to [`Self::try_insert`] on
    /// completion. Wraps on overflow (`u64::MAX → 0`); the
    /// dispatcher tolerates this because consecutive bumps
    /// always advance by 1, so a wrap that happens to land on
    /// a stale spawn's captured epoch would require 2^64
    /// activate / deactivate cycles in flight -- not a
    /// realistic concern.
    pub fn bump_epoch(&mut self, buffer: BufferId, mode: ModeId) -> u64 {
        let entry = self.epochs.entry((buffer, mode)).or_insert(0);
        *entry = entry.wrapping_add(1);
        *entry
    }

    /// Current epoch for `(buffer, mode)`. `0` if the pair has
    /// never had an activation queued. Used by tests + the
    /// dispatcher's spawn task to validate before stashing.
    pub fn current_epoch(&self, buffer: BufferId, mode: ModeId) -> u64 {
        self.epochs.get(&(buffer, mode)).copied().unwrap_or(0)
    }

    /// Insert `guard` only if `my_epoch` still matches the
    /// store's current epoch for `(buffer, mode)`. Returns
    /// `Ok(())` on success; on epoch mismatch returns
    /// `Err(guard)` so the caller can drop the Guard outside
    /// the lock (the Box's `Drop` then fires the original
    /// type's cleanup).
    ///
    /// Used by the M-async.4 spawn-task path: a deactivate
    /// (or a subsequent activate) arriving while a spawn was
    /// in flight bumps the epoch via [`Self::remove`] /
    /// [`Self::bump_epoch`]; the spawn's late `try_insert`
    /// then fails the match and drops the Guard instead of
    /// stashing it in a logically-inactive store slot.
    pub fn try_insert(
        &mut self,
        buffer: BufferId,
        mode: ModeId,
        my_epoch: u64,
        guard: Box<dyn Any + Send>,
    ) -> Result<(), Box<dyn Any + Send>> {
        if self.current_epoch(buffer, mode) == my_epoch {
            self.map.insert((buffer, mode), guard);
            Ok(())
        } else {
            Err(guard)
        }
    }

    /// Take ownership of the Guard for `(buffer, mode)`,
    /// bumping the epoch so any in-flight spawn that hasn't
    /// inserted yet fails its [`Self::try_insert`] check.
    /// Returns `None` if no Guard was stashed.
    pub fn remove(&mut self, buffer: BufferId, mode: ModeId) -> Option<Box<dyn Any + Send>> {
        // Bump first so a spawn task's later try_insert (after
        // its on_activate.await resolves) sees the mismatch
        // regardless of whether a Guard was already present.
        self.bump_epoch(buffer, mode);
        self.map.remove(&(buffer, mode))
    }

    /// Drop every Guard belonging to `buffer`. Call when a
    /// buffer is deleted -- the dispatcher's normal
    /// deactivation path may not run if the buffer vanishes
    /// before the App can deactivate its modes. Bumps the
    /// epoch for every `(buffer, *)` entry so any in-flight
    /// spawn for the purged buffer fails its later
    /// [`Self::try_insert`].
    pub fn purge_buffer(&mut self, buffer: BufferId) {
        self.map.retain(|(b, _), _| *b != buffer);
        for ((b, _), epoch) in self.epochs.iter_mut() {
            if *b == buffer {
                *epoch = epoch.wrapping_add(1);
            }
        }
    }

    /// Number of stashed Guards.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// True iff a Guard is stashed for `(buffer, mode)`.
    pub fn contains(&self, buffer: BufferId, mode: ModeId) -> bool {
        self.map.contains_key(&(buffer, mode))
    }
}

/// Cheap-clone, thread-safe handle to a [`GuardStore`]. The App
/// owns one; the dispatcher clones it into each spawned
/// lifecycle task so the task can lock + insert the Guard on
/// completion. Locks are held briefly (single map mutation per
/// activation / deactivation); `std::sync::Mutex` is correct
/// because no `.await` happens inside the lock.
#[derive(Clone, Default)]
pub struct GuardStoreHandle {
    inner: Arc<Mutex<GuardStore>>,
}

impl std::fmt::Debug for GuardStoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        match guard {
            Ok(g) => f
                .debug_struct("GuardStoreHandle")
                .field("count", &g.len())
                .finish_non_exhaustive(),
            Err(_) => f.debug_struct("GuardStoreHandle").finish_non_exhaustive(),
        }
    }
}

impl GuardStoreHandle {
    /// Fresh empty handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stash a Guard unconditionally (skipping the epoch
    /// check). Used in tests + the rare reload path; the
    /// production dispatcher routes through
    /// [`Self::try_insert`].
    pub fn insert(&self, buffer: BufferId, mode: ModeId, guard: Box<dyn Any + Send>) {
        if let Ok(mut store) = self.inner.lock() {
            store.insert(buffer, mode, guard);
        }
    }

    /// Bump + return the new epoch for `(buffer, mode)`. The
    /// dispatcher's sync prefix calls this when queueing each
    /// cascade step; the spawn task captures it and passes
    /// back to [`Self::try_insert`].
    pub fn bump_epoch(&self, buffer: BufferId, mode: ModeId) -> u64 {
        self.inner
            .lock()
            .map(|mut store| store.bump_epoch(buffer, mode))
            .unwrap_or(0)
    }

    /// Insert iff `my_epoch` still matches the current
    /// epoch. Returns `Err(guard)` on stale; the caller drops
    /// the Guard outside the lock so the original type's
    /// `Drop` fires.
    pub fn try_insert(
        &self,
        buffer: BufferId,
        mode: ModeId,
        my_epoch: u64,
        guard: Box<dyn Any + Send>,
    ) -> Result<(), Box<dyn Any + Send>> {
        match self.inner.lock() {
            Ok(mut store) => store.try_insert(buffer, mode, my_epoch, guard),
            // Poisoned mutex: treat as "stale" so caller drops.
            Err(_) => Err(guard),
        }
    }

    /// Take ownership of the Guard. Bumps the epoch
    /// (invalidating any in-flight spawn) then removes; the
    /// caller drops the returned `Box`, firing the Guard's
    /// `Drop` impl *outside* the lock.
    pub fn remove(&self, buffer: BufferId, mode: ModeId) -> Option<Box<dyn Any + Send>> {
        self.inner.lock().ok()?.remove(buffer, mode)
    }

    /// Drop every Guard for `buffer`. Used when a buffer is
    /// deleted.
    pub fn purge_buffer(&self, buffer: BufferId) {
        if let Ok(mut store) = self.inner.lock() {
            store.purge_buffer(buffer);
        }
    }

    /// True iff a Guard is stashed for `(buffer, mode)`.
    pub fn contains(&self, buffer: BufferId, mode: ModeId) -> bool {
        self.inner
            .lock()
            .map(|s| s.contains(buffer, mode))
            .unwrap_or(false)
    }

    /// Number of stashed Guards.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|s| s.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Test Guard that increments a counter on drop, proving
    /// `Box<dyn Any + Send>::drop` correctly invokes the
    /// original type's `Drop` via the vtable.
    struct DropCounter {
        count: Arc<AtomicU32>,
    }
    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn insert_then_remove_drops_guard() {
        let count = Arc::new(AtomicU32::new(0));
        let mut store = GuardStore::new();
        store.insert(
            BufferId::new(1),
            ModeId::new("x-mode"),
            Box::new(DropCounter {
                count: count.clone(),
            }),
        );
        // Drop hasn't fired yet -- Guard is owned by the store.
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let removed = store.remove(BufferId::new(1), ModeId::new("x-mode"));
        // Box is now in `removed`; still hasn't dropped.
        assert!(removed.is_some());
        // Dropping the removed Box fires the original Guard's Drop.
        drop(removed);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn remove_missing_returns_none() {
        let mut store = GuardStore::new();
        assert!(
            store
                .remove(BufferId::new(1), ModeId::new("x-mode"))
                .is_none()
        );
    }

    #[test]
    fn purge_buffer_drops_every_mode_for_that_buffer() {
        let c1 = Arc::new(AtomicU32::new(0));
        let c2 = Arc::new(AtomicU32::new(0));
        let other = Arc::new(AtomicU32::new(0));
        let mut store = GuardStore::new();
        store.insert(
            BufferId::new(1),
            ModeId::new("a-mode"),
            Box::new(DropCounter { count: c1.clone() }),
        );
        store.insert(
            BufferId::new(1),
            ModeId::new("b-mode"),
            Box::new(DropCounter { count: c2.clone() }),
        );
        store.insert(
            BufferId::new(2),
            ModeId::new("a-mode"),
            Box::new(DropCounter {
                count: other.clone(),
            }),
        );
        store.purge_buffer(BufferId::new(1));
        // Buffer 1's two Guards dropped; buffer 2 intact.
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
        assert_eq!(other.load(Ordering::SeqCst), 0);
        assert_eq!(store.len(), 1);
        assert!(store.contains(BufferId::new(2), ModeId::new("a-mode")));
    }

    #[test]
    fn insert_replaces_existing_and_drops_old() {
        let old = Arc::new(AtomicU32::new(0));
        let new = Arc::new(AtomicU32::new(0));
        let mut store = GuardStore::new();
        store.insert(
            BufferId::new(1),
            ModeId::new("x-mode"),
            Box::new(DropCounter {
                count: old.clone(),
            }),
        );
        store.insert(
            BufferId::new(1),
            ModeId::new("x-mode"),
            Box::new(DropCounter {
                count: new.clone(),
            }),
        );
        // Old Guard dropped at replacement time.
        assert_eq!(old.load(Ordering::SeqCst), 1);
        assert_eq!(new.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unit_guard_works() {
        // Marker modes use `Guard = ()`; storing/removing it
        // must not panic.
        let mut store = GuardStore::new();
        store.insert(BufferId::new(1), ModeId::new("marker-mode"), Box::new(()));
        let g = store.remove(BufferId::new(1), ModeId::new("marker-mode"));
        assert!(g.is_some());
    }
}
