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
//! The store lives in the App, not in the registry, because:
//! - the registry is `Clone` (cheap, shallow over `Arc<dyn DynMode>`)
//!   and shared across buffers; per-buffer Guard storage cannot
//!   be cloned (`Box<dyn Any>` is not `Clone`);
//! - the App is the single owner of buffer-keyed state and can
//!   purge a buffer's Guards on buffer deletion in one place.

use std::any::Any;
use std::collections::HashMap;

use lattice_protocol::ids::BufferId;

use crate::mode::ModeId;

/// Type-erased per-`(buffer, mode)` Guard storage.
///
/// Construct one per App (the dispatcher takes `&mut GuardStore`
/// on every activation / deactivation call). Default is empty.
///
/// Not `Clone` -- `Box<dyn Any>` is not `Clone`. The App owns
/// exactly one, passes it `&mut` to the dispatcher.
#[derive(Default)]
pub struct GuardStore {
    map: HashMap<(BufferId, ModeId), Box<dyn Any + Send>>,
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
    /// `Drop` impl); the dispatcher prevents this in practice
    /// by checking `active_modes` before activating.
    pub fn insert(&mut self, buffer: BufferId, mode: ModeId, guard: Box<dyn Any + Send>) {
        self.map.insert((buffer, mode), guard);
    }

    /// Take ownership of the Guard for `(buffer, mode)` and
    /// return it; the caller drops it (firing `Drop`). Returns
    /// `None` if no Guard was stashed -- legitimate when the
    /// mode wasn't active.
    pub fn remove(&mut self, buffer: BufferId, mode: ModeId) -> Option<Box<dyn Any + Send>> {
        self.map.remove(&(buffer, mode))
    }

    /// Drop every Guard belonging to `buffer`. Call when a
    /// buffer is deleted -- the dispatcher's normal
    /// deactivation path may not run if the buffer vanishes
    /// before the App can deactivate its modes.
    pub fn purge_buffer(&mut self, buffer: BufferId) {
        self.map.retain(|(b, _), _| *b != buffer);
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
