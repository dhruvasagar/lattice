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
use lattice_grammar::{CommandId, CommandRegistry};
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
pub struct KeymapReverseLookupHandle {
    reverse_cache: Arc<ArcSwap<HashMap<CommandId, Vec<KeyChord>>>>,
    command_registry: Arc<CommandRegistry>,
}

impl KeymapReverseLookupHandle {
    /// Construct from a `KeymapHandle`'s reverse cache and a
    /// command registry. Editor boot calls this once after the
    /// builtin catalog is registered.
    pub fn new(handle: &KeymapHandle, command_registry: Arc<CommandRegistry>) -> Arc<Self> {
        Arc::new(Self {
            reverse_cache: handle.reverse_cache_arc(),
            command_registry,
        })
    }
}

impl lattice_completion::KeymapReverseLookup for KeymapReverseLookupHandle {
    fn chords_for(&self, command_name: &str) -> Vec<KeyChord> {
        let Some(id) = self.command_registry.id_by_name(command_name) else {
            return Vec::new();
        };
        let cache = self.reverse_cache.load();
        cache.get(&id).cloned().unwrap_or_default()
    }
}
