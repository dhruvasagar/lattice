//! Layered option overrides (M.2.1).
//!
//! Modes (and other layer producers — buffer-local sets,
//! modal-state hooks) contribute typed overrides via
//! [`OptionOverride`]. The resolver in `lattice-config` walks
//! the layers in priority order (`mode-architecture.md` §6.1)
//! and picks the first non-empty value per option for scalars;
//! collection-shaped options concatenate.
//!
//! ## Why this lives in `lattice-config`
//!
//! `Mode::options()` (in `lattice-mode`) returns an
//! [`OptionOverrideSet`]. These types once lived in `lattice-mode`
//! to break a `lattice-mode → lattice-config → lattice-core →
//! lattice-mode` cycle; the M.4 dependency inversion removed
//! `Document::modes` from `lattice-core`, retiring the cycle, so the
//! layer inputs now sit beside the [`crate::Resolver`] and
//! [`crate::ResolvedOptions`] they feed. `lattice-mode` re-exports
//! them.
//!
//! ## Type-safe construction via `lattice-config`'s `overrides!`
//!
//! Modes don't construct [`OptionOverride`] directly with the
//! erased [`OptionOverride::new`] API. Instead they use the
//! `overrides!` macro from `lattice-config`, which has access
//! to `OptionDecl` and emits compile-time-typed wrappers around
//! [`OptionOverride::new`]:
//!
//! ```
//! use lattice_config::{OptionOverrideSet, OverridePriority, Tabstop, Wrap};
//!
//! fn options() -> OptionOverrideSet {
//!     lattice_config::overrides! {
//!         Tabstop = 4,
//!         #[priority(High)]
//!         Wrap = false,
//!     }
//! }
//!
//! let set = options();
//! assert_eq!(set.len(), 2);
//! let wrap = set.iter().nth(1).unwrap();
//! assert_eq!(wrap.downcast_value::<bool>(), Some(&false));
//! assert_eq!(wrap.priority, OverridePriority::High);
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
/// Most modes use `Normal`; among `Normal`s the
/// [`crate::Resolver`] prefers the higher layer (the mode registry
/// orders minor-mode layers by activation, so last-activated wins)
/// and `lattice-mode` emits a `ModeEvent::OptionConflict` event for
/// visibility. `High` / `Low` are explicit overrides
/// for modes that genuinely need to win or lose regardless of
/// activation order (`read-only-mode` ⇒ `High` for
/// `writable=false`).
///
/// See `mode-architecture.md` §6.2 for the conflict policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OverridePriority {
    /// Loses to any `Normal` or `High` entry for the same option,
    /// whatever its layer — a fallback a mode offers only if nobody
    /// else has an opinion.
    Low,
    /// The default: resolved by layer order, then position in layer.
    #[default]
    Normal,
    /// Beats every `Normal` / `Low` entry regardless of layer — except
    /// a user's buffer-local (`:setlocal`) value, which outranks any
    /// mode contribution at any priority.
    High,
}

/// What an option is known by in an override and in a resolved snapshot
/// (RF.4e′, `plugin-mode-options.md` §8).
///
/// A native option is a Rust `OptionDecl` type, and its `TypeId` is its key:
/// the typed reads on the per-frame path (`ResolvedOptions::get::<T>()`) hash
/// nothing else. An option a plugin registered through the `config` seam has
/// no Rust type, and Rust cannot mint a `TypeId` at runtime, so it is known by
/// its canonical registered name (`refactor.declaration`). Without that, a
/// plugin option could not be `:setlocal`, overridden by a mode, or read per
/// buffer: there was nothing to file the value under.
///
/// Compares equal to a bare `TypeId` when it is that native key, so a check
/// like `ov.key == TypeId::of::<Wrap>()` reads as it did before.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OptionKey {
    /// A declared option: the `TypeId` of its `OptionDecl` type.
    Native(TypeId),
    /// A runtime-registered (plugin) option: its canonical name.
    Plugin(Arc<str>),
}

impl OptionKey {
    /// The native key of declaration type `T`.
    pub fn of<T: 'static>() -> Self {
        Self::Native(TypeId::of::<T>())
    }

    /// The `TypeId`, for a native key; `None` for a plugin option.
    pub fn type_id(&self) -> Option<TypeId> {
        match self {
            Self::Native(id) => Some(*id),
            Self::Plugin(_) => None,
        }
    }
}

impl From<TypeId> for OptionKey {
    fn from(id: TypeId) -> Self {
        Self::Native(id)
    }
}

impl PartialEq<TypeId> for OptionKey {
    fn eq(&self, other: &TypeId) -> bool {
        matches!(self, Self::Native(id) if id == other)
    }
}

/// One option-value override from a single layer producer (a
/// mode, a buffer-local set, a modal-state hook).
///
/// Identity is the [`OptionKey`]: the `TypeId` of a declaration
/// type (e.g. `TypeId::of::<Tabstop>()`), or a plugin option's
/// name. The resolver merges by this key; the typed downcast
/// against the option's `Value` is performed when reading the
/// resolved cache.
#[derive(Clone)]
pub struct OptionOverride {
    /// The option this override targets. Construct type-correctly:
    /// the `overrides!` macro checks a native value against its
    /// declaration; a runtime caller parses through the option's
    /// own `parse_to_erased`.
    pub key: OptionKey,
    /// The override value, type-erased. Downcast at resolution
    /// time to the declaration's `Value` type.
    pub value: Arc<dyn Any + Send + Sync>,
    /// Tie-break priority. See [`OverridePriority`].
    pub priority: OverridePriority,
}

impl std::fmt::Debug for OptionOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionOverride")
            .field("key", &self.key)
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
    pub fn new<V: Clone + Send + Sync + 'static>(key: impl Into<OptionKey>, value: V) -> Self {
        Self {
            key: key.into(),
            value: Arc::new(value),
            priority: OverridePriority::default(),
        }
    }

    /// Same as [`Self::new`] but with explicit priority.
    pub fn with_priority<V: Clone + Send + Sync + 'static>(
        key: impl Into<OptionKey>,
        value: V,
        priority: OverridePriority,
    ) -> Self {
        Self {
            key: key.into(),
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
    /// An empty set — what a mode that overrides nothing returns.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty set with room for `cap` overrides before spilling
    /// past the inline capacity (4).
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

    /// Number of overrides in the set, duplicates included (two
    /// entries for one option count twice).
    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    /// `true` when the set contributes nothing.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    /// The overrides in push order — the order the resolver visits
    /// them in, so a later entry for the same option wins within the
    /// set (priority permitting).
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
        assert_eq!(ov.key, TypeId::of::<OptionA>());
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
        assert_ne!(a.key, b.key);
    }

    #[test]
    fn override_set_preserves_push_order() {
        let mut set = OptionOverrideSet::new();
        set.push(OptionOverride::new(TypeId::of::<OptionA>(), 1i64));
        set.push(OptionOverride::new(TypeId::of::<OptionB>(), 2i64));
        let collected: Vec<_> = set.iter().filter_map(|o| o.key.type_id()).collect();
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
            OptionOverride::with_priority(TypeId::of::<OptionA>(), true, OverridePriority::High);
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
