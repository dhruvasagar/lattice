//! `KeymapRegistry` -- the public, layered keymap engine the
//! input dispatcher consults. Audit slice 8.c of the M3
//! refactor; see `docs/keymap-architecture.md` for the design.
//!
//! ## Five-layer model (DESIGN.md §5.2.3)
//!
//! Bindings live in five layers, in priority order
//! (`Builtin < MajorMode < MinorMode(_) < User < Buffer`):
//!
//! 1. **Builtin** -- the default vim keymap, registered at
//!    startup from the existing `KeymapEntry` catalog.
//! 2. **MajorMode** -- per-major-mode (rust, markdown, ...)
//!    additions / overrides.
//! 3. **MinorMode** -- pushed/popped layers
//!    (active-snippet, completion-popup, picker, chord-capture).
//!    Each push gets a unique tag so multiple minors stack.
//! 4. **User** -- compiled `init.rs` bindings.
//! 5. **Buffer** -- per-buffer ad-hoc bindings (`:nmap <buffer>`).
//!
//! ## Wait-free reads, mailbox-style writes (in spirit)
//!
//! Reads (`lookup`) walk one merged trie per `BindingMode` --
//! the layers are physically merged into the read structure on
//! every write. Read cost is one `ArcSwap::load` + the trie
//! walk (audit slice 8.b: ~17ns single-chord, ~43ns three-chord).
//!
//! Writes (`bind` / `unbind` / `push_layer` / `pop_layer`)
//! take a brief mutex on the layer stack, mutate the affected
//! per-mode tries, rebuild the merged structure for every mode
//! that changed, and `ArcSwap::store` it. The mutex covers
//! pure in-memory work (no I/O); typical write completes in
//! sub-millisecond per the slice 8.b merge bench (~444 ns per
//! layer-merge × 6 modes = ~3 µs worst case).
//!
//! Writes are infrequent (startup catalog enumeration; minor-
//! mode push/pop on UI events; user `:bind` / `:unmap`); the
//! brief lock has no correctness exposure to the keystroke
//! path because reads never touch it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use lattice_grammar::{CommandInvocation, SourceLocation};

use crate::chord::KeyChord;
use crate::keymap::BindingMode;
use crate::keymap_trie::{
    BoundCommand, ChordPattern, KeymapLayer, KeymapTrie, LookupResult,
};

/// Stable id for a runtime-pushed layer (minor-mode overlays,
/// per-buffer bindings). Issued by [`KeymapHandle::push_layer`];
/// the caller passes it to `pop_layer` to remove the layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(u32);

impl LayerId {
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// One layer in the registry's stack. Per-layer bindings are
/// keyed by `BindingMode` so the merge can route a `Normal` ->
/// `Normal` (and not bleed `Visual` bindings into Normal lookup).
#[derive(Debug, Clone)]
struct RegistryLayer {
    /// Where this layer sits in priority order. The vec is
    /// sorted ascending by this; merges run lowest -> highest.
    layer: KeymapLayer,
    /// Stable id (only meaningful for `MinorMode` and `Buffer`
    /// layers that the caller may want to pop later).
    id: LayerId,
    /// Human-readable label for `:describe-key` provenance.
    /// "builtin", "major:rust", "minor:completion-popup", ...
    label: String,
    /// Per-mode tries.
    modes: HashMap<BindingMode, KeymapTrie>,
}

/// Wait-free read cell. The current merged-across-layers
/// per-mode trie set. Rebuilt by writers; read by every
/// keystroke.
#[derive(Debug, Default, Clone)]
struct MergedKeymap {
    by_mode: HashMap<BindingMode, KeymapTrie>,
}

/// Internal registry state, behind the registry's mutex.
/// Holds the per-layer trie tables; the merged cell is
/// rebuilt from this on every write.
struct RegistryInner {
    layers: Vec<RegistryLayer>,
    next_layer_id: u32,
}

impl RegistryInner {
    fn new() -> Self {
        Self {
            layers: Vec::new(),
            next_layer_id: 1,
        }
    }

    /// Mutate (insert if absent) the per-layer trie for
    /// `(layer, mode)`. Returns a mutable reference. Maintains
    /// the layer-vec sort.
    fn layer_mut(&mut self, layer: KeymapLayer, label_for_new: &str) -> &mut RegistryLayer {
        if let Some(pos) = self.layers.iter().position(|l| l.layer == layer) {
            return &mut self.layers[pos];
        }
        let id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        let new = RegistryLayer {
            layer,
            id,
            label: label_for_new.to_string(),
            modes: HashMap::new(),
        };
        // Insert sorted ascending by KeymapLayer.
        let pos = self
            .layers
            .iter()
            .position(|l| l.layer > layer)
            .unwrap_or(self.layers.len());
        self.layers.insert(pos, new);
        &mut self.layers[pos]
    }

    fn build_merged(&self) -> MergedKeymap {
        let mut merged = MergedKeymap::default();
        // Walk layers ascending; merge_over overlays each on
        // top, so the highest-priority layer's bindings end
        // up authoritative (architecture doc §2 + §4).
        for layer in &self.layers {
            for (mode, trie) in &layer.modes {
                let target = merged.by_mode.entry(*mode).or_default();
                target.merge_over(trie);
            }
        }
        merged
    }
}

/// Keymap registry. Cheap to clone (`Arc`-backed); every
/// caller (App, plugins, the future WIT host) holds a
/// [`KeymapHandle`] that wraps the same underlying registry.
///
/// Construction: [`KeymapRegistry::new`] returns an empty
/// registry; the App's startup pass enumerates the
/// `KeymapEntry` catalog and calls `bind` for each entry into
/// `KeymapLayer::Builtin`.
pub struct KeymapRegistry {
    inner: Mutex<RegistryInner>,
    merged: Arc<ArcSwap<MergedKeymap>>,
}

impl KeymapRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RegistryInner::new()),
            merged: Arc::new(ArcSwap::from_pointee(MergedKeymap::default())),
        })
    }
}

impl Default for KeymapRegistry {
    fn default() -> Self {
        // Allow `KeymapRegistry::default()` for tests without
        // forcing the Arc wrap. Consumers should still go
        // through `KeymapHandle`.
        Self {
            inner: Mutex::new(RegistryInner::new()),
            merged: Arc::new(ArcSwap::from_pointee(MergedKeymap::default())),
        }
    }
}

/// Editor-facing handle to the keymap registry.
///
/// **Reads are wait-free.** [`Self::lookup`] does one
/// `ArcSwap::load` + one trie walk. The keystroke path holds
/// the returned `Arc<MergedKeymap>` only for the duration of
/// the lookup; concurrent writes (mode push/pop, user `:bind`,
/// plugin registration) cannot stall it.
///
/// **Writes are mutex-routed.** [`Self::bind`] / [`Self::unbind`]
/// / [`Self::push_layer`] / [`Self::pop_layer`] take a brief
/// mutex on the layer stack, mutate the affected per-mode
/// tries, rebuild the merged cell, and `ArcSwap::store` it.
/// Writes are infrequent (startup; mode transitions; user
/// commands), so the lock is uncontended in practice.
#[derive(Clone)]
pub struct KeymapHandle {
    registry: Arc<KeymapRegistry>,
}

impl KeymapHandle {
    pub fn new() -> Self {
        Self {
            registry: KeymapRegistry::new(),
        }
    }

    /// Look up the typed binding for `chords` in `mode`.
    /// Wait-free.
    pub fn lookup(&self, mode: BindingMode, chords: &[KeyChord]) -> LookupResult {
        let merged = self.registry.merged.load();
        match merged.by_mode.get(&mode) {
            Some(trie) => trie.lookup(chords),
            None => LookupResult::Unbound,
        }
    }

    /// Register a binding at `(layer, mode, path)`. Replaces
    /// any prior binding at the exact same triple within the
    /// same layer (last-bind-wins per layer); higher-priority
    /// layers shadow lower-priority ones automatically via the
    /// merged-trie rebuild.
    pub fn bind(
        &self,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
        command: CommandInvocation,
        source: SourceLocation,
    ) {
        let bound = Arc::new(BoundCommand {
            command,
            source,
            layer,
        });
        let label = default_label(layer);
        let merged = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let layer_ref = inner.layer_mut(layer, &label);
            layer_ref
                .modes
                .entry(mode)
                .or_default()
                .insert(path, bound);
            inner.build_merged()
        };
        self.registry.merged.store(Arc::new(merged));
    }

    /// Remove the binding at `(layer, mode, path)`. No-op if
    /// nothing was registered there. Returns the dropped
    /// binding so callers can echo provenance ("unbound `dd`
    /// from user, init.rs:42").
    pub fn unbind(
        &self,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
    ) -> Option<Arc<BoundCommand>> {
        let (dropped, merged) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let pos = inner.layers.iter().position(|l| l.layer == layer)?;
            let layer_ref = &mut inner.layers[pos];
            let trie = layer_ref.modes.get_mut(&mode)?;
            let dropped = trie.remove(path);
            let merged = inner.build_merged();
            (dropped, merged)
        };
        self.registry.merged.store(Arc::new(merged));
        dropped
    }

    /// Push a fresh minor-mode (or buffer) layer. Returns the
    /// stable id the caller passes to [`Self::pop_layer`] to
    /// remove it.
    ///
    /// `bindings` is the layer's full per-mode binding set --
    /// computed by the caller (e.g. completion-popup wires
    /// its overrides at activation time). The registry copies
    /// the tries in; the caller's `KeymapTrie` instances are
    /// no longer needed.
    ///
    /// `layer` must be either `MinorMode(_)` (the registry
    /// allocates the tag; whatever caller passes is ignored)
    /// or `Buffer`. Other layer kinds error-back as a no-op
    /// today; future revisions can lift this if needed.
    pub fn push_layer(
        &self,
        kind: PushLayerKind,
        label: impl Into<String>,
        bindings: HashMap<BindingMode, KeymapTrie>,
    ) -> LayerId {
        let label = label.into();
        let (id, merged) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let id = LayerId(inner.next_layer_id);
            inner.next_layer_id += 1;
            let layer = match kind {
                PushLayerKind::MinorMode => KeymapLayer::MinorMode(id.0),
                PushLayerKind::Buffer => KeymapLayer::Buffer,
            };
            let new = RegistryLayer {
                layer,
                id,
                label,
                modes: bindings,
            };
            let pos = inner
                .layers
                .iter()
                .position(|l| l.layer > layer)
                .unwrap_or(inner.layers.len());
            inner.layers.insert(pos, new);
            (id, inner.build_merged())
        };
        self.registry.merged.store(Arc::new(merged));
        id
    }

    /// Pop the layer issued by an earlier `push_layer`.
    /// No-op if the id is unknown (caller may double-pop on
    /// the way out of an error path; defensive).
    pub fn pop_layer(&self, id: LayerId) {
        let merged = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let pos = inner.layers.iter().position(|l| l.id == id);
            if let Some(pos) = pos {
                inner.layers.remove(pos);
            }
            inner.build_merged()
        };
        self.registry.merged.store(Arc::new(merged));
    }

    /// Total binding count across all layers. Telemetry +
    /// tests; not on the hot path.
    pub fn binding_count(&self) -> usize {
        let inner = self.registry.inner.lock().expect("registry mutex");
        inner
            .layers
            .iter()
            .flat_map(|l| l.modes.values())
            .map(|t| t.binding_count())
            .sum()
    }
}

impl Default for KeymapHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// What kind of runtime-pushed layer to allocate.
/// `MinorMode` gets a fresh `MinorMode(id)` tag (multiple
/// minor modes can stack); `Buffer` is a singleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushLayerKind {
    MinorMode,
    Buffer,
}

fn default_label(layer: KeymapLayer) -> String {
    match layer {
        KeymapLayer::Builtin => "builtin".into(),
        KeymapLayer::MajorMode => "major-mode".into(),
        KeymapLayer::MinorMode(id) => format!("minor-mode:{id}"),
        KeymapLayer::User => "user".into(),
        KeymapLayer::Buffer => "buffer".into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_protocol::ids::CommandId;

    fn invocation(n: u64) -> CommandInvocation {
        CommandInvocation::of(CommandId::new(n))
    }

    fn src(label: &'static str) -> SourceLocation {
        let _ = label;
        SourceLocation::synthetic("test")
    }

    fn lit(c: char) -> ChordPattern {
        ChordPattern::Literal(KeyChord::char(c))
    }

    fn pressed(c: char) -> KeyChord {
        KeyChord::char(c)
    }

    #[test]
    fn lookup_returns_bound_after_bind() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(1),
            src("dd"),
        );
        let r = h.lookup(BindingMode::Normal, &[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(1));
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn higher_layer_shadows_lower() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(100),
            src("builtin.dd"),
        );
        h.bind(
            KeymapLayer::User,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(200),
            src("user.dd"),
        );
        let r = h.lookup(BindingMode::Normal, &[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(
                    command.command.command,
                    CommandId::new(200),
                    "user layer must win over builtin"
                );
                assert_eq!(command.layer, KeymapLayer::User);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn unbinding_user_layer_uncovers_builtin() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(100),
            src("builtin.dd"),
        );
        h.bind(
            KeymapLayer::User,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(200),
            src("user.dd"),
        );
        // Unbind user.dd -> builtin.dd should resurface.
        let dropped = h
            .unbind(
                KeymapLayer::User,
                BindingMode::Normal,
                &[lit('d'), lit('d')],
            )
            .expect("user binding existed");
        assert_eq!(dropped.command.command, CommandId::new(200));
        let r = h.lookup(BindingMode::Normal, &[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(100));
                assert_eq!(command.layer, KeymapLayer::Builtin);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn modes_are_independent() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('j')],
            invocation(1),
            src("normal.j"),
        );
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Visual,
            &[lit('j')],
            invocation(2),
            src("visual.j"),
        );
        // Same chord, different mode -> distinct bindings.
        let normal = h.lookup(BindingMode::Normal, &[pressed('j')]);
        let visual = h.lookup(BindingMode::Visual, &[pressed('j')]);
        match (normal, visual) {
            (
                LookupResult::Bound { command: nb, .. },
                LookupResult::Bound { command: vb, .. },
            ) => {
                assert_eq!(nb.command.command, CommandId::new(1));
                assert_eq!(vb.command.command, CommandId::new(2));
            }
            other => panic!("expected two Bound results, got {other:?}"),
        }
    }

    #[test]
    fn unrelated_mode_lookup_is_unbound() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('j')],
            invocation(1),
            src("normal.j"),
        );
        let r = h.lookup(BindingMode::Insert, &[pressed('j')]);
        assert!(matches!(r, LookupResult::Unbound), "got {r:?}");
    }

    #[test]
    fn push_minor_mode_shadows_builtins_then_pop_restores() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Insert,
            &[ChordPattern::Literal(KeyChord::special(
                crate::chord::SpecialKey::Tab,
            ))],
            invocation(1),
            src("builtin.tab"),
        );

        // Active-snippet minor mode wants <Tab> for placeholder
        // navigation. Push a minor-mode layer with its own
        // <Tab> binding.
        let mut minor_modes = HashMap::new();
        let mut t = KeymapTrie::new();
        let bound = Arc::new(BoundCommand {
            command: invocation(99),
            source: src("snippet.tab"),
            layer: KeymapLayer::MinorMode(0), // tag overridden by registry
        });
        t.insert(
            &[ChordPattern::Literal(KeyChord::special(
                crate::chord::SpecialKey::Tab,
            ))],
            bound,
        );
        minor_modes.insert(BindingMode::Insert, t);
        let id = h.push_layer(PushLayerKind::MinorMode, "snippet", minor_modes);

        // <Tab> -> snippet.tab.
        let r = h.lookup(
            BindingMode::Insert,
            &[KeyChord::special(crate::chord::SpecialKey::Tab)],
        );
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(99));
            }
            other => panic!("expected Bound (snippet), got {other:?}"),
        }

        // Pop the layer -> builtin.tab resurfaces.
        h.pop_layer(id);
        let r = h.lookup(
            BindingMode::Insert,
            &[KeyChord::special(crate::chord::SpecialKey::Tab)],
        );
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(1));
            }
            other => panic!("expected Bound (builtin), got {other:?}"),
        }
    }

    #[test]
    fn pop_unknown_id_is_noop() {
        let h = KeymapHandle::new();
        h.pop_layer(LayerId(9999));
        // No panic, no state change.
        assert_eq!(h.binding_count(), 0);
    }

    #[test]
    fn binding_count_tallies_across_layers() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('j')],
            invocation(1),
            src("j"),
        );
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('k')],
            invocation(2),
            src("k"),
        );
        h.bind(
            KeymapLayer::User,
            BindingMode::Normal,
            &[lit('j')],
            invocation(3),
            src("user.j"),
        );
        // Count is per-binding-per-layer (3 entries: 2 builtin
        // + 1 user), not the merged-deduped count.
        assert_eq!(h.binding_count(), 3);
    }

    #[test]
    fn lookup_partial_then_complete() {
        let h = KeymapHandle::new();
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('g'), lit('d')],
            invocation(1),
            src("gd"),
        );
        let r = h.lookup(BindingMode::Normal, &[pressed('g')]);
        assert!(matches!(r, LookupResult::Partial), "got {r:?}");
        let r = h.lookup(BindingMode::Normal, &[pressed('g'), pressed('d')]);
        assert!(matches!(r, LookupResult::Bound { .. }), "got {r:?}");
    }

    #[test]
    fn lookup_against_empty_registry_is_unbound() {
        let h = KeymapHandle::new();
        let r = h.lookup(BindingMode::Normal, &[pressed('j')]);
        assert!(matches!(r, LookupResult::Unbound), "got {r:?}");
    }
}
