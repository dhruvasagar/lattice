//! Layered option overrides (M.2.1).
//!
//! Modes (and other layer producers — buffer-local sets,
//! modal-state hooks) contribute typed overrides via
//! [`OptionOverride`]. The resolver in `lattice-config` walks
//! the layers in priority order (`mode-architecture.md` §6.1)
//! and picks the first non-empty value per option for scalars;
//! collection-shaped options concatenate.
//!
//! ## Why this lives in `lattice-mode`, not `lattice-config`
//!
//! `Mode::options()` returns an [`OptionOverrideSet`]. If the
//! type lived in `lattice-config`, then `lattice-mode → lattice-
//! config → lattice-core → lattice-mode` would form a dependency
//! cycle. So the layer-input types live here (low layer) and the
//! resolver + cached output live in `lattice-config` (which
//! depends on `lattice-mode` for these inputs).
//!
//! ## Type-safe construction via `lattice-config`'s `overrides!`
//!
//! Modes don't construct [`OptionOverride`] directly with the
//! erased [`OptionOverride::new`] API. Instead they use the
//! `overrides!` macro from `lattice-config`, which has access
//! to `OptionDecl` and emits compile-time-typed wrappers around
//! [`OptionOverride::new`]:
//!
//! ```ignore
//! fn options(&self) -> OptionOverrideSet {
//!     lattice_config::overrides! {
//!         Tabstop = 4,
//!         Wrap = true,
//!     }
//! }
//! ```
//!
//! The macro asserts at compile time that each value matches its
//! declaration's `Value` type. Direct [`OptionOverride::new`]
//! is reserved for the WIT plugin adapter (M.10), where
//! declarations are runtime data and TypeId is the only handle.

use std::any::{Any, TypeId};
use std::sync::Arc;

use smallvec::SmallVec;

/// Tie-break priority for two layer entries that target the
/// same option.
///
/// Most modes use `Normal`; the registry picks last-activated
/// among `Normal`s and emits a `ModeEvent::OptionConflict`
/// event for visibility. `High` / `Low` are explicit overrides
/// for modes that genuinely need to win or lose regardless of
/// activation order (`read-only-mode` ⇒ `High` for
/// `writable=false`).
///
/// See `mode-architecture.md` §6.2 for the conflict policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OverridePriority {
    Low,
    Normal,
    High,
}

impl Default for OverridePriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// One option-value override from a single layer producer (a
/// mode, a buffer-local set, a modal-state hook).
///
/// Identity is the `option_type_id` — the `TypeId` of the
/// declaration type (e.g. `TypeId::of::<Tabstop>()`). The
/// resolver looks up the option by this id; the typed downcast
/// against the option's `Value` is performed when emitting the
/// resolved cache.
#[derive(Clone)]
pub struct OptionOverride {
    /// `TypeId` of the `OptionDecl` type this override targets.
    /// `OptionDecl` lives in `lattice-config`; we don't have
    /// access to the trait at this layer, so we identify by
    /// `TypeId` and trust the macro / consumer to construct
    /// type-correctly.
    pub option_type_id: TypeId,
    /// The override value, type-erased. Downcast at resolution
    /// time to the declaration's `Value` type.
    pub value: Arc<dyn Any + Send + Sync>,
    /// Tie-break priority. See [`OverridePriority`].
    pub priority: OverridePriority,
}

impl std::fmt::Debug for OptionOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionOverride")
            .field("option_type_id", &self.option_type_id)
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
}

impl OptionOverride {
    /// Erased constructor. The caller is responsible for
    /// supplying a `value` of the type matching the declaration's
    /// `Value` -- this contract is enforced by
    /// `lattice-config`'s `overrides!` macro at compile time
    /// (it generates a typed let-binding before the call). For
    /// runtime / plugin construction, the caller is responsible
    /// for honouring the contract.
    pub fn new<V: Clone + Send + Sync + 'static>(option_type_id: TypeId, value: V) -> Self {
        Self {
            option_type_id,
            value: Arc::new(value),
            priority: OverridePriority::default(),
        }
    }

    /// Same as [`Self::new`] but with explicit priority.
    pub fn with_priority<V: Clone + Send + Sync + 'static>(
        option_type_id: TypeId,
        value: V,
        priority: OverridePriority,
    ) -> Self {
        Self {
            option_type_id,
            value: Arc::new(value),
            priority,
        }
    }

    /// Promote `self` to a higher priority. Used by the
    /// `overrides!` macro's priority-attribute branch.
    pub fn at_priority(mut self, priority: OverridePriority) -> Self {
        self.priority = priority;
        self
    }

    /// Attempt to downcast the value to the requested type.
    /// Returns `None` if the stored value doesn't match `V`.
    pub fn downcast_value<V: 'static>(&self) -> Option<&V> {
        self.value.downcast_ref::<V>()
    }
}

/// A set of [`OptionOverride`]s contributed by one layer
/// producer (typically the return value of `Mode::options()`).
///
/// `SmallVec` keeps the typical case (0-4 overrides per mode)
/// inline; modes that contribute many overrides spill to the
/// heap. Layer-input data, not hot-path read data — the
/// resolver walks each set exactly once per
/// `recompute_options` call.
#[derive(Default, Clone)]
pub struct OptionOverrideSet {
    overrides: SmallVec<[OptionOverride; 4]>,
}

impl std::fmt::Debug for OptionOverrideSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionOverrideSet")
            .field("len", &self.overrides.len())
            .finish_non_exhaustive()
    }
}

impl OptionOverrideSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            overrides: SmallVec::with_capacity(cap),
        }
    }

    /// Append an override. Order within a set is preserved;
    /// the resolver visits elements in this order when merging.
    pub fn push(&mut self, ov: OptionOverride) {
        self.overrides.push(ov);
    }

    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &OptionOverride> {
        self.overrides.iter()
    }
}

impl FromIterator<OptionOverride> for OptionOverrideSet {
    fn from_iter<I: IntoIterator<Item = OptionOverride>>(iter: I) -> Self {
        let mut set = Self::new();
        for ov in iter {
            set.push(ov);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OptionA;
    struct OptionB;

    #[test]
    fn override_carries_typed_value() {
        let ov = OptionOverride::new(TypeId::of::<OptionA>(), 42i64);
        assert_eq!(ov.option_type_id, TypeId::of::<OptionA>());
        assert_eq!(ov.priority, OverridePriority::Normal);
        assert_eq!(ov.downcast_value::<i64>().copied(), Some(42));
    }

    #[test]
    fn downcast_to_wrong_type_returns_none() {
        let ov = OptionOverride::new(TypeId::of::<OptionA>(), 42i64);
        assert!(ov.downcast_value::<bool>().is_none());
    }

    #[test]
    fn override_targets_correct_type() {
        let a = OptionOverride::new(TypeId::of::<OptionA>(), 1i64);
        let b = OptionOverride::new(TypeId::of::<OptionB>(), 2i64);
        assert_ne!(a.option_type_id, b.option_type_id);
    }

    #[test]
    fn override_set_preserves_push_order() {
        let mut set = OptionOverrideSet::new();
        set.push(OptionOverride::new(TypeId::of::<OptionA>(), 1i64));
        set.push(OptionOverride::new(TypeId::of::<OptionB>(), 2i64));
        let collected: Vec<_> = set.iter().map(|o| o.option_type_id).collect();
        assert_eq!(
            collected,
            vec![TypeId::of::<OptionA>(), TypeId::of::<OptionB>()]
        );
    }

    #[test]
    fn priority_ordering() {
        assert!(OverridePriority::Low < OverridePriority::Normal);
        assert!(OverridePriority::Normal < OverridePriority::High);
    }

    #[test]
    fn with_priority_preserves_priority() {
        let ov = OptionOverride::with_priority(
            TypeId::of::<OptionA>(),
            true,
            OverridePriority::High,
        );
        assert_eq!(ov.priority, OverridePriority::High);
        assert_eq!(ov.downcast_value::<bool>().copied(), Some(true));
    }

    #[test]
    fn at_priority_promotes() {
        let ov = OptionOverride::new(TypeId::of::<OptionA>(), 7i64);
        let promoted = ov.at_priority(OverridePriority::High);
        assert_eq!(promoted.priority, OverridePriority::High);
    }
}
