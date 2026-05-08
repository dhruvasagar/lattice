//! `ModeContext`: the read-only handle passed to
//! [`crate::Mode::on_activate`] / [`crate::Mode::on_deactivate`].
//!
//! Per `mode-architecture.md` §5.2, lifecycle hooks may do side
//! effects (spawn a server, open a watcher) but must NOT mutate
//! the config registry, the keymap registry, or another mode's
//! state. Those are owned by the registry through the
//! declarative trait methods. The context API enforces this by
//! exposing only:
//!
//! - The `BufferId` the activation is operating on.
//! - (Future) read-only buffer access -- M.3+ when the
//!   `Document` carries `ActiveModes`, the context can borrow
//!   the buffer's text + metadata.
//! - (Future) an `&EventBus` so the mode can publish (not
//!   subscribe -- subscriptions are declarative via
//!   `Mode::subscriptions`) one-shot informational events.
//!   Wired in M.4 alongside the event-bus integration.
//!
//! M.1 keeps the surface minimal: just `BufferId`. The trait
//! method signatures already use `&ModeContext` (not
//! `&mut ModeContext`), so adding fields later does not break
//! the API.

use lattice_protocol::ids::BufferId;

/// Read-only handle into the buffer the mode is being activated
/// against (or deactivated from). Lifecycle hooks read this to
/// know *which* buffer they're affecting, and (in later slices)
/// to read buffer state.
///
/// Construction is via [`ModeContext::for_buffer`]; the registry
/// builds one per activation cycle.
#[derive(Debug, Clone, Copy)]
pub struct ModeContext {
    buffer_id: BufferId,
}

impl ModeContext {
    pub fn for_buffer(buffer_id: BufferId) -> Self {
        Self { buffer_id }
    }

    pub fn buffer_id(&self) -> BufferId {
        self.buffer_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_round_trips_buffer_id() {
        let ctx = ModeContext::for_buffer(BufferId::new(7));
        assert_eq!(ctx.buffer_id(), BufferId::new(7));
    }
}
