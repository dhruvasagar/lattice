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
use crate::origin::OptionOrigin;
use crate::overrides::OptionKey;

/// Cached snapshot of the resolved value for every option a
/// buffer reads. Built by the [`crate::Resolver`]; invalidated
/// on mode toggle, option write, or modal-state transition;
/// recomputed eagerly on invalidation per the v1 invalidation
/// policy (`mode-architecture.md` §6.3.1).
///
/// Public read API: [`Self::get`] (typed), [`Self::get_origin`]
/// (layer that supplied the value). The internal storage is
/// `pub(crate)` so the resolver in this crate can populate it;
/// external code reads through the typed accessors.
#[derive(Debug, Default, Clone)]
pub struct ResolvedOptions {
    by_type: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    /// Parallel map: which layer each option's winning value came from.
    /// Entries are inserted in lockstep with `by_type`; missing entries
    /// default to [`OptionOrigin::Default`].
    origins: HashMap<TypeId, OptionOrigin>,
    /// RF.4e′: plugin options a layer overrode, by canonical name, with the
    /// layer that won. Separate from `by_type` so the native typed read stays
    /// one `TypeId` lookup. Not seeded: a plugin option absent here has no
    /// override in this buffer, and reads its global value.
    by_name: HashMap<Arc<str>, (Arc<dyn Any + Send + Sync>, OptionOrigin)>,
}

impl ResolvedOptions {
    /// An empty cache. Every [`Self::get`] returns `None` until it is
    /// seeded (typically by
    /// [`crate::ConfigRegistry::bootstrap_resolved_with_current_values`])
    /// and overlaid by a [`crate::Resolver`].
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

    /// Test helper: insert a resolved value directly. Used by
    /// tests across crates that exercise the read path without
    /// running the resolver. Production code fills the cache through
    /// the [`crate::Resolver`] and
    /// [`crate::ConfigRegistry::bootstrap_resolved_with_current_values`],
    /// which also record an [`OptionOrigin`]; this does not, so the
    /// entry's origin stays whatever it was (default:
    /// [`OptionOrigin::Default`]).
    pub fn insert<T: OptionDecl>(&mut self, value: T::Value)
    where
        T::Value: Send + Sync + 'static,
    {
        self.by_type.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Erased insert with explicit origin. Used by the resolver's
    /// origin-aware path and by `bootstrap_resolved_with_current_values`.
    pub(crate) fn insert_erased_with_origin(
        &mut self,
        key: OptionKey,
        value: Arc<dyn Any + Send + Sync>,
        origin: OptionOrigin,
    ) {
        match key {
            OptionKey::Native(type_id) => {
                self.by_type.insert(type_id, value);
                self.origins.insert(type_id, origin);
            }
            OptionKey::Plugin(name) => {
                self.by_name.insert(name, (value, origin));
            }
        }
    }

    /// Look up the origin for option type `T`. Returns
    /// [`OptionOrigin::Default`] if the option wasn't resolved in this
    /// cycle (unknown option / resolver hasn't run yet).
    pub fn get_origin<T: OptionDecl>(&self) -> OptionOrigin {
        self.origins
            .get(&TypeId::of::<T>())
            .cloned()
            .unwrap_or_default()
    }

    /// Key-based origin lookup for sites that only have a runtime key
    /// (e.g. the query echo path in `do_set`). [`OptionOrigin::Default`]
    /// when nothing resolved it, which for a plugin option means no layer
    /// overrode it.
    pub fn get_origin_for(&self, key: &OptionKey) -> OptionOrigin {
        match key {
            OptionKey::Native(type_id) => self.origins.get(type_id).cloned(),
            OptionKey::Plugin(name) => self.by_name.get(name).map(|(_, o)| o.clone()),
        }
        .unwrap_or_default()
    }

    /// Key-based erased value lookup. Used by the query echo path
    /// to format the resolved value when the concrete type isn't
    /// statically known. `None` for a plugin option no layer overrode:
    /// its value is the global one.
    pub fn get_erased(&self, key: &OptionKey) -> Option<&Arc<dyn Any + Send + Sync>> {
        match key {
            OptionKey::Native(type_id) => self.by_type.get(type_id),
            OptionKey::Plugin(name) => self.by_name.get(name).map(|(v, _)| v),
        }
    }

    /// Number of entries (for tests and `:describe-buffer` /
    /// introspection diagnostic output).
    pub fn len(&self) -> usize {
        self.by_type.len() + self.by_name.len()
    }

    /// `true` when no option has been resolved into the cache.
    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty() && self.by_name.is_empty()
    }

    /// Iterate over the native `(TypeId, erased Arc)` pairs. Used by
    /// `:describe-option-resolution` (M.8) and tests. Plugin options a
    /// layer overrode are not here; read them by key ([`Self::get_erased`]).
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
