//! `Resolver`: walks layered overrides and produces a
//! [`crate::ResolvedOptions`] cache for one buffer
//! (`mode-architecture.md` §6.1).
//!
//! Layer priority (highest to lowest):
//! 1. Modal-state override
//! 2. Buffer-local explicit set (`:setlocal`)
//! 3. Active minor modes (in activation order; `OverridePriority`
//!    breaks ties)
//! 4. Major mode
//! 5. Global (the registry's current value)
//! 6. Built-in default (the option's `default_value()`)
//!
//! For scalars: first non-empty layer wins. For collections
//! (statusline contributors, decoration providers, completion
//! sources) the layers concatenate; that's a layer-aware policy
//! that the resolver applies based on the option's value type.
//! M.2.0a's resolver is the scalar-only path; collection-shaped
//! options land in M.2.1 alongside the actual mode integrations
//! that produce them.
//!
//! ## Default-value resolution
//!
//! M.2.0a's resolver doesn't itself supply layer 6 (built-in
//! defaults). The expectation is that the registry pre-populates
//! the resolved cache with defaults via a one-time bootstrap,
//! and the resolver's per-recompute walk overlays the higher-
//! priority layers on top. This keeps the per-recompute cost
//! bounded to "options that have at least one override" rather
//! than re-iterating every registered option on every layer
//! change. Bootstrap is M.2.0b's territory (when migration of
//! built-in options to the macro path lets the registry
//! enumerate them via the linkme slice). Until then, callers
//! prepopulate with default values explicitly; tests do this
//! directly.

use std::any::TypeId;

use crate::origin::OptionOrigin;
use crate::overrides::{OptionOverride, OptionOverrideSet, OverridePriority};
use crate::resolved::ResolvedOptions;

/// Walks layered overrides and emits a fresh [`ResolvedOptions`].
///
/// The resolver is stateless -- it's just an algorithm. Callers
/// typically own the cache and ask the resolver to refill it
/// via [`Self::resolve_into`].
#[derive(Default)]
pub struct Resolver;

impl Resolver {
    pub fn new() -> Self {
        Self
    }

    /// Walk `layers` (highest priority first) and write resolved
    /// values into `out`. Each layer is an iterable of
    /// [`OptionOverride`]s in the layer's own internal order.
    /// Within a layer, last-pushed wins for the same option
    /// type; across layers, higher priority wins.
    ///
    /// Existing entries in `out` are preserved unless overridden
    /// by a layer; this lets callers seed `out` with defaults
    /// (via the registry's default-bootstrap, M.2.0b) and have
    /// the resolver overlay only what changed.
    ///
    /// `OverridePriority::High` wins regardless of layer
    /// position; `Low` only wins when no `Normal`/`High` covers
    /// the option. Within a single layer, two overrides at the
    /// same priority resolve to last-pushed (per
    /// `mode-architecture.md` §6.2 conflict policy; M.2.1 hooks
    /// this to a `ModeEvent::OptionConflict` emission).
    ///
    /// Origin is not tracked; use [`Self::resolve_into_with_origins`]
    /// when `:set name?` / `:setlocal name?` echo is needed.
    pub fn resolve_into<'a, L>(&self, layers: L, out: &mut ResolvedOptions)
    where
        L: IntoIterator<Item = &'a OptionOverrideSet>,
    {
        // Delegate to the origin-aware path, tagging every layer
        // with `GlobalConfig` as a neutral fallback. The bootstrap
        // already wrote the correct origin before this is called.
        self.resolve_into_with_origins(
            layers
                .into_iter()
                .map(|set| (set, OptionOrigin::GlobalConfig)),
            out,
        );
    }

    /// Origin-aware resolution. Each element is an
    /// `(&OptionOverrideSet, OptionOrigin)` pair; the origin is
    /// recorded alongside the winning value in `out`. The caller is
    /// responsible for assigning the correct [`OptionOrigin`] to each
    /// layer (e.g. `BufferLocal` for the buffer-local override set,
    /// `ModeContribution { mode_id }` for each mode's set).
    pub fn resolve_into_with_origins<'a>(
        &self,
        layers: impl IntoIterator<Item = (&'a OptionOverrideSet, OptionOrigin)>,
        out: &mut ResolvedOptions,
    ) {
        let mut winners: std::collections::HashMap<TypeId, Candidate<'_>> =
            std::collections::HashMap::new();

        for (layer_idx, (set, origin)) in layers.into_iter().enumerate() {
            let layer_rank = usize::MAX - layer_idx;
            for (pos, ov) in set.iter().enumerate() {
                let candidate = Candidate {
                    ov,
                    layer_rank,
                    within_layer_pos: pos,
                    origin: origin.clone(),
                };
                match winners.get(&ov.option_type_id) {
                    None => {
                        winners.insert(ov.option_type_id, candidate);
                    }
                    Some(existing) => {
                        if Self::candidate_better(&candidate, existing) {
                            winners.insert(ov.option_type_id, candidate);
                        }
                    }
                }
            }
        }

        for (type_id, c) in winners {
            out.insert_erased_with_origin(type_id, c.ov.value.clone(), c.origin);
        }
    }

    /// "Is `a` more authoritative than `b`?" Used during the
    /// merge walk. Order: `OverridePriority::High` always wins;
    /// `Low` always loses; among `Normal`s, higher layer rank
    /// wins; within a layer, later position wins.
    fn candidate_better(a: &Candidate<'_>, b: &Candidate<'_>) -> bool {
        // Explicit-priority wins absolute.
        if a.ov.priority == OverridePriority::High && b.ov.priority != OverridePriority::High {
            return true;
        }
        if b.ov.priority == OverridePriority::High {
            return false;
        }
        if a.ov.priority == OverridePriority::Low && b.ov.priority != OverridePriority::Low {
            return false;
        }
        if b.ov.priority == OverridePriority::Low {
            return true;
        }
        // Normal vs Normal: layer first, then position within layer.
        if a.layer_rank != b.layer_rank {
            return a.layer_rank > b.layer_rank;
        }
        a.within_layer_pos > b.within_layer_pos
    }
}

/// Internal merge-walk state. Tracks where one candidate
/// override sits in the layer/position lattice.
struct Candidate<'a> {
    ov: &'a OptionOverride,
    /// Higher = more authoritative. Caller iterates highest
    /// priority first; encoded as `usize::MAX - layer_idx`.
    layer_rank: usize,
    /// Within-layer position; ties within a layer resolve to
    /// higher position (= last pushed).
    within_layer_pos: usize,
    /// The layer this candidate came from; written to
    /// [`ResolvedOptions`] alongside the value when this
    /// candidate wins.
    origin: OptionOrigin,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::option_decl::{HasGroup, OptionDecl};
    use std::any::TypeId;
    use std::sync::Arc;

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

    fn ts(v: i64) -> OptionOverride {
        OptionOverride::new(TypeId::of::<Tabstop>(), v)
    }
    fn ts_with(v: i64, p: OverridePriority) -> OptionOverride {
        OptionOverride::with_priority(TypeId::of::<Tabstop>(), v, p)
    }
    fn num(v: bool) -> OptionOverride {
        OptionOverride::new(TypeId::of::<Number>(), v)
    }

    fn read_i64(r: &ResolvedOptions, _t: &Tabstop) -> Option<i64> {
        r.get::<Tabstop>().as_deref().copied()
    }

    fn read_bool(r: &ResolvedOptions, _t: &Number) -> Option<bool> {
        r.get::<Number>().as_deref().copied()
    }

    #[test]
    fn empty_layers_leave_cache_untouched() {
        let resolver = Resolver::new();
        let mut out = ResolvedOptions::new();
        out.insert::<Tabstop>(8); // pretend the default was bootstrapped
        let layers: [&OptionOverrideSet; 0] = [];
        resolver.resolve_into(layers, &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(8));
    }

    #[test]
    fn higher_priority_layer_wins() {
        // Three layers; top is most authoritative.
        let modal = OptionOverrideSet::from_iter([ts(1)]);
        let buffer_local = OptionOverrideSet::from_iter([ts(2)]);
        let global = OptionOverrideSet::from_iter([ts(3)]);
        let resolver = Resolver::new();
        let mut out = ResolvedOptions::new();
        resolver.resolve_into([&modal, &buffer_local, &global], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(1));
    }

    #[test]
    fn within_a_layer_last_wins() {
        let layer = OptionOverrideSet::from_iter([ts(1), ts(2), ts(3)]);
        let resolver = Resolver::new();
        let mut out = ResolvedOptions::new();
        resolver.resolve_into([&layer], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(3));
    }

    #[test]
    fn high_priority_beats_higher_layer() {
        // Modal layer (highest) at Normal vs minor layer (lower)
        // at High: High wins despite being in a lower layer.
        let modal = OptionOverrideSet::from_iter([ts(1)]);
        let minor = OptionOverrideSet::from_iter([ts_with(99, OverridePriority::High)]);
        let resolver = Resolver::new();
        let mut out = ResolvedOptions::new();
        resolver.resolve_into([&modal, &minor], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(99));
    }

    #[test]
    fn low_priority_loses_to_normal() {
        let low = OptionOverrideSet::from_iter([ts_with(1, OverridePriority::Low)]);
        let normal = OptionOverrideSet::from_iter([ts(2)]);
        let resolver = Resolver::new();
        let mut out = ResolvedOptions::new();
        resolver.resolve_into([&low, &normal], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(2));
    }

    #[test]
    fn distinct_options_resolve_independently() {
        let layer1 = OptionOverrideSet::from_iter([ts(4)]);
        let layer2 = OptionOverrideSet::from_iter([num(true)]);
        let resolver = Resolver::new();
        let mut out = ResolvedOptions::new();
        resolver.resolve_into([&layer1, &layer2], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(4));
        assert_eq!(read_bool(&out, &Number), Some(true));
    }

    #[test]
    fn pre_populated_default_overridden_by_layer() {
        // Bootstrap with default, then a layer overrides.
        let mut out = ResolvedOptions::new();
        out.insert::<Tabstop>(8);
        let layer = OptionOverrideSet::from_iter([ts(2)]);
        let resolver = Resolver::new();
        resolver.resolve_into([&layer], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(2));
    }

    #[test]
    fn pre_populated_default_preserved_when_no_layer_covers() {
        // Bootstrap default for Tabstop; layer only covers Number.
        let mut out = ResolvedOptions::new();
        out.insert::<Tabstop>(8);
        let layer = OptionOverrideSet::from_iter([num(true)]);
        let resolver = Resolver::new();
        resolver.resolve_into([&layer], &mut out);
        assert_eq!(read_i64(&out, &Tabstop), Some(8));
        assert_eq!(read_bool(&out, &Number), Some(true));
    }

    #[test]
    #[allow(unused_variables)] // suppress warning for type-arg-only references
    fn arc_clone_round_trips() {
        // Ensure ResolvedOptions::get returns Arc<T::Value>
        // and the value survives clone semantics.
        let mut out = ResolvedOptions::new();
        out.insert::<Tabstop>(4);
        let a: Arc<i64> = out.get::<Tabstop>().unwrap();
        let b: Arc<i64> = out.get::<Tabstop>().unwrap();
        assert_eq!(*a, 4);
        assert_eq!(*b, 4);
    }
}
