//! `ResolvedOptions`: per-buffer cached snapshot of every option's
//! current resolved value (`mode-architecture.md` §6.3).
//!
//! Reads are O(1) `TypeId` lookups against a [`HashMap`]; the
//! resolver populates the cache once per invalidation cycle.
//! No layer walk on the keystroke path.
//!
//! Storage: erased through `Arc<dyn Any + Send + Sync>` so
//! options of different `Value` types coexist in the same map.
//! Typed reads downcast to the option's `Value` type at access.
//! The downcast is infallible after a successful resolution
//! (the resolver constructs entries with the correct type) but
//! we still return `Option<T>` from the typed read because (a)
//! the option may not be registered, and (b) the Rust API
//! convention for "could be missing" is `Option`, not panic.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::option_decl::OptionDecl;

/// Cached snapshot of the resolved value for every option a
/// buffer reads. Built by the [`crate::Resolver`]; invalidated
/// on mode toggle, option write, or modal-state transition;
/// recomputed eagerly on invalidation per the v1 invalidation
/// policy (`mode-architecture.md` §6.3.1).
///
/// Public read API: [`Self::get`] (typed). The internal
/// `Arc<dyn Any>` storage is `pub(crate)` so the resolver in
/// this crate can populate it; external code reads through
/// `get`.
#[derive(Debug, Default, Clone)]
pub struct ResolvedOptions {
    by_type: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ResolvedOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the resolved value for option type `T`. Returns
    /// `None` if `T` was not part of this resolution cycle
    /// (e.g. the option isn't registered, or the resolver
    /// hasn't run yet).
    pub fn get<T: OptionDecl>(&self) -> Option<Arc<T::Value>>
    where
        T::Value: Send + Sync + 'static,
    {
        let any = self.by_type.get(&TypeId::of::<T>())?;
        // The resolver always inserts `Arc<T::Value>`; this
        // downcast is infallible by construction. We still
        // return Option to avoid panicking when an option is
        // unregistered (legitimate transient state during
        // crate boot).
        any.clone().downcast::<T::Value>().ok()
    }

    /// Test-only helper: insert a resolved value directly. Used
    /// by tests that exercise the read path without running the
    /// resolver. Production code uses [`Self::insert_erased`]
    /// from the resolver.
    #[cfg(test)]
    pub fn insert<T: OptionDecl>(&mut self, value: T::Value)
    where
        T::Value: Send + Sync + 'static,
    {
        self.by_type.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Erased insert used by [`crate::Resolver`]. The caller
    /// owns the type-correct construction; the cache stores
    /// erased.
    pub(crate) fn insert_erased(&mut self, type_id: TypeId, value: Arc<dyn Any + Send + Sync>) {
        self.by_type.insert(type_id, value);
    }

    /// Number of entries (for tests and `:describe-buffer` /
    /// introspection diagnostic output).
    pub fn len(&self) -> usize {
        self.by_type.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty()
    }

    /// Iterate over `(TypeId, erased Arc)` pairs. Used by
    /// `:describe-option-resolution` (M.8) and tests.
    pub fn iter(&self) -> impl Iterator<Item = (&TypeId, &Arc<dyn Any + Send + Sync>)> {
        self.by_type.iter()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::option_decl::HasGroup;

    struct Tabstop;
    impl OptionDecl for Tabstop {
        type Value = i64;
        const NAME: &'static str = "test-tabstop";
        const DOC: &'static str = "";
        fn default_value() -> i64 {
            8
        }
    }
    impl HasGroup for Tabstop {
        const GROUP_NAME: &'static str = "editor";
    }

    struct Number;
    impl OptionDecl for Number {
        type Value = bool;
        const NAME: &'static str = "test-number";
        const DOC: &'static str = "";
        fn default_value() -> bool {
            false
        }
    }
    impl HasGroup for Number {
        const GROUP_NAME: &'static str = "editor";
    }

    #[test]
    fn empty_returns_none() {
        let r = ResolvedOptions::new();
        assert!(r.get::<Tabstop>().is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn insert_and_get_round_trips() {
        let mut r = ResolvedOptions::new();
        r.insert::<Tabstop>(4);
        let v = r.get::<Tabstop>().unwrap();
        assert_eq!(*v, 4);
    }

    #[test]
    fn distinct_options_coexist() {
        let mut r = ResolvedOptions::new();
        r.insert::<Tabstop>(2);
        r.insert::<Number>(true);
        assert_eq!(*r.get::<Tabstop>().unwrap(), 2);
        assert!(*r.get::<Number>().unwrap());
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn second_insert_for_same_type_overwrites() {
        let mut r = ResolvedOptions::new();
        r.insert::<Tabstop>(2);
        r.insert::<Tabstop>(4);
        assert_eq!(*r.get::<Tabstop>().unwrap(), 4);
        assert_eq!(r.len(), 1);
    }
}
