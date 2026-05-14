//! Buffer kind + id + flags -- moved from `lattice-ui-tui` to
//! the renderer-agnostic substrate in Phase 5.2 first wave.
//!
//! The actual definitions live in `lattice-core`; this module
//! is a thin re-export so the canonical host-side path
//! (`lattice_host::buffers::BufferKind`) lives next to the rest
//! of the host's substrate. `lattice-ui-tui::buffers` continues
//! to work via `pub use lattice_host::buffers;` so call sites
//! that haven't migrated yet don't break.

pub use lattice_core::{BufferFlags, BufferId, BufferKind};
