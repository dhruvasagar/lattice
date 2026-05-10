//! M.4 follow-up: file-tree buffer model moved to
//! `lattice-file-tree`. This shim re-exports the public surface so
//! the existing `crate::file_tree::*` callsites in App keep
//! compiling. New callers should import from `lattice_file_tree`
//! directly.

pub use lattice_file_tree::*;
