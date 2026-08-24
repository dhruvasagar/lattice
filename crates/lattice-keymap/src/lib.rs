//! `lattice-keymap` — the single home for all keymap types, trie resolution,
//! and the runtime registry.
//!
//! Dependency position in the workspace:
//!   lattice-protocol → lattice-grammar → lattice-keymap
//!     → lattice-mode → lattice-host
//!
//! Nothing in this crate may import from `lattice-mode` or `lattice-host`.

pub mod binding_mode;
pub mod contribution;
pub mod keymap_entry;
pub mod mode_id;

pub use binding_mode::BindingMode;
pub use contribution::{Keymap, KeymapBinding};
pub use keymap_entry::{KeymapEntry, default_keymap, entries, lookup};
pub use mode_id::ModeId;

pub mod trie;
pub use lattice_protocol::ChordPattern;
pub use trie::{BoundCommand, KeymapLayer, KeymapTrie, LookupResult};

pub mod registry;
pub use registry::{
    DEFAULT_LEADER, KeymapCapability, KeymapError, KeymapHandle, KeymapRegistry, LayerId,
    PushLayerKind, expand_leader,
};

pub mod resolution;
pub use resolution::{KeymapResolution, LayerHit, parse_describe_key_arg};
