//! Re-export shim — `KeymapTrie`, `KeymapLayer`, `BoundCommand`, and
//! `LookupResult` moved to `lattice-keymap` in K.3 (2026-06-07).
//! Existing `use crate::keymap_trie::{...}` callers in this crate
//! continue to work unchanged.
pub use lattice_keymap::{BoundCommand, KeymapLayer, KeymapTrie, LookupResult};
pub use lattice_protocol::ChordPattern;
