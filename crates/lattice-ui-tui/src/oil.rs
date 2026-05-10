//! M.4 follow-up: oil-buffer model moved to `lattice-oil`. This
//! shim re-exports the public surface so the existing
//! `crate::oil::*` callsites in App keep compiling. New callers
//! should import from `lattice_oil` directly.

pub use lattice_oil::*;
