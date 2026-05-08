//! Layered option overrides (M.2.0).
//!
//! Modes (and other layer producers -- buffer-local sets, modal-
//! state hooks) contribute typed overrides via [`OptionOverride`].
//! The resolver walks layers in priority order
//! (`mode-architecture.md` §6.1) and picks the first non-empty
//! value per option for scalars; collection-shaped options
//! concatenate.
//!
//! `OptionOverride` is type-erased through `Arc<dyn Any + Send +
//! Sync>` for storage; the resolver downcasts back to the option's
//! `Value` type when emitting the resolved cache. Modes don't see
//! the erasure -- the [`crate::mode_options!`] macro (M.2.1)
//! generates type-safe wrappers that produce `OptionOverride`s
//! correctly typed against the option's `Value`.

use std::any::{Any, TypeId};
use std::sync::Arc;

use smallvec::SmallVec;

/// Tie-break priority for two layer entries that target the same
/// option.
///
/// Most modes use `Normal`; the registry picks last-activated
/// among `Normal`s and emits a `ModeEvent::OptionConflict` event
/// for visibility. `High` / `Low` are explicit overrides for
/// modes that genuinely need to win or lose regardless of
/// activation order (`read-only-mode` ⇒ `High` for `writable=false`).
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
/// Identity is by `option_type_id` -- the `TypeId` of the
/// declaration type (e.g. `TypeId::of::<Tabstop>()`). The
/// resolver looks up the option by this id; the typed downcast
/// against the option's `Value` is performed when emitting the
/// resolved cache.
#[derive(Clone)]
pub struct OptionOverride {
    /// `TypeId` of the [`crate::OptionDecl`] type this override
    /// targets.
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
    /// Construct a typed override against an [`crate::OptionDecl`].
    /// Used by the M.2.1 `mode_options!` macro to package each
    /// declarative `Mode::options()` entry.
    ///
    /// `T` is the declaration type; `V` is the value type
    /// (must match `T::Value` -- enforced by the macro at
    /// generation, not by the function signature here, because
    /// runtime-constructed overrides from the plugin adapter
    /// don't have access to the type-level binding).
    pub fn new<T: 'static, V: Clone + Send + Sync + 'static>(value: V) -> Self {
        Self {
            option_type_id: TypeId::of::<T>(),
            value: Arc::new(value),
            priority: OverridePriority::default(),
        }
    }

    /// Same as [`Self::new`] but with explicit priority.
    pub fn with_priority<T: 'static, V: Clone + Send + Sync + 'static>(
        value: V,
        priority: OverridePriority,
    ) -> Self {
        Self {
            option_type_id: TypeId::of::<T>(),
            value: Arc::new(value),
            priority,
        }
    }

    /// Attempt to downcast the value to the requested type.
    /// Returns `None` if the override targets a different type
    /// or if `V` doesn't match the stored `Arc<dyn Any>`.
    pub fn downcast_value<V: 'static>(&self) -> Option<&V> {
        self.value.downcast_ref::<V>()
    }
}

/// A set of [`OptionOverride`]s contributed by one layer
/// producer (typically the return value of `Mode::options()`).
///
/// `SmallVec` keeps the typical case (0-4 overrides per mode)
/// inline; modes that contribute many overrides spill to the
/// heap. This is layer-input data, not hot-path read data --
/// resolver walks each `OptionOverrideSet` exactly once per
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
    /// the resolver visits the elements in this order when
    /// merging.
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
        let ov = OptionOverride::new::<OptionA, _>(42i64);
        assert_eq!(ov.option_type_id, TypeId::of::<OptionA>());
        assert_eq!(ov.priority, OverridePriority::Normal);
        assert_eq!(ov.downcast_value::<i64>().copied(), Some(42));
    }

    #[test]
    fn downcast_to_wrong_type_returns_none() {
        let ov = OptionOverride::new::<OptionA, _>(42i64);
        assert!(ov.downcast_value::<bool>().is_none());
    }

    #[test]
    fn override_targets_correct_type() {
        let a = OptionOverride::new::<OptionA, _>(1i64);
        let b = OptionOverride::new::<OptionB, _>(2i64);
        assert_ne!(a.option_type_id, b.option_type_id);
    }

    #[test]
    fn override_set_preserves_push_order() {
        let mut set = OptionOverrideSet::new();
        set.push(OptionOverride::new::<OptionA, _>(1i64));
        set.push(OptionOverride::new::<OptionB, _>(2i64));
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
        let ov =
            OptionOverride::with_priority::<OptionA, _>(true, OverridePriority::High);
        assert_eq!(ov.priority, OverridePriority::High);
        assert_eq!(ov.downcast_value::<bool>().copied(), Some(true));
    }
}
