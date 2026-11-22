//! OB.3c: host-side [`lattice_mode::ActiveModesQuery`] — which modes are active
//! on a buffer, answered off the editor thread.
//!
//! The editor's per-buffer mode sets sit behind `&mut Editor`; a plugin call
//! runs on another thread. This reads the published [`RenderState`]'s
//! `modes` map instead, the same snapshot every renderer reads, so the answer
//! is the set as of the last publish. Registered once at boot under
//! [`lattice_mode::ActiveModesQueryHandle`]; the plugin host uses it to gate
//! `host-services.buffer-text` on the caller's own modes.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::render_state::RenderState;

/// Reads a buffer's active modes over the published render state: one
/// `ArcSwap::load` and a `HashMap` lookup.
#[derive(Debug)]
pub(crate) struct HostActiveModesQuery {
    render_state: Arc<ArcSwap<RenderState>>,
}

impl HostActiveModesQuery {
    pub(crate) fn new(render_state: Arc<ArcSwap<RenderState>>) -> Self {
        Self { render_state }
    }
}

impl lattice_mode::ActiveModesQuery for HostActiveModesQuery {
    fn active_on(&self, buffer: lattice_core::BufferId) -> Vec<lattice_mode::ModeId> {
        self.render_state
            .load()
            .modes
            .map
            .get(&buffer)
            .map(|active| active.ids().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_mode::ActiveModesQuery;

    #[test]
    fn a_buffer_the_published_state_does_not_hold_has_no_modes() {
        let rs = Arc::new(ArcSwap::from_pointee(RenderState::default()));
        let q = HostActiveModesQuery::new(rs);
        assert!(q.active_on(lattice_core::BufferId(3)).is_empty());
    }
}
