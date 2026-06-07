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
use lattice_grammar::{CommandId, CommandInvocation, SourceLocation};
use lattice_protocol::chord::{ChordParseError, KeyChord, parse_chord_sequence};

use crate::{BindingMode, BoundCommand, ChordPattern, KeymapLayer, KeymapTrie, LookupResult, ModeId};
use crate::resolution::{KeymapResolution, LayerHit};

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
    /// Write only to a single specified [`KeymapLayer::MinorMode`]
    /// identified by its [`ModeId`]. Mirror of the WIT
    /// `keymap-write:plugin-layer` variant: each subsystem
    /// (plugin, user init.rs extending an existing mode) gets
    /// a capability scoped to one specific mode's keymap layer
    /// — e.g. `OwnedLayer { mode_id: ModeId::new("diff-mode") }`
    /// authorises writes only to the `diff-mode` layer.
    ///
    /// K.1.b (2026-05-30): re-keyed from opaque `LayerId` to
    /// `ModeId` so the capability names the mode it targets
    /// directly. Matches emacs's `(:map foo-mode-map ...)`
    /// shape: the binding is scoped to the mode, lives + dies
    /// with the mode's activation lifecycle.
    OwnedLayer { mode_id: ModeId },
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
        (
            KeymapCapability::OwnedLayer { mode_id: cap_mode },
            KeymapLayer::MinorMode(layer_mode),
        ) => cap_mode == layer_mode,
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

    /// K.1.c (2026-05-30): merge **only** the always-on
    /// layers (Builtin + MajorMode + User + Buffer). MinorMode
    /// layers are excluded — they're folded in per-keystroke
    /// based on the active buffer's mode set (see
    /// [`KeymapHandle::lookup_with_context`]).
    ///
    /// Pre-K.1.c the merge included every layer regardless of
    /// activation, which made the lookup path miss the
    /// "minor-mode bindings only fire when the mode is
    /// active on the active buffer" semantics emacs-style
    /// composability demands.
    fn build_always_on_merged(&self) -> MergedKeymap {
        let mut merged = MergedKeymap::default();
        for layer in &self.layers {
            if matches!(layer.layer, KeymapLayer::MinorMode(_)) {
                continue;
            }
            for (mode, trie) in &layer.modes {
                let target = merged.by_mode.entry(*mode).or_default();
                target.merge_over(trie);
            }
        }
        merged
    }

    /// K.1.c (2026-05-30): snapshot the per-`ModeId` minor-mode
    /// tries for the read-side cache. Each value is the full
    /// per-`BindingMode` trie set for that mode's keymap layer.
    /// The keystroke path consults this map for each active
    /// `ModeId` (in reverse activation order, last-wins) when
    /// composing the merged trie.
    fn build_minor_mode_tries(&self) -> HashMap<ModeId, Arc<HashMap<BindingMode, KeymapTrie>>> {
        let mut out = HashMap::new();
        for layer in &self.layers {
            if let KeymapLayer::MinorMode(mode_id) = layer.layer {
                out.insert(mode_id, Arc::new(layer.modes.clone()));
            }
        }
        out
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
    /// K.1.c (2026-05-30): cached merge of the **always-on**
    /// layers only (`Builtin + MajorMode + User + Buffer`).
    /// Wait-free read; rebuilt by writers. Pre-K.1.c this
    /// cached every layer; minor-mode layers are now folded in
    /// per-keystroke (see [`merged_minor_modes`](Self::merged_minor_modes)).
    merged: Arc<ArcSwap<MergedKeymap>>,
    /// K.1.c (2026-05-30): per-`ModeId` minor-mode trie cache.
    /// Wait-free read; consulted per-keystroke for each mode
    /// in `active_modes[active_buffer]` (reverse activation
    /// order, last-wins) by
    /// [`KeymapHandle::lookup_with_context`]. Rebuilt by
    /// writers alongside `merged`.
    minor_mode_tries: Arc<ArcSwap<HashMap<ModeId, Arc<HashMap<BindingMode, KeymapTrie>>>>>,
    /// MARG.2 (2026-06-03): reverse cache for the keybinding
    /// annotator surface. Indexes Normal-mode bindings by
    /// [`CommandId`] so `:` line command completion can show
    /// `<C-w>v` next to `:split-pane-vertical` (see
    /// `docs/dev/architecture/marginalia.md` §6). Rebuilt
    /// alongside `merged` at every bind / unbind / push /
    /// pop. Wait-free read.
    ///
    /// Coverage limits (v1):
    /// - **Normal mode only.** The `:` line completion picker
    ///   surfaces "what keybinding fires this command" — the
    ///   useful answer is Normal-mode chord (where operators /
    ///   motions / window commands live). Insert-mode
    ///   bindings (`<C-x><C-o>` etc.) get their own annotator
    ///   slice if/when needed.
    /// - **Literal-only paths.** Bindings whose chord path
    ///   contains `ChordPattern::CharLiteral` (e.g.
    ///   `f<char>`, `m<char>`) are skipped: the marginalia
    ///   column wants a clean chord-only sequence, not a
    ///   placeholder. Such commands still show in
    ///   `:describe-command`; the annotator just doesn't
    ///   prepend the chord.
    /// - **First-binding-wins.** When multiple chords bind
    ///   the same command, the first one encountered during
    ///   the trie walk is stored. Alternates remain
    ///   reachable via `:describe-command`. MRU-influenced
    ///   "pick the chord the user actually uses" is a
    ///   post-v1 follow-up flagged in marginalia.md §8.
    pub(crate) reverse_cache: Arc<ArcSwap<HashMap<CommandId, Vec<KeyChord>>>>,
}

impl KeymapRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RegistryInner::new()),
            merged: Arc::new(ArcSwap::from_pointee(MergedKeymap::default())),
            minor_mode_tries: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            reverse_cache: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        })
    }

    /// MARG.2 (2026-06-03): rebuild + store the Normal-mode
    /// reverse cache. Called by every write site after the
    /// `merged` ArcSwap has been stored. Cheap: walks the
    /// Normal-mode trie once (O(N) over bound chords).
    fn rebuild_reverse_cache(&self) {
        let merged = self.merged.load();
        let cache = build_reverse_cache_from_merged(&merged);
        self.reverse_cache.store(Arc::new(cache));
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
            minor_mode_tries: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            reverse_cache: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }
}

/// MARG.2 (2026-06-03): walk the merged Normal-mode trie and
/// produce a `CommandId → Vec<KeyChord>` map for the
/// keybinding annotator. Skips bindings whose chord path
/// contains `ChordPattern::CharLiteral` (wildcard) since the
/// marginalia column wants a clean chord-only sequence.
/// First-binding-wins on collisions.
fn build_reverse_cache_from_merged(merged: &MergedKeymap) -> HashMap<CommandId, Vec<KeyChord>> {
    let mut out: HashMap<CommandId, Vec<KeyChord>> = HashMap::new();
    let Some(trie) = merged.by_mode.get(&BindingMode::Normal) else {
        return out;
    };
    trie.walk_bindings(|path, bound| {
        let mut chords: Vec<KeyChord> = Vec::with_capacity(path.len());
        for seg in path {
            match seg {
                ChordPattern::Literal(c) => chords.push(*c),
                // Skip the whole binding when any segment is
                // a wildcard. Returning from the closure
                // skips THIS binding; the walker continues
                // with the next one.
                ChordPattern::CharLiteral => return,
            }
        }
        if chords.is_empty() {
            return;
        }
        out.entry(bound.command.command).or_insert(chords);
    });
    out
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
    pub(crate) registry: Arc<KeymapRegistry>,
}

impl std::fmt::Debug for KeymapHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeymapHandle").finish_non_exhaustive()
    }
}

impl KeymapHandle {
    pub fn new() -> Self {
        Self {
            registry: KeymapRegistry::new(),
        }
    }

    /// Expose the reverse-cache ArcSwap for callers in `lattice-host`
    /// that construct `KeymapReverseLookupHandle`. `lattice-keymap`
    /// cannot depend on `lattice-completion` (circular dep risk), so
    /// the `KeymapReverseLookupHandle` type lives in `lattice-host`
    /// and obtains the cache via this accessor.
    pub fn reverse_cache_arc(&self) -> Arc<ArcSwap<HashMap<CommandId, Vec<KeyChord>>>> {
        Arc::clone(&self.registry.reverse_cache)
    }

    /// Look up the typed binding for `chords` in `mode`.
    /// Wait-free.
    ///
    /// K.1.c (2026-05-30): preserves pre-K.1.c semantics by
    /// treating **all registered minor modes as active** (in
    /// `ModeId`-alphabetical order, matching K.1.b's sorted-
    /// layers-vec merge order). Legacy callers (the
    /// translate dispatcher's completion-popup / snippet
    /// keystroke path) continue to work unchanged — their
    /// mode lifecycle already gates at push/pop, so
    /// "everything always active" matches the push/pop
    /// surface.
    ///
    /// For per-buffer-gated lookup (the emacs-style
    /// composability story — `do` in diff-mode only fires
    /// when the active buffer has diff-mode active) use
    /// [`Self::lookup_with_context`] with the buffer's
    /// `active_modes`. D.5 wires diff-mode through that
    /// path; other modes migrate as their consumers care.
    pub fn lookup(&self, mode: BindingMode, chords: &[KeyChord]) -> LookupResult {
        let minors = self.registry.minor_mode_tries.load();
        let mut sorted: Vec<ModeId> = minors.keys().copied().collect();
        sorted.sort();
        self.lookup_with_context(mode, chords, &sorted)
    }

    /// K.1.c (2026-05-30): mode-aware lookup.
    ///
    /// Composes a fresh per-keystroke merged trie:
    /// 1. Start with the cached always-on merge
    ///    (`Builtin + MajorMode + User + Buffer`).
    /// 2. For each `mode_id` in `active_modes` (first to
    ///    last activation order) overlay that mode's
    ///    minor-mode layer on top — **last-activated wins**
    ///    among minor modes.
    /// 3. Note: the always-on cache already has `User /
    ///    Buffer` overlaid above `Builtin / MajorMode`. The
    ///    minor-mode overlay applied in step 2 therefore
    ///    sits *above* User/Buffer at lookup time — which
    ///    differs from the pre-K.1.c "MinorMode < User <
    ///    Buffer" priority. Rationale: mode-scoped chords
    ///    (`do` in `diff-mode`, `M-d` in
    ///    `corfu-popupinfo-map`) intentionally claim their
    ///    chord while the mode is active, even over the
    ///    user's global rebinds; users wanting to override
    ///    a specific mode's binding use the
    ///    `OwnedLayer { mode_id }` capability to bind
    ///    inside that mode's layer (where same-layer
    ///    last-write-wins applies). This matches emacs's
    ///    minor-mode-precedence-over-global semantics.
    ///
    /// Wait-free reads: two `ArcSwap::load` calls
    /// (`merged`, `minor_mode_tries`) + per-`active_modes`
    /// merge work. Typical `active_modes.len()` is 0-3 so
    /// the overhead is small.
    pub fn lookup_with_context(
        &self,
        mode: BindingMode,
        chords: &[KeyChord],
        active_modes: &[ModeId],
    ) -> LookupResult {
        let always_on = self.registry.merged.load();
        // Fast path: no minor modes active → use the cached
        // always-on trie directly, no per-tick allocation.
        if active_modes.is_empty() {
            return match always_on.by_mode.get(&mode) {
                Some(trie) => trie.lookup(chords),
                None => LookupResult::Unbound,
            };
        }
        // Per-tick fold: start from always-on, overlay each
        // active minor mode in activation order (last wins).
        let minors = self.registry.minor_mode_tries.load();
        let mut composite = KeymapTrie::new();
        if let Some(base) = always_on.by_mode.get(&mode) {
            composite.merge_over(base);
        }
        for mode_id in active_modes {
            if let Some(per_mode) = minors.get(mode_id) {
                if let Some(trie) = per_mode.get(&mode) {
                    composite.merge_over(trie);
                }
            }
        }
        composite.lookup(chords)
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
        let (merged, minors) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let layer_ref = inner.layer_mut(layer, &label);
            layer_ref.modes.entry(mode).or_default().insert(path, bound);
            (
                inner.build_always_on_merged(),
                inner.build_minor_mode_tries(),
            )
        };
        self.registry.merged.store(Arc::new(merged));
        self.registry.minor_mode_tries.store(Arc::new(minors));
        // MARG.2: keep the reverse-cache in lockstep with the
        // merged trie. Every site that stores `merged` /
        // `minor_mode_tries` must also rebuild the reverse
        // cache or the keybinding annotator will surface
        // stale chord text.
        self.registry.rebuild_reverse_cache();
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
        let (dropped, merged, minors) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let pos = inner.layers.iter().position(|l| l.layer == layer)?;
            let layer_ref = &mut inner.layers[pos];
            let trie = layer_ref.modes.get_mut(&mode)?;
            let dropped = trie.remove(path);
            (
                dropped,
                inner.build_always_on_merged(),
                inner.build_minor_mode_tries(),
            )
        };
        self.registry.merged.store(Arc::new(merged));
        self.registry.minor_mode_tries.store(Arc::new(minors));
        // MARG.2: keep the reverse-cache in lockstep with the
        // merged trie. Every site that stores `merged` /
        // `minor_mode_tries` must also rebuild the reverse
        // cache or the keybinding annotator will surface
        // stale chord text.
        self.registry.rebuild_reverse_cache();
        dropped
    }

    /// Install a minor-mode or buffer layer.
    ///
    /// K.1.b (2026-05-30): for `PushLayerKind::MinorMode(mode_id)`,
    /// the layer's identity is the `mode_id` — pushing for the
    /// same mode_id is **idempotent on the layer**: the
    /// existing layer's bindings are replaced, no sibling layer
    /// is minted. `Buffer` continues to mint a fresh opaque
    /// `LayerId` per push.
    ///
    /// `bindings` is the layer's full per-mode binding set --
    /// computed by the caller (e.g. completion-popup wires its
    /// overrides at activation time). The registry copies the
    /// tries in; the caller's `KeymapTrie` instances are no
    /// longer needed after the call returns.
    ///
    /// Returns the `LayerId` of the installed layer (whether
    /// freshly minted or pre-existing for the same `mode_id`).
    /// For `MinorMode`, prefer popping via
    /// [`Self::pop_minor_mode_layer`] (by `mode_id`) over
    /// [`Self::pop_layer`] (by `LayerId`); both work but the
    /// former is what matches the install signature.
    pub fn push_layer(
        &self,
        kind: PushLayerKind,
        label: impl Into<String>,
        bindings: HashMap<BindingMode, KeymapTrie>,
    ) -> LayerId {
        let label = label.into();
        let (id, merged, minors) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let layer = match kind {
                PushLayerKind::MajorMode(mode_id) => KeymapLayer::MajorMode(mode_id),
                PushLayerKind::MinorMode(mode_id) => KeymapLayer::MinorMode(mode_id),
                PushLayerKind::Buffer => KeymapLayer::Buffer,
            };
            // K.1.b: idempotent-on-identity for MinorMode —
            // re-pushing the same mode_id replaces bindings on
            // the existing layer rather than minting a new one.
            // Buffer always mints fresh (Buffer layer is a
            // singleton in practice but we don't enforce that
            // here).
            let id = if let Some(pos) = inner.layers.iter().position(|l| l.layer == layer) {
                let existing_id = inner.layers[pos].id;
                inner.layers[pos].label = label;
                inner.layers[pos].modes = bindings;
                existing_id
            } else {
                let id = LayerId(inner.next_layer_id);
                inner.next_layer_id += 1;
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
                id
            };
            (
                id,
                inner.build_always_on_merged(),
                inner.build_minor_mode_tries(),
            )
        };
        self.registry.merged.store(Arc::new(merged));
        self.registry.minor_mode_tries.store(Arc::new(minors));
        // MARG.2: keep the reverse-cache in lockstep with the
        // merged trie. Every site that stores `merged` /
        // `minor_mode_tries` must also rebuild the reverse
        // cache or the keybinding annotator will surface
        // stale chord text.
        self.registry.rebuild_reverse_cache();
        id
    }

    /// Pop the layer issued by an earlier `push_layer`.
    /// No-op if the id is unknown (caller may double-pop on
    /// the way out of an error path; defensive).
    pub fn pop_layer(&self, id: LayerId) {
        let (merged, minors) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let pos = inner.layers.iter().position(|l| l.id == id);
            if let Some(pos) = pos {
                inner.layers.remove(pos);
            }
            (
                inner.build_always_on_merged(),
                inner.build_minor_mode_tries(),
            )
        };
        self.registry.merged.store(Arc::new(merged));
        self.registry.minor_mode_tries.store(Arc::new(minors));
        // MARG.2: keep the reverse-cache in lockstep with the
        // merged trie. Every site that stores `merged` /
        // `minor_mode_tries` must also rebuild the reverse
        // cache or the keybinding annotator will surface
        // stale chord text.
        self.registry.rebuild_reverse_cache();
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

    /// K.1.d (2026-05-30): enumerate every binding registered
    /// for `chords` in `mode` across all layers, returning the
    /// layer + binding pair for each. Telemetry path (drives
    /// `:describe-key`'s mode-aware section); not on the
    /// keystroke hot path. Order: layer-priority ascending
    /// (Builtin first, then MajorMode, then MinorMode layers
    /// in ModeId-alphabetical order, then User, then Buffer).
    /// Callers cross-reference the layers' MinorMode entries
    /// against the active buffer's `ActiveModes` to mark
    /// which would actually fire right now.
    pub fn enumerate_chord_bindings(
        &self,
        mode: BindingMode,
        chords: &[KeyChord],
    ) -> Vec<(KeymapLayer, Arc<BoundCommand>)> {
        let inner = self.registry.inner.lock().expect("registry mutex");
        let mut hits = Vec::new();
        for layer in &inner.layers {
            if let Some(trie) = layer.modes.get(&mode) {
                if let LookupResult::Bound { command, .. } = trie.lookup(chords) {
                    hits.push((layer.layer, command));
                }
            }
        }
        hits
    }

    /// Full per-layer trace for `chords` in `mode`.
    ///
    /// Returns a [`KeymapResolution`] whose `hits` list every registered
    /// layer that has a terminal binding at the given chord path, in
    /// priority order ascending (Builtin first, Buffer last). The `active`
    /// flag on each hit is set by crossing the layer against `active_modes`:
    /// - `Builtin`, `User`, `Buffer` are always active.
    /// - `MajorMode(id)` and `MinorMode(id)` are active iff `id` is
    ///   contained in `active_modes`.
    ///
    /// Telemetry path; not on the keystroke hot path.
    pub fn resolve_trace(
        &self,
        mode: BindingMode,
        chords: &[KeyChord],
        active_modes: &[ModeId],
    ) -> KeymapResolution {
        let pairs = self.enumerate_chord_bindings(mode, chords);
        let hits = pairs
            .into_iter()
            .map(|(layer, command)| {
                let active = match layer {
                    // MajorMode is always-on: it lives in the always_on
                    // merged trie used by lookup_with_context, so its
                    // bindings fire regardless of which modes are active.
                    KeymapLayer::Builtin
                    | KeymapLayer::MajorMode(_)
                    | KeymapLayer::User
                    | KeymapLayer::Buffer => true,
                    KeymapLayer::MinorMode(id) => active_modes.contains(&id),
                };
                LayerHit { layer, command, active }
            })
            .collect();
        KeymapResolution { mode, hits }
    }

    /// Run [`Self::resolve_trace`] for every `BindingMode` variant.
    ///
    /// Returns only the modes that have at least one registered binding
    /// for `chords` (i.e. non-empty `hits`). Callers that want to display
    /// `:describe-key` with all modes iterate the returned vec; modes with
    /// no bindings are omitted to keep output compact.
    ///
    /// Telemetry path; not on the keystroke hot path.
    pub fn resolve_trace_all_modes(
        &self,
        chords: &[KeyChord],
        active_modes: &[ModeId],
    ) -> Vec<KeymapResolution> {
        BindingMode::all()
            .iter()
            .map(|&mode| self.resolve_trace(mode, chords, active_modes))
            .filter(|r| !r.hits.is_empty())
            .collect()
    }

    /// Human-readable label for a `KeymapLayer`, derived from the layer's
    /// registered label string (set at `push_layer` / `bind` time). Falls
    /// back to `default_label` when the layer hasn't been explicitly named.
    /// Used by `:describe-key` output.
    pub fn layer_label_string(&self, layer: KeymapLayer) -> String {
        let inner = self.registry.inner.lock().expect("registry mutex");
        inner
            .layers
            .iter()
            .find(|l| l.layer == layer)
            .map(|l| l.label.clone())
            .unwrap_or_else(|| default_label(layer))
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
    /// [`lattice_protocol::chord::parse_chord_sequence`]; wildcards
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
            // denials. The actual layer hasn't been installed;
            // the synthesised tag reflects the layer *kind* the
            // caller tried to write to.
            let placeholder = match kind {
                PushLayerKind::MajorMode(mode_id) => KeymapLayer::MajorMode(mode_id),
                PushLayerKind::MinorMode(mode_id) => KeymapLayer::MinorMode(mode_id),
                PushLayerKind::Buffer => KeymapLayer::Buffer,
            };
            return Err(KeymapError::CapabilityDenied {
                capability,
                layer: placeholder,
            });
        }
        Ok(self.push_layer(kind, label, bindings))
    }

    /// K.1.b (2026-05-30): pop a minor-mode layer by its
    /// `ModeId`. The natural complement to
    /// `push_layer(PushLayerKind::MinorMode(mode_id), …)` —
    /// callers don't have to thread a separate `LayerId`
    /// through teardown when the mode id is what they already
    /// know. No-op if no layer for `mode_id` is currently
    /// installed (defensive against double-pop on error paths).
    /// Returns `true` iff a layer was removed.
    pub fn pop_minor_mode_layer(&self, mode_id: ModeId) -> bool {
        let (removed, merged, minors) = {
            let mut inner = self.registry.inner.lock().expect("registry mutex");
            let pos = inner
                .layers
                .iter()
                .position(|l| l.layer == KeymapLayer::MinorMode(mode_id));
            let removed = if let Some(pos) = pos {
                inner.layers.remove(pos);
                true
            } else {
                false
            };
            (
                removed,
                inner.build_always_on_merged(),
                inner.build_minor_mode_tries(),
            )
        };
        self.registry.merged.store(Arc::new(merged));
        self.registry.minor_mode_tries.store(Arc::new(minors));
        // MARG.2: keep the reverse-cache in lockstep with the
        // merged trie. Every site that stores `merged` /
        // `minor_mode_tries` must also rebuild the reverse
        // cache or the keybinding annotator will surface
        // stale chord text.
        self.registry.rebuild_reverse_cache();
        removed
    }
}

impl Default for KeymapHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// What kind of runtime-pushed layer to install.
///
/// K.1.b (2026-05-30): `MinorMode` now carries a typed
/// [`ModeId`] — the layer's identity = the mode's identity.
/// Pushing for the same `mode_id` is idempotent on the layer
/// (replaces bindings; no sibling layer minted). `Buffer`
/// stays opaque (a future K.1.x slice will type it on
/// [`lattice_core::BufferId`] for symmetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushLayerKind {
    MajorMode(ModeId),
    MinorMode(ModeId),
    Buffer,
}

fn default_label(layer: KeymapLayer) -> String {
    match layer {
        // K.2.4.A.2: user-facing friendly labels, not Debug slugs.
        KeymapLayer::Builtin => "Built-in".into(),
        KeymapLayer::MajorMode(mode_id) => format!("Major mode: {mode_id}"),
        // K.1.b: label derives from ModeId so `:describe-key`
        // provenance reads `Minor mode: diff-mode` without drift.
        KeymapLayer::MinorMode(mode_id) => format!("Minor mode: {mode_id}"),
        KeymapLayer::User => "User config".into(),
        KeymapLayer::Buffer => "Buffer".into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::chord::SpecialKey;
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
                SpecialKey::Tab,
            ))],
            invocation(1),
            src("builtin.tab"),
        );

        // Active-snippet minor mode wants <Tab> for placeholder
        // navigation. Push a minor-mode layer with its own
        // <Tab> binding.
        let snippet_mode = ModeId::new("snippet");
        let mut minor_modes = HashMap::new();
        let mut t = KeymapTrie::new();
        let bound = Arc::new(BoundCommand::from_invocation(
            invocation(99),
            src("snippet.tab"),
            KeymapLayer::MinorMode(snippet_mode),
        ));
        t.insert(
            &[ChordPattern::Literal(KeyChord::special(
                SpecialKey::Tab,
            ))],
            bound,
        );
        minor_modes.insert(BindingMode::Insert, t);
        let id = h.push_layer(
            PushLayerKind::MinorMode(snippet_mode),
            "snippet",
            minor_modes,
        );

        // <Tab> -> snippet.tab.
        let r = h.lookup(
            BindingMode::Insert,
            &[KeyChord::special(SpecialKey::Tab)],
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
            &[KeyChord::special(SpecialKey::Tab)],
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
        let snippet_mode = ModeId::new("snippet");
        let mut bindings = HashMap::new();
        let mut t = KeymapTrie::new();
        let bound = Arc::new(BoundCommand::from_invocation(
            invocation(1),
            src("snippet"),
            KeymapLayer::MinorMode(snippet_mode),
        ));
        t.insert(&[lit('q')], bound);
        bindings.insert(BindingMode::Normal, t);
        let id = h.push_layer(PushLayerKind::MinorMode(snippet_mode), "snippet", bindings);
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
            KeymapLayer::MajorMode(ModeId::new("test-major")),
            KeymapLayer::MinorMode(ModeId::new("test-minor-7")),
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
        for layer in [
            KeymapLayer::MinorMode(ModeId::new("test-minor")),
            KeymapLayer::Buffer,
        ] {
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
        for layer in [
            KeymapLayer::MinorMode(ModeId::new("test-minor-3")),
            KeymapLayer::Buffer,
        ] {
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
            KeymapLayer::MajorMode(ModeId::new("major-mode")),
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
        // Push two minor-mode layers; only the first's mode is
        // authorised by the OwnedLayer capability we mint.
        let mode_a = ModeId::new("plugin-a");
        let mode_b = ModeId::new("plugin-b");
        let _id_a = h.push_layer(PushLayerKind::MinorMode(mode_a), "plugin-a", HashMap::new());
        let _id_b = h.push_layer(PushLayerKind::MinorMode(mode_b), "plugin-b", HashMap::new());
        let cap = KeymapCapability::OwnedLayer { mode_id: mode_a };

        // Plugin-a writes to its own MinorMode(mode_a) -- ok.
        let r = h.try_bind(
            cap,
            KeymapLayer::MinorMode(mode_a),
            BindingMode::Normal,
            &[lit('j')],
            invocation(1),
            src("plugin-a"),
        );
        assert!(r.is_ok());

        // Plugin-a tries to write to plugin-b's layer -- denied.
        let r = h.try_bind(
            cap,
            KeymapLayer::MinorMode(mode_b),
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
            PushLayerKind::MinorMode(ModeId::new("should-fail-mode")),
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
            PushLayerKind::MinorMode(ModeId::new("plugin-overlay")),
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
        let mode_a = ModeId::new("plugin-a");
        let mode_b = ModeId::new("plugin-b");

        // Plugin A pushes its layer + binds `<leader>x`.
        let _id_a = h.push_layer(PushLayerKind::MinorMode(mode_a), "plugin-a", HashMap::new());
        h.try_bind(
            KeymapCapability::OwnedLayer { mode_id: mode_a },
            KeymapLayer::MinorMode(mode_a),
            BindingMode::Normal,
            &[lit('x')],
            invocation(1),
            src("plugin-a"),
        )
        .unwrap();

        // Plugin B pushes after A. K.1.b: the registry sorts
        // MinorMode layers by ModeId (alphabetical via the
        // interned string), so "plugin-b" > "plugin-a"; B's
        // layer ends up higher in the merge and wins on
        // overlapping chords. K.1.c will replace this
        // ModeId-alphabetic ordering with per-buffer active-
        // mode reverse-activation order.
        let id_b = h.push_layer(PushLayerKind::MinorMode(mode_b), "plugin-b", HashMap::new());
        h.try_bind(
            KeymapCapability::OwnedLayer { mode_id: mode_b },
            KeymapLayer::MinorMode(mode_b),
            BindingMode::Normal,
            &[lit('x')],
            invocation(2),
            src("plugin-b"),
        )
        .unwrap();

        // Plugin B's binding wins (higher ModeId in alpha order).
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
        let mode_id = ModeId::new("plugin-foo");
        let _id = h.push_layer(
            PushLayerKind::MinorMode(mode_id),
            "plugin-foo",
            HashMap::new(),
        );
        let cap = KeymapCapability::OwnedLayer { mode_id };
        let plugin_cmd = invocation(0xFEED);

        // Bind `<C-x>fo` (multi-chord prefix, since `<leader>`
        // isn't yet parseable by `parse_chord_sequence`).
        h.try_bind_chord_string(
            cap,
            KeymapLayer::MinorMode(mode_id),
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
                assert_eq!(command.layer, KeymapLayer::MinorMode(mode_id));
            }
            other => panic!("expected Bound (plugin command), got {other:?}"),
        }
    }

    // ---- K.1.c: per-buffer active-mode filter ----

    /// K.1.c: `lookup_with_context` with empty `active_modes`
    /// skips every minor-mode layer's bindings. Legacy
    /// `lookup` (which iterates *all* registered minor modes)
    /// continues to see them — that's the back-compat path.
    /// Together: the new context-aware API lets callers opt
    /// into per-buffer gating without disrupting any existing
    /// dispatch path.
    #[test]
    fn lookup_with_context_empty_active_modes_skips_minor_modes() {
        let h = KeymapHandle::new();
        let diff_mode = ModeId::new("diff-mode");
        // Push diff-mode with `do` → command 42.
        let mut bindings = HashMap::new();
        let mut trie = KeymapTrie::new();
        let bound = Arc::new(BoundCommand::from_invocation(
            invocation(42),
            src("diff-mode.do"),
            KeymapLayer::MinorMode(diff_mode),
        ));
        trie.insert(&[lit('d'), lit('o')], bound);
        bindings.insert(BindingMode::Normal, trie);
        h.push_layer(PushLayerKind::MinorMode(diff_mode), "diff-mode", bindings);

        // Legacy lookup sees the binding (all modes active).
        let legacy = h.lookup(BindingMode::Normal, &[pressed('d'), pressed('o')]);
        assert!(
            matches!(legacy, LookupResult::Bound { .. }),
            "legacy lookup must see diff-mode.do"
        );

        // Context-aware lookup with empty active_modes does NOT
        // fire the diff-mode binding — diff-mode isn't active.
        let ctx_empty =
            h.lookup_with_context(BindingMode::Normal, &[pressed('d'), pressed('o')], &[]);
        assert!(
            matches!(ctx_empty, LookupResult::Unbound),
            "lookup_with_context(&[]) must NOT see diff-mode.do (mode not active)"
        );

        // Context-aware with diff-mode listed → fires.
        let ctx_active = h.lookup_with_context(
            BindingMode::Normal,
            &[pressed('d'), pressed('o')],
            &[diff_mode],
        );
        match ctx_active {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(42));
            }
            other => panic!("expected Bound (diff-mode.do active), got {other:?}"),
        }
    }

    /// K.1.c: chord reuse across modes is the headline
    /// composability story. Two modes both bind `do` to
    /// different commands; the lookup result depends on which
    /// mode is in `active_modes` for that buffer. Per-buffer
    /// activation drives semantics — exactly the emacs
    /// `(:map foo-mode-map …)` shape.
    #[test]
    fn lookup_with_context_chord_reuse_across_modes() {
        let h = KeymapHandle::new();
        let diff_mode = ModeId::new("diff-mode");
        let overlay_mode = ModeId::new("my-overlay-mode");

        // diff-mode binds `do` → command 100 (diff-get).
        let mut diff_bindings = HashMap::new();
        let mut diff_trie = KeymapTrie::new();
        diff_trie.insert(
            &[lit('d'), lit('o')],
            Arc::new(BoundCommand::from_invocation(
                invocation(100),
                src("diff-mode.do"),
                KeymapLayer::MinorMode(diff_mode),
            )),
        );
        diff_bindings.insert(BindingMode::Normal, diff_trie);
        h.push_layer(
            PushLayerKind::MinorMode(diff_mode),
            "diff-mode",
            diff_bindings,
        );

        // overlay-mode binds the same `do` → command 200.
        let mut overlay_bindings = HashMap::new();
        let mut overlay_trie = KeymapTrie::new();
        overlay_trie.insert(
            &[lit('d'), lit('o')],
            Arc::new(BoundCommand::from_invocation(
                invocation(200),
                src("overlay.do"),
                KeymapLayer::MinorMode(overlay_mode),
            )),
        );
        overlay_bindings.insert(BindingMode::Normal, overlay_trie);
        h.push_layer(
            PushLayerKind::MinorMode(overlay_mode),
            "overlay",
            overlay_bindings,
        );

        // Buffer A: only diff-mode active → diff-mode.do wins.
        match h.lookup_with_context(
            BindingMode::Normal,
            &[pressed('d'), pressed('o')],
            &[diff_mode],
        ) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(100));
            }
            other => panic!("expected diff-mode.do, got {other:?}"),
        }

        // Buffer B: only overlay-mode active → overlay.do wins.
        match h.lookup_with_context(
            BindingMode::Normal,
            &[pressed('d'), pressed('o')],
            &[overlay_mode],
        ) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, CommandId::new(200));
            }
            other => panic!("expected overlay.do, got {other:?}"),
        }

        // Buffer C: neither active → no binding.
        let neither =
            h.lookup_with_context(BindingMode::Normal, &[pressed('d'), pressed('o')], &[]);
        assert!(matches!(neither, LookupResult::Unbound));
    }

    /// K.1.c: "last-activated wins" for overlapping minor-mode
    /// bindings — the per-buffer activation order in
    /// `active_modes` is iterated in order, with later
    /// entries overlaying earlier ones (matching emacs's
    /// `minor-mode-map-alist` re-promotion semantics).
    /// Reordering `active_modes` flips the winner without
    /// re-registering anything in the keymap registry.
    #[test]
    fn lookup_with_context_last_activated_wins() {
        let h = KeymapHandle::new();
        let mode_a = ModeId::new("mode-a");
        let mode_b = ModeId::new("mode-b");

        let bind_in = |mode_id: ModeId, cmd: u64| {
            let mut bindings = HashMap::new();
            let mut trie = KeymapTrie::new();
            trie.insert(
                &[lit('x')],
                Arc::new(BoundCommand::from_invocation(
                    invocation(cmd),
                    src("test"),
                    KeymapLayer::MinorMode(mode_id),
                )),
            );
            bindings.insert(BindingMode::Normal, trie);
            h.push_layer(PushLayerKind::MinorMode(mode_id), "test", bindings);
        };
        bind_in(mode_a, 1);
        bind_in(mode_b, 2);

        // [a, b] order: b activated last → b wins.
        match h.lookup_with_context(BindingMode::Normal, &[pressed('x')], &[mode_a, mode_b]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(
                    command.command.command,
                    CommandId::new(2),
                    "last-activated (b) must win",
                );
            }
            other => panic!("expected Bound, got {other:?}"),
        }

        // [b, a] order: a activated last → a wins.
        match h.lookup_with_context(BindingMode::Normal, &[pressed('x')], &[mode_b, mode_a]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(
                    command.command.command,
                    CommandId::new(1),
                    "reordering active_modes flips the winner",
                );
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }
}
