//! [`BufferOptionsResolved`]: a buffer's options were re-resolved.
//!
//! Buffer-local options (`:setlocal`) live on the editor thread, in the host's
//! per-buffer resolution cache. A subsystem whose work runs off that thread (an
//! LLM request on the runtime, say) cannot read the cache, and the global
//! [`crate::ConfigRegistry`] does not hold the buffer-local layer. This event is
//! the generic seam: the host publishes it after every per-buffer resolution, and
//! a subscriber keeps whatever slice of the snapshot it needs. The payload is a
//! snapshot, so a subscriber never sees a half-resolved cache and never has to
//! reach back into the editor.

use std::sync::Arc;

use lattice_core::BufferId;

use crate::resolved::ResolvedOptions;

/// Published by the host after it re-resolves `buffer`'s options, from any
/// layer: `:set`, `:setlocal`, a mode toggle.
#[derive(Debug, Clone)]
pub struct BufferOptionsResolved {
    /// The buffer that was re-resolved.
    pub buffer: BufferId,
    /// The full resolved snapshot, each entry tagged with its origin.
    pub options: Arc<ResolvedOptions>,
}

lattice_protocol::register_event!(
    BufferOptionsResolved,
    "config.buffer-options-resolved",
    "A buffer's options were re-resolved; carries the resolved snapshot so off-thread \
     readers can see buffer-local values.",
    "lattice-config",
);
