//! `KeymapRegistry` -- the public, layered keymap engine the
//! input dispatcher consults. Audit slice 8.c of the M3
//! refactor; see `docs/dev/architecture/keymap-architecture.md` for the design.
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

use crate::chord::{ChordParseError, KeyChord, parse_chord_sequence};
use crate::keymap::BindingMode;
use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, KeymapTrie, LookupResult};

/// Privilege bundle a writer presents when calling
/// capability-gated bind APIs (slice 8.h). Mirrors the WIT
/// `keymap-write` capability variants in DESIGN.md §5.5: the
/// host hands one of these to every caller of the registry --
/// built-in startup, the user's compiled `init.rs`, each loaded
/// plugin -- and the registry enforces the layer scope before
/// committing any write.
///
/// Today the enforcement runs purely in-process (no WASM host
/// has landed yet). When the plugin host is built, the WIT
/// `bind` / `unbind` / `push-layer` / `pop-layer` host functions
/// translate the caller's manifest-declared capability into one
/// of these variants and call through `try_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeymapCapability {
    /// Unrestricted: write to any layer. Reserved for the host's
    /// startup pass that registers the built-in catalog.
    Full,
    /// Write only to [`KeymapLayer::User`]. The compiled
    /// `init.rs` runs with this capability at boot. Mirror of
    /// the WIT "user" capability; denies writes to `Builtin`,
    /// `MajorMode`, `MinorMode(_)`, and `Buffer`.
    User,
    /// Write to any [`KeymapLayer::MinorMode`] or
    /// [`KeymapLayer::Buffer`] layer. Plugins receive this when
    /// their manifest declares `keymap-write:minor-mode` --
    /// permits transient overlays (custom modes, popup
    /// overrides) but denies writes to `Builtin` / `MajorMode` /
    /// `User`.
    MinorMode,
    /// Write only to a single specified `KeymapLayer::MinorMode`
    /// id. Mirror of the WIT `keymap-write:plugin-layer`
    /// variant: each plugin gets one dedicated layer at load
    /// time, and this capability scopes writes to that one
    /// layer (a stricter `MinorMode`).
    OwnedLayer { layer_id: LayerId },
}

/// Errors returned by the capability-gated bind APIs.
/// Slice 8.h.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapError {
    /// The supplied capability doesn't authorise writes to the
    /// requested layer. Surfaced to the host so it can echo
    /// `:bind` / `:unmap` errors and so plugin manifests that
    /// claim the wrong scope fail loudly at first registration.
    CapabilityDenied {
        capability: KeymapCapability,
        layer: KeymapLayer,
    },
    /// The supplied chord string couldn't be parsed.
    InvalidChord(ChordParseError),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::CapabilityDenied { capability, layer } => write!(
                f,
                "keymap capability {capability:?} cannot write to {layer:?}",
            ),
            KeymapError::InvalidChord(e) => write!(f, "invalid chord: {e:?}"),
        }
    }
}

impl std::error::Error for KeymapError {}

/// Returns `true` when `capability` authorises writes to
/// `layer`. The check is the only place layer scope is
/// enforced -- every capability-gated API funnels through here.
fn capability_allows(capability: KeymapCapability, layer: KeymapLayer) -> bool {
    match (capability, layer) {
        (KeymapCapability::Full, _) => true,
        (KeymapCapability::User, KeymapLayer::User) => true,
        (KeymapCapability::MinorMode, KeymapLayer::MinorMode(_) | KeymapLayer::Buffer) => true,
        (KeymapCapability::OwnedLayer { layer_id }, KeymapLayer::MinorMode(id)) => {
            layer_id.raw() == id
        }
        _ => false,
    }
}

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
        let bound = Arc::new(BoundCommand::from_invocation(command, source, layer));
        self.bind_bound(layer, mode, path, bound);
    }

    /// Lower-level binder: register a pre-built
    /// `Arc<BoundCommand>` directly. Used by the per-mode
    /// migration helpers (`keymap_replace::register_replace_bindings`
    /// + sibling slices) to register `BoundCommand`s carrying
    /// `legacy_action`. Production code should prefer [`Self::bind`]
    /// once the legacy bridge retires (slice 8.i).
    pub fn bind_bound(
        &self,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
        bound: Arc<BoundCommand>,
    ) {
        let label = default_label(layer);
        let merged = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let layer_ref = inner.layer_mut(layer, &label);
            layer_ref.modes.entry(mode).or_default().insert(path, bound);
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

    /// Human-readable label for the layer carrying `id`, if any.
    /// Drives `:describe-key`'s provenance row ("user, init.rs:42";
    /// "minor-mode:completion-popup"). Telemetry path; not on the
    /// hot path.
    pub fn layer_label(&self, id: LayerId) -> Option<String> {
        let inner = self.registry.inner.lock().expect("registry mutex");
        inner
            .layers
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.label.clone())
    }

    // ---- Slice 8.h: capability-gated WIT-shaped API.

    /// Capability-gated [`Self::bind`]. The host hands every
    /// caller a [`KeymapCapability`] derived from its manifest;
    /// this entry point checks the capability before committing
    /// the write so plugins / `init.rs` can't escape their
    /// declared scope.
    pub fn try_bind(
        &self,
        capability: KeymapCapability,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
        command: CommandInvocation,
        source: SourceLocation,
    ) -> Result<(), KeymapError> {
        if !capability_allows(capability, layer) {
            return Err(KeymapError::CapabilityDenied { capability, layer });
        }
        self.bind(layer, mode, path, command, source);
        Ok(())
    }

    /// Capability-gated convenience that parses `chord_str`
    /// (`"<leader>w"`, `"gd"`, `"<C-w>j"`) into a
    /// `Vec<ChordPattern::Literal>` before delegating to
    /// [`Self::try_bind`]. The host's WIT `bind` host-fn calls
    /// this; user `init.rs` calls a thin wrapper around it.
    ///
    /// `chord_str` must round-trip through
    /// [`crate::chord::parse_chord_sequence`]; wildcards
    /// (`<CharLiteral>`) aren't expressible from chord strings
    /// today and require [`Self::try_bind`] with a hand-built
    /// `&[ChordPattern]`.
    pub fn try_bind_chord_string(
        &self,
        capability: KeymapCapability,
        layer: KeymapLayer,
        mode: BindingMode,
        chord_str: &str,
        command: CommandInvocation,
        source: SourceLocation,
    ) -> Result<(), KeymapError> {
        let chords = parse_chord_sequence(chord_str).map_err(KeymapError::InvalidChord)?;
        let path: Vec<ChordPattern> = chords.into_iter().map(ChordPattern::Literal).collect();
        self.try_bind(capability, layer, mode, &path, command, source)
    }

    /// Capability-gated [`Self::unbind`]. Returns the dropped
    /// binding (or `None` when the path wasn't bound) so the
    /// host can echo "unbound `dd` (was: delete-line)".
    pub fn try_unbind(
        &self,
        capability: KeymapCapability,
        layer: KeymapLayer,
        mode: BindingMode,
        path: &[ChordPattern],
    ) -> Result<Option<Arc<BoundCommand>>, KeymapError> {
        if !capability_allows(capability, layer) {
            return Err(KeymapError::CapabilityDenied { capability, layer });
        }
        Ok(self.unbind(layer, mode, path))
    }

    /// Capability-gated [`Self::push_layer`]. Always permitted
    /// for `Full`, `MinorMode`, and `OwnedLayer` capabilities
    /// (push creates a new `MinorMode` layer regardless of the
    /// capability's specific scope). The `User` capability
    /// can't push runtime layers -- user config writes live in
    /// the static `User` layer registered at boot.
    pub fn try_push_layer(
        &self,
        capability: KeymapCapability,
        kind: PushLayerKind,
        label: impl Into<String>,
        bindings: HashMap<BindingMode, KeymapTrie>,
    ) -> Result<LayerId, KeymapError> {
        // `User` capability cannot push minor-mode layers --
        // it's the only one denied here. Every other capability
        // either has full reach (`Full`) or is scoped to
        // minor-mode-style layers anyway.
        if matches!(capability, KeymapCapability::User) {
            // Synthesise a `KeymapLayer` tag for the error so
            // the message is consistent with `try_bind`'s
            // denials. The actual layer hasn't been minted; the
            // `MinorMode(0)` placeholder is just the layer
            // *kind* the caller tried to write to.
            let placeholder = match kind {
                PushLayerKind::MinorMode => KeymapLayer::MinorMode(0),
                PushLayerKind::Buffer => KeymapLayer::Buffer,
            };
            return Err(KeymapError::CapabilityDenied {
                capability,
                layer: placeholder,
            });
        }
        Ok(self.push_layer(kind, label, bindings))
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
    #![allow(clippy::unwrap_used, clippy::panic)]
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
            (LookupResult::Bound { command: nb, .. }, LookupResult::Bound { command: vb, .. }) => {
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
        let bound = Arc::new(BoundCommand::from_invocation(
            invocation(99),
            src("snippet.tab"),
            KeymapLayer::MinorMode(0), // tag overridden by registry
        ));
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

    #[test]
    fn layer_label_round_trips_for_runtime_pushed_layers() {
        let h = KeymapHandle::new();
        let mut bindings = HashMap::new();
        let mut t = KeymapTrie::new();
        let bound = Arc::new(BoundCommand::from_invocation(
            invocation(1),
            src("snippet"),
            KeymapLayer::MinorMode(0),
        ));
        t.insert(&[lit('q')], bound);
        bindings.insert(BindingMode::Normal, t);
        let id = h.push_layer(PushLayerKind::MinorMode, "snippet", bindings);
        assert_eq!(h.layer_label(id).as_deref(), Some("snippet"));
        // Unknown id -> None.
        assert!(h.layer_label(LayerId(9999)).is_none());
    }

    // ---- Slice 8.h: capability gating ----

    /// `Full` -- the host's startup capability -- writes to
    /// every layer. The built-in catalog enumeration relies on
    /// this.
    #[test]
    fn full_capability_writes_to_every_layer() {
        let h = KeymapHandle::new();
        for layer in [
            KeymapLayer::Builtin,
            KeymapLayer::MajorMode,
            KeymapLayer::MinorMode(7),
            KeymapLayer::User,
            KeymapLayer::Buffer,
        ] {
            let r = h.try_bind(
                KeymapCapability::Full,
                layer,
                BindingMode::Normal,
                &[lit('j')],
                invocation(1),
                src("startup"),
            );
            assert!(r.is_ok(), "Full denied {layer:?}");
            // Clean up so the next iteration doesn't conflict.
            let _ = h.unbind(layer, BindingMode::Normal, &[lit('j')]);
        }
    }

    /// `User` capability writes only to `KeymapLayer::User`.
    /// Mirrors the WIT spec: the compiled `init.rs` runs with
    /// this capability and can rebind `dd` etc., but can't
    /// touch the built-in catalog.
    #[test]
    fn user_capability_accepts_user_layer() {
        let h = KeymapHandle::new();
        let r = h.try_bind(
            KeymapCapability::User,
            KeymapLayer::User,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(42),
            src("init.rs:1"),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn user_capability_denies_builtin_layer() {
        let h = KeymapHandle::new();
        let r = h.try_bind(
            KeymapCapability::User,
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('j')],
            invocation(1),
            src("init.rs"),
        );
        match r {
            Err(KeymapError::CapabilityDenied {
                capability: KeymapCapability::User,
                layer: KeymapLayer::Builtin,
            }) => {}
            other => panic!("expected CapabilityDenied, got {other:?}"),
        }
    }

    #[test]
    fn user_capability_denies_minor_mode_and_buffer_layers() {
        let h = KeymapHandle::new();
        for layer in [KeymapLayer::MinorMode(0), KeymapLayer::Buffer] {
            let r = h.try_bind(
                KeymapCapability::User,
                layer,
                BindingMode::Normal,
                &[lit('j')],
                invocation(1),
                src("init.rs"),
            );
            assert!(
                matches!(r, Err(KeymapError::CapabilityDenied { .. })),
                "User must deny {layer:?}"
            );
        }
    }

    #[test]
    fn minor_mode_capability_accepts_minor_mode_and_buffer() {
        let h = KeymapHandle::new();
        for layer in [KeymapLayer::MinorMode(3), KeymapLayer::Buffer] {
            let r = h.try_bind(
                KeymapCapability::MinorMode,
                layer,
                BindingMode::Normal,
                &[lit('j')],
                invocation(1),
                src("plugin"),
            );
            assert!(r.is_ok(), "MinorMode denied {layer:?}");
            let _ = h.unbind(layer, BindingMode::Normal, &[lit('j')]);
        }
    }

    #[test]
    fn minor_mode_capability_denies_builtin_and_user() {
        let h = KeymapHandle::new();
        for layer in [
            KeymapLayer::Builtin,
            KeymapLayer::MajorMode,
            KeymapLayer::User,
        ] {
            let r = h.try_bind(
                KeymapCapability::MinorMode,
                layer,
                BindingMode::Normal,
                &[lit('j')],
                invocation(1),
                src("plugin"),
            );
            assert!(
                matches!(r, Err(KeymapError::CapabilityDenied { .. })),
                "MinorMode must deny {layer:?}"
            );
        }
    }

    #[test]
    fn owned_layer_capability_accepts_only_its_own_id() {
        let h = KeymapHandle::new();
        // Push two minor-mode layers; only the first's id is
        // authorised by the OwnedLayer capability we mint.
        let id_a = h.push_layer(PushLayerKind::MinorMode, "plugin-a", HashMap::new());
        let id_b = h.push_layer(PushLayerKind::MinorMode, "plugin-b", HashMap::new());
        let cap = KeymapCapability::OwnedLayer { layer_id: id_a };

        // Plugin-a writes to its own MinorMode(id_a) -- ok.
        let r = h.try_bind(
            cap,
            KeymapLayer::MinorMode(id_a.raw()),
            BindingMode::Normal,
            &[lit('j')],
            invocation(1),
            src("plugin-a"),
        );
        assert!(r.is_ok());

        // Plugin-a tries to write to plugin-b's layer -- denied.
        let r = h.try_bind(
            cap,
            KeymapLayer::MinorMode(id_b.raw()),
            BindingMode::Normal,
            &[lit('k')],
            invocation(2),
            src("plugin-a"),
        );
        assert!(matches!(r, Err(KeymapError::CapabilityDenied { .. })));

        // Plugin-a tries to write to Builtin -- denied.
        let r = h.try_bind(
            cap,
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('k')],
            invocation(2),
            src("plugin-a"),
        );
        assert!(matches!(r, Err(KeymapError::CapabilityDenied { .. })));
    }

    #[test]
    fn user_capability_cannot_push_layer() {
        let h = KeymapHandle::new();
        let r = h.try_push_layer(
            KeymapCapability::User,
            PushLayerKind::MinorMode,
            "should-fail",
            HashMap::new(),
        );
        assert!(matches!(r, Err(KeymapError::CapabilityDenied { .. })));
    }

    #[test]
    fn minor_mode_capability_can_push_layer() {
        let h = KeymapHandle::new();
        let r = h.try_push_layer(
            KeymapCapability::MinorMode,
            PushLayerKind::MinorMode,
            "plugin-overlay",
            HashMap::new(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn try_bind_chord_string_parses_and_binds() {
        let h = KeymapHandle::new();
        let r = h.try_bind_chord_string(
            KeymapCapability::User,
            KeymapLayer::User,
            BindingMode::Normal,
            "<C-w>j",
            invocation(7),
            src("init.rs"),
        );
        assert!(r.is_ok());

        // The path must round-trip through the trie -- press
        // <C-w> then j and the user-layer binding fires.
        let lookup = h.lookup(
            BindingMode::Normal,
            &[KeyChord::ctrl('w'), KeyChord::char('j')],
        );
        match lookup {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(7));
                assert_eq!(command.layer, KeymapLayer::User);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn try_bind_chord_string_rejects_invalid_chord() {
        let h = KeymapHandle::new();
        // `<lt>` is parseable; `<` alone is not (unterminated angle).
        let r = h.try_bind_chord_string(
            KeymapCapability::User,
            KeymapLayer::User,
            BindingMode::Normal,
            "<not-closed",
            invocation(1),
            src("init.rs"),
        );
        assert!(matches!(r, Err(KeymapError::InvalidChord(_))));
    }

    /// Architecture-doc test: "user remaps `dd` and the
    /// rebinding survives a restart". Persistence isn't a
    /// registry concern (init.rs reruns at boot), so we
    /// simulate the surviving-restart shape: the user's
    /// `[d, d]` binding at `KeymapLayer::User` overrides the
    /// built-in's same-path binding, and the override stays
    /// authoritative across an arbitrary number of intervening
    /// reads / merges.
    #[test]
    fn user_remaps_dd_and_overrides_builtin() {
        let h = KeymapHandle::new();
        // Built-in catalog -- `[d, d]` -> command 100.
        h.try_bind(
            KeymapCapability::Full,
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(100),
            src("builtin.dd"),
        )
        .unwrap();
        // User `init.rs` -- rebind `[d, d]` -> command 200.
        h.try_bind(
            KeymapCapability::User,
            KeymapLayer::User,
            BindingMode::Normal,
            &[lit('d'), lit('d')],
            invocation(200),
            src("init.rs:42"),
        )
        .unwrap();

        let r = h.lookup(BindingMode::Normal, &[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(
                    command.command.command,
                    CommandId::new(200),
                    "user override must win",
                );
                assert_eq!(command.layer, KeymapLayer::User);
            }
            other => panic!("expected Bound (user.dd), got {other:?}"),
        }

        // Drive a few synthetic reads to assure the override
        // survives the merged-trie rebuild even when other
        // unrelated writes happen.
        for c in ['j', 'k', 'l'] {
            h.try_bind(
                KeymapCapability::Full,
                KeymapLayer::Builtin,
                BindingMode::Normal,
                &[lit(c)],
                invocation(c as u64),
                src("builtin"),
            )
            .unwrap();
        }
        let r = h.lookup(BindingMode::Normal, &[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(200));
            }
            other => panic!("expected Bound (user.dd) after extra binds, got {other:?}"),
        }
    }

    /// Architecture-doc test: "two plugins try to bind the
    /// same chord". Each plugin pushes its own MinorMode layer;
    /// the registry merges in priority order (LayerId
    /// ascending). The plugin pushed last wins, but the older
    /// plugin's binding stays in its layer so a future pop_layer
    /// of the winner restores it.
    #[test]
    fn conflicting_plugins_resolve_via_layer_priority() {
        let h = KeymapHandle::new();

        // Plugin A pushes its layer + binds `<leader>x`.
        let id_a = h.push_layer(PushLayerKind::MinorMode, "plugin-a", HashMap::new());
        h.try_bind(
            KeymapCapability::OwnedLayer { layer_id: id_a },
            KeymapLayer::MinorMode(id_a.raw()),
            BindingMode::Normal,
            &[lit('x')],
            invocation(1),
            src("plugin-a"),
        )
        .unwrap();

        // Plugin B pushes after A -- higher LayerId, wins.
        let id_b = h.push_layer(PushLayerKind::MinorMode, "plugin-b", HashMap::new());
        h.try_bind(
            KeymapCapability::OwnedLayer { layer_id: id_b },
            KeymapLayer::MinorMode(id_b.raw()),
            BindingMode::Normal,
            &[lit('x')],
            invocation(2),
            src("plugin-b"),
        )
        .unwrap();

        // Plugin B's binding wins.
        let r = h.lookup(BindingMode::Normal, &[pressed('x')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(2));
            }
            other => panic!("expected Bound (plugin-b.x), got {other:?}"),
        }

        // Pop plugin B; plugin A's binding resurfaces.
        h.pop_layer(id_b);
        let r = h.lookup(BindingMode::Normal, &[pressed('x')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(
                    command.command.command,
                    CommandId::new(1),
                    "plugin-a's binding should reappear after b pops",
                );
            }
            other => panic!("expected Bound (plugin-a.x), got {other:?}"),
        }
    }

    /// Architecture-doc test: "plugin binds chord that fires
    /// plugin command". Without the WASM host, we simulate the
    /// host-side bind path: a plugin with a dedicated
    /// `OwnedLayer` capability binds a chord to a typed
    /// `CommandInvocation` carrying the plugin's command id.
    /// Lookup of the chord returns the plugin command.
    #[test]
    fn plugin_binds_chord_that_fires_plugin_command() {
        let h = KeymapHandle::new();
        let id = h.push_layer(PushLayerKind::MinorMode, "plugin-foo", HashMap::new());
        let cap = KeymapCapability::OwnedLayer { layer_id: id };
        let plugin_cmd = invocation(0xFEED);

        // Bind `<C-x>fo` (multi-chord prefix, since `<leader>`
        // isn't yet parseable by `parse_chord_sequence`).
        h.try_bind_chord_string(
            cap,
            KeymapLayer::MinorMode(id.raw()),
            BindingMode::Normal,
            "<C-x>fo",
            plugin_cmd.clone(),
            src("plugin-foo:wit"),
        )
        .unwrap();

        let path = vec![
            KeyChord::ctrl('x'),
            KeyChord::char('f'),
            KeyChord::char('o'),
        ];
        let r = h.lookup(BindingMode::Normal, &path);
        match r {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, plugin_cmd.command);
                assert_eq!(command.layer, KeymapLayer::MinorMode(id.raw()));
            }
            other => panic!("expected Bound (plugin command), got {other:?}"),
        }
    }
}
