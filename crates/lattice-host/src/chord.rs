//! Re-export shim for the chord primitives.
//!
//! K.2.1 (2026-06-01) relocated the canonical definitions of
//! `KeyChord` / `KeyKind` / `KeyMods` / `SpecialKey` /
//! `ChordPattern` (and the chord parser / formatter) into
//! `lattice-protocol`, so any crate at the dependency floor
//! can construct chord values without depending on
//! `lattice-host`. The host's own keymap matcher
//! (`KeymapTrie` / `KeymapLayer` / `BoundCommand`) still lives
//! here; only the wire data moved.
//!
//! This shim preserves the existing
//! `lattice_host::chord::{...}` import paths used by the
//! TUI / GPUI adapters and the host internals. Plan to retire
//! the shim once downstream re-imports flip to
//! `lattice_protocol::chord::*`.

pub use lattice_protocol::chord::*;
