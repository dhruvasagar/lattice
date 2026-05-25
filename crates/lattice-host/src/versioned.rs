//! `Versioned<T>` — a tiny newtype wrapper that bumps a monotonic
//! `u64` version on every `&mut` access via `DerefMut`.
//!
//! ## Why this exists
//!
//! Perf plan B.4: identity-preserving Arc publish for unchanged
//! `RenderState` sub-states. Every `Editor::build_render_state`
//! tick today freshly `Arc::new`s every sub-state struct even when
//! the backing data hasn't moved. Most keystrokes don't touch
//! `pane_tree` / `active_modes` / `buffer_locals` / `pane_highlights` /
//! `lsp_progress`, so reusing the prior Arc when nothing changed
//! drops both the outer allocation and the inner map / tree clones
//! they would otherwise produce.
//!
//! The cache lookup needs a cheap-to-compare key. The naive option
//! ("content-hash on every publish") makes the hash cost the
//! rebuild cost — wash. The alternative ("dirty-flag set by every
//! mutator") is bulletproof in spec but easy to forget at one of
//! the ~30 mutation sites and fails silently when missed.
//!
//! `Versioned<T>` takes the third path: any code that obtains a
//! `&mut` to the inner data goes through `DerefMut`, which
//! increments the counter atomically with the access. There is no
//! way to mutate through a `Versioned<T>` without bumping. Reads
//! (`Deref`) don't bump. The cost is one `u64` add per `&mut`
//! borrow — sub-nanosecond, dwarfed by whatever mutation follows.
//!
//! ## Trade-offs
//!
//! - Over-bumps on read-then-no-op-mutate (e.g. `.iter_mut()` that
//!   the caller never actually writes through) cause a spurious
//!   cache miss the next publish. Safe; just costs one extra
//!   rebuild of that sub-state. Bounded.
//! - Field-assignment (`self.field = ...`) is not a `DerefMut` —
//!   it replaces the wrapper entirely. The `From<T>` /
//!   `Versioned::new` constructors zero the version, so the next
//!   `build_render_state` will rebuild correctly. Use [`Self::replace`]
//!   when you want to bump rather than reset.
//! - Single-threaded only by design. The wrapper isn't atomic; it
//!   relies on `Editor` being mutated from one thread (the actor).
//!   If you need cross-thread mutation, you have other problems —
//!   talk to the actor instead of mutating Editor directly.

use std::ops::{Deref, DerefMut};

/// Newtype wrapper that bumps a `u64` version counter on every
/// `DerefMut` access. See module docs for the rationale.
#[derive(Debug, Default, Clone)]
pub struct Versioned<T> {
    inner: T,
    version: u64,
}

impl<T> Versioned<T> {
    /// Wrap a value at version 0.
    pub fn new(inner: T) -> Self {
        Self { inner, version: 0 }
    }

    /// Current version. Cache consumers compare against the version
    /// stored alongside the cached output to decide whether to
    /// reuse.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Replace the inner value AND bump the version (whereas a
    /// plain `*v = Versioned::new(x)` would reset to 0). Use when
    /// you need the swap to invalidate downstream caches.
    pub fn replace(&mut self, value: T) -> T {
        // Bump first so a panic in `mem::replace` doesn't leave us
        // with stale (data, version) pairing.
        self.version = self.version.wrapping_add(1);
        std::mem::replace(&mut self.inner, value)
    }

    /// Unwrap to the underlying value, discarding the version.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> Deref for Versioned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for Versioned<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.version = self.version.wrapping_add(1);
        &mut self.inner
    }
}

impl<T> From<T> for Versioned<T> {
    fn from(inner: T) -> Self {
        Self::new(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn new_starts_at_version_zero() {
        let v: Versioned<i32> = Versioned::new(42);
        assert_eq!(v.version(), 0);
        assert_eq!(*v, 42);
    }

    #[test]
    fn deref_does_not_bump() {
        let v: Versioned<i32> = Versioned::new(7);
        let _ = *v;
        let _ = *v;
        let _ = v.clone();
        assert_eq!(v.version(), 0);
    }

    #[test]
    fn deref_mut_bumps_each_access() {
        let mut v: Versioned<i32> = Versioned::new(7);
        *v += 1;
        assert_eq!(v.version(), 1);
        *v += 1;
        assert_eq!(v.version(), 2);
    }

    #[test]
    fn replace_bumps_and_returns_old() {
        let mut v: Versioned<String> = Versioned::new("a".into());
        let old = v.replace("b".into());
        assert_eq!(old, "a");
        assert_eq!(*v, "b");
        assert_eq!(v.version(), 1);
    }

    #[test]
    fn into_inner_discards_version() {
        let mut v: Versioned<i32> = Versioned::new(1);
        *v = 2;
        assert_eq!(v.version(), 1);
        let inner = v.into_inner();
        assert_eq!(inner, 2);
    }

    #[test]
    fn hashmap_mutator_through_autoref_bumps() {
        // The whole point of the wrapper: an existing call like
        // `self.field.insert(k, v)` autorefs `&mut self.field`,
        // which fires DerefMut and bumps the version. No call-site
        // change needed.
        let mut v: Versioned<HashMap<u32, u32>> = Versioned::new(HashMap::new());
        v.insert(1, 10);
        assert_eq!(v.version(), 1);
        v.insert(2, 20);
        assert_eq!(v.version(), 2);
        let _len = v.len();
        let _val = v.get(&1);
        assert_eq!(v.version(), 2);
    }

    #[test]
    fn from_t_constructs_at_version_zero() {
        let v: Versioned<i32> = 5.into();
        assert_eq!(v.version(), 0);
        assert_eq!(*v, 5);
    }
}
