//! Re-export shim — `KeymapCapability`, `KeymapError`,
//! `KeymapHandle`, `KeymapRegistry`, `LayerId`, and
//! `PushLayerKind` moved to `lattice-keymap` in K.3
//! (2026-06-07). `KeymapReverseLookupHandle` stays here
//! because it implements `lattice_completion::KeymapReverseLookup`
//! — `lattice-keymap` cannot depend on `lattice-completion`.
pub use lattice_keymap::{
    KeymapCapability, KeymapError, KeymapHandle, KeymapRegistry, LayerId, PushLayerKind,
};

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_completion::{KeybindingSource, KeymapReverseLookup};
use lattice_grammar::CommandId;
use lattice_keymap::KeymapLayer;
use lattice_protocol::KeyChord;

/// MARG.2 (2026-06-03): adapter that implements
/// [`lattice_completion::KeymapReverseLookup`] over a
/// [`KeymapRegistry`]'s reverse cache + a [`CommandRegistry`]
/// for the canonical-name → [`CommandId`] resolution. Editor
/// boot constructs one and passes it to
/// [`lattice_completion::KeybindingAnnotator::new`].
///
/// The adapter holds Arc clones of both registries; cache
/// loads are wait-free via `ArcSwap::load`.
///
/// MARG.3 (2026-07-15): the reverse cache now carries
/// [`KeymapLayer`] provenance alongside each chord, and
/// [`chords_with_source`](KeymapReverseLookup::chords_with_source)
/// converts layers to [`KeybindingSource`] for the picker's
/// mode-aware filtering + provenance column.
pub struct KeymapReverseLookupHandle {
    // MARG.3: the reverse cache carries `KeymapLayer` provenance per chord.
    reverse_cache: Arc<ArcSwap<HashMap<CommandId, Vec<(KeyChord, KeymapLayer)>>>>,
    // B3b: the `ArcSwap` handle so a plugin command's keybinding annotation
    // resolves against runtime registrations too; `.load()` per lookup.
    command_registry: lattice_grammar::CommandRegistryHandle,
}

impl KeymapReverseLookupHandle {
    /// Construct from a `KeymapHandle`'s reverse cache and a
    /// command registry. Editor boot calls this once after the
    /// builtin catalog is registered.
    pub fn new(
        handle: &KeymapHandle,
        command_registry: lattice_grammar::CommandRegistryHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            reverse_cache: handle.reverse_cache_arc(),
            command_registry,
        })
    }
}

impl KeymapReverseLookup for KeymapReverseLookupHandle {
    fn chords_for(&self, command_name: &str) -> Vec<KeyChord> {
        let Some(id) = self.command_registry.load().id_by_name(command_name) else {
            return Vec::new();
        };
        let cache = self.reverse_cache.load();
        cache
            .get(&id)
            .map(|entries| entries.iter().map(|(c, _)| *c).collect())
            .unwrap_or_default()
    }

    fn chords_with_source(&self, command_name: &str) -> Vec<(KeyChord, KeybindingSource)> {
        let Some(id) = self.command_registry.load().id_by_name(command_name) else {
            return Vec::new();
        };
        let cache = self.reverse_cache.load();
        cache
            .get(&id)
            .map(|entries| {
                entries
                    .iter()
                    .map(|(chord, layer)| (*chord, layer_to_source(*layer)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn layer_to_source(layer: KeymapLayer) -> KeybindingSource {
    match layer {
        KeymapLayer::Builtin | KeymapLayer::User | KeymapLayer::Buffer => {
            KeybindingSource::AlwaysOn
        }
        KeymapLayer::MajorMode(mode_id) | KeymapLayer::MinorMode(mode_id) => {
            KeybindingSource::Mode(Arc::from(mode_id.as_str()))
        }
    }
}
