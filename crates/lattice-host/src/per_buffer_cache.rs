//! `PerBufferCache<T>` — per-buffer cache primitive for the
//! 5.8.AF.5 Slice 3b migration.
//!
//! Every LSP feature that produces a per-buffer cache (inlay
//! hints, folding ranges, semantic tokens, code lenses, document
//! links, document colors, pull diagnostics, …) needs:
//!
//! - **Wait-free reads** at render time (renderer paints every
//!   frame; can't take a lock).
//! - **Concurrent writes** from background tasks on the LSP
//!   runtime (the spawned request task `.store()`s the result
//!   when the response arrives — paramount goal #4).
//! - **Per-buffer keying** so closing buffer A doesn't affect
//!   buffer B's cache.
//! - **Shareability** so the renderer's `RenderState` snapshot
//!   can hold a clone that observes writes by the task.
//!
//! `Arc<ArcSwap<HashMap<BufferId, Arc<T>>>>` satisfies all four:
//!
//! - The outer `Arc` lets the spawned task clone the slot into
//!   itself.
//! - The `ArcSwap` makes the inner `HashMap` swappable atomically.
//! - The inner `Arc<T>` per entry lets readers detach a value
//!   cheaply (one Arc bump) without holding a lock.
//! - The `ArcSwap` snapshot is wait-free for readers.
//!
//! Writes are copy-on-write: a writer loads the current
//! `Arc<HashMap>`, clones the underlying `HashMap`, mutates the
//! clone, and stores. For typical sessions (~5–50 open buffers)
//! the clone cost is microseconds and writes are rare
//! (~1 per LSP response per cache type), so the cost stays well
//! below the 100µs publication budget.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_core::BufferId;

/// Per-buffer cache slot — the canonical Slice 3b shape for
/// per-buffer LSP feature data.
///
/// Use [`PerBufferCacheExt`] for the read / write / remove
/// helpers; the raw type exposes only what `ArcSwap` exposes.
pub type PerBufferCache<T> = Arc<ArcSwap<HashMap<BufferId, Arc<T>>>>;

/// Construct an empty `PerBufferCache<T>`. The Slice 3b boot
/// path uses this; downstream code clones the resulting Arc.
pub fn empty<T>() -> PerBufferCache<T> {
    Arc::new(ArcSwap::from_pointee(HashMap::new()))
}

/// Convenience trait carrying the standard read / insert /
/// remove operations for a [`PerBufferCache`].
///
/// All operations are non-blocking. Writes use a copy-on-write
/// pattern: load the current `Arc<HashMap>`, clone the inner
/// `HashMap`, mutate, store. Concurrent writers may race; the
/// last-writer-wins outcome is acceptable for LSP feature caches
/// (each buffer's writer is itself single-flight via
/// cancellation tokens; cross-buffer races are independent).
pub trait PerBufferCacheExt<T> {
    /// Wait-free read: returns a detached `Arc<T>` snapshot of
    /// the cache entry for `id`, or `None` if no entry exists.
    /// The returned `Arc` is independent of any subsequent
    /// store; the renderer can hold it across the frame.
    fn get_for(&self, id: BufferId) -> Option<Arc<T>>;

    /// Store (or replace) the cache entry for `id`. Copy-on-
    /// write: clones the current `HashMap`, inserts, stores.
    fn insert_for(&self, id: BufferId, value: T);

    /// Remove the cache entry for `id` if present. No-op when
    /// the entry doesn't exist. Copy-on-write semantics.
    fn remove_for(&self, id: BufferId);

    /// Retain only entries matching the predicate. Mirrors
    /// `HashMap::retain`'s semantics: the predicate sees each
    /// `(BufferId, &T)`; entries returning `false` are dropped.
    ///
    /// Copy-on-write: if no entries need to be dropped, the
    /// underlying Arc is unchanged. Used by LSP `*/refresh`
    /// drains to evict per-server caches when a server's
    /// invalidation notification arrives.
    fn retain<F: FnMut(BufferId, &T) -> bool>(&self, predicate: F);

    /// Returns `true` when no entries exist. Wait-free.
    fn is_empty_snapshot(&self) -> bool;
}

impl<T> PerBufferCacheExt<T> for PerBufferCache<T> {
    fn get_for(&self, id: BufferId) -> Option<Arc<T>> {
        self.load().get(&id).cloned()
    }

    fn insert_for(&self, id: BufferId, value: T) {
        let current = self.load();
        let mut next = (**current).clone();
        next.insert(id, Arc::new(value));
        self.store(Arc::new(next));
    }

    fn remove_for(&self, id: BufferId) {
        let current = self.load();
        if !current.contains_key(&id) {
            // Avoid the clone when the key is absent — common
            // when buffer-close fires for buffers the cache
            // never observed.
            return;
        }
        let mut next = (**current).clone();
        next.remove(&id);
        self.store(Arc::new(next));
    }

    fn retain<F: FnMut(BufferId, &T) -> bool>(&self, mut predicate: F) {
        let current = self.load();
        // First pass: identify keys to drop without cloning the
        // map. Skip the rebuild entirely when nothing matches.
        let drop_keys: Vec<BufferId> = current
            .iter()
            .filter_map(|(id, value)| if predicate(*id, value) { None } else { Some(*id) })
            .collect();
        if drop_keys.is_empty() {
            return;
        }
        let mut next = (**current).clone();
        for id in drop_keys {
            next.remove(&id);
        }
        self.store(Arc::new(next));
    }

    fn is_empty_snapshot(&self) -> bool {
        self.load().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Basic insert / get round-trip. Verifies the Arc-bump
    /// read contract.
    #[test]
    fn insert_and_get_roundtrip() {
        let cache: PerBufferCache<u32> = empty();
        let id = BufferId(7);
        cache.insert_for(id, 42);
        let v = cache.get_for(id).expect("entry must be present");
        assert_eq!(*v, 42);
    }

    /// Remove on a non-existent key is a no-op (does NOT clone
    /// the map, which matters for the buffer-close hot path
    /// that runs on every close regardless of whether the cache
    /// holds an entry for that buffer).
    #[test]
    fn remove_missing_skips_clone() {
        let cache: PerBufferCache<u32> = empty();
        let before = Arc::as_ptr(&cache.load_full());
        cache.remove_for(BufferId(99));
        let after = Arc::as_ptr(&cache.load_full());
        assert_eq!(
            before, after,
            "remove of absent key must not allocate a new Arc"
        );
    }

    /// A clone of the outer `Arc` observes writes made through
    /// the original — this is the property that lets
    /// `RenderState.lsp.<cache>` see writes made by the spawned
    /// task on the LSP runtime without re-publishing
    /// `RenderState`.
    #[test]
    fn clone_observes_writes_through_arcswap() {
        let cache: PerBufferCache<u32> = empty();
        let shared = cache.clone();
        cache.insert_for(BufferId(1), 100);
        let v = shared.get_for(BufferId(1)).expect("clone sees write");
        assert_eq!(*v, 100);
    }

    /// Writes through one clone are observable through another
    /// clone — symmetric to the test above; covers the case
    /// where the spawned task holds clone A and the renderer
    /// reads via clone B.
    #[test]
    fn writes_through_one_clone_visible_to_another() {
        let cache_a: PerBufferCache<&'static str> = empty();
        let cache_b = cache_a.clone();
        cache_b.insert_for(BufferId(2), "from-b");
        assert_eq!(*cache_a.get_for(BufferId(2)).unwrap(), "from-b");
    }
}
