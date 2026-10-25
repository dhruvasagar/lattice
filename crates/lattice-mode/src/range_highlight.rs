//! Ranges of a buffer's text a mode wants tinted while it is active — a
//! snippet's placeholders today; linked-editing pairs and secondary cursors
//! are the same shape.
//!
//! A mode registers a [`RangeHighlightSource`] for one buffer, from its
//! `on_activate`, through the host's [`RangeHighlightRegistrar`] service, and
//! keeps the returned [`RangeHighlightRegistration`] in its guard: dropping
//! it (the mode deactivating) withdraws the source. The host calls the
//! active buffer's sources each time it publishes that buffer's render
//! state, with the same document snapshot the published text comes from, so
//! a highlight never trails the text it covers — the ranges move with every
//! keystroke, the way a selection does. Both renderers paint them with the
//! cursor-coupled overlays, in the theme element the source names.
//!
//! Not the gutter seam ([`crate::decoration_source`], signs beside a line)
//! and not synthetic-buffer highlighting
//! ([`crate::pending_synthetic_highlights`], asynchronous foreground spans
//! that may lag an edit): this one tints text, and is never stale.
//!
//! Sources run on the editor actor, never on a render thread, and should be
//! cheap: they are called once per publish of the active buffer.

use std::ops::Range;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_theme::ElementId;

use crate::context::ModeContext;
use crate::mode::ModeId;

/// One range to tint: absolute utf-8 bytes into the document, and the theme
/// element whose background tints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeHighlight {
    /// The tinted text, in absolute utf-8 bytes.
    pub bytes: Range<usize>,
    /// The element whose background tints it.
    pub element: ElementId,
}

/// What a mode registers: the highlights for the buffer as `snapshot` holds
/// it. Empty when there is nothing to show.
pub type RangeHighlightSource =
    Arc<dyn Fn(&lattice_runtime::DocumentSnapshot) -> Vec<RangeHighlight> + Send + Sync>;

/// The host's side: where sources are kept, per buffer and per owning mode.
pub trait RangeHighlightRegistrar: Send + Sync {
    /// Register `source` for `buffer` under `owner`, replacing the source
    /// `owner` had there.
    fn register(&self, buffer: BufferId, owner: ModeId, source: RangeHighlightSource);
    /// Withdraw `owner`'s source for `buffer`, if any.
    fn unregister(&self, buffer: BufferId, owner: &ModeId);
}

/// The service a mode looks up (`ctx.service::<RangeHighlightRegistrarHandle>()`).
pub type RangeHighlightRegistrarHandle = Arc<dyn RangeHighlightRegistrar>;

/// Withdraws a source when dropped. A mode keeps it in its guard.
pub struct RangeHighlightRegistration {
    registrar: RangeHighlightRegistrarHandle,
    buffer: BufferId,
    owner: ModeId,
}

impl Drop for RangeHighlightRegistration {
    fn drop(&mut self) {
        self.registrar.unregister(self.buffer, &self.owner);
    }
}

/// Register `source` for the buffer `ctx` activates on, owned by `owner`.
/// `None` in a harness with no registrar service — the mode works, its
/// ranges are just not tinted.
pub fn register_range_highlights(
    ctx: &ModeContext,
    owner: ModeId,
    source: RangeHighlightSource,
) -> Option<RangeHighlightRegistration> {
    let registrar = ctx
        .service::<RangeHighlightRegistrarHandle>()
        .map(|outer| (*outer).clone())?;
    let buffer = BufferId(ctx.buffer_id().raw() as u32);
    registrar.register(buffer, owner, source);
    Some(RangeHighlightRegistration {
        registrar,
        buffer,
        owner,
    })
}
