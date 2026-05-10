//! M.4 follow-up: help-buffer model moved to `lattice-help`. This
//! shim re-exports the public surface so the existing
//! `crate::help::*` callsites in App keep compiling. New callers
//! should import from `lattice_help` directly.

pub use lattice_help::*;
