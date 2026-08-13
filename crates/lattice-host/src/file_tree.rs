//! File-tree buffer model -- re-exported from `lattice-file-tree`.
//!
//! Moved from `lattice-ui-tui` in Phase 5.2 first wave. The
//! definitions live in `lattice-file-tree`; this module
//! provides the canonical host-side import path so consumers
//! can write `lattice_host::file_tree::*` instead of reaching
//! into `lattice-file-tree` directly. Existing
//! `lattice_ui_tui::file_tree::*` imports keep working via
//! a `pub use lattice_host::file_tree;` re-export in
//! lattice-ui-tui's lib.rs.

pub use lattice_listing::file_tree::*;
