//! `lattice-keymap` — the single home for all keymap types, trie resolution,
//! and the runtime registry.
//!
//! Dependency position in the workspace:
//!   lattice-protocol → lattice-grammar → lattice-keymap
//!     → lattice-mode → lattice-host
//!
//! Nothing in this crate may import from `lattice-mode` or `lattice-host`.

pub mod mode_id;

pub use mode_id::ModeId;
