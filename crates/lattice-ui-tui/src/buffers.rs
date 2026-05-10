//! M.4 follow-up: `BufferKind` / `BufferId` / `BufferFlags` moved
//! to `lattice-core`. This module re-exports them so existing
//! `crate::buffers::BufferKind` etc. imports keep working during
//! the transition. Drop this shim in a future cleanup once every
//! call site imports from `lattice_core` directly.

pub use lattice_core::{BufferFlags, BufferId, BufferKind};
