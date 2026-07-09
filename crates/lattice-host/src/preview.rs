//! Preview isolation (PI series).
//!
//! In-pane picker preview renders buffer B as an **isolated read-only
//! projection** in a pane — never mutating the committed buffer A, the
//! global active-buffer hot state, or A's resolved options / mode stack.
//! See `docs/dev/architecture/preview-isolation.md` for the contract and
//! `docs/dev/operations/slice-plans/preview-isolation.md` (PI series) for
//! sequencing.
//!
//! This module owns the two host-side pieces the design calls out:
//!
//! - [`PreviewOverride`] — the ephemeral per-pane sidecar value
//!   (`Editor::preview_overrides`, keyed by `PaneId`). It records the
//!   *displayed* buffer + the preview viewport (cursor / scroll) while a
//!   pane's *committed* `buffer_id` stays put. Baked into the published
//!   pane-tree leaves at render-publish time (`build_render_state`) so the
//!   renderers show the displayed buffer while `:ls` / modeline / dispatch
//!   keep reading the committed one.
//! - [`PreviewMode`] — the `preview-mode` minor (§10.2 resolution: option
//!   (a)) that owns `ReadOnly = true` and, by its presence on B's own mode
//!   stack, the ephemeral "this buffer is being previewed" marker. It
//!   deliberately does **not** touch `CursorLine`, so a preview keeps the
//!   buffer's cursorline (the target line stays highlighted — e.g. an LSP
//!   reference preview).

use lattice_core::{BufferId, BufferKind};
use lattice_mode::{CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};
use lattice_protocol::position::Position;

/// PI.1: a pane's ephemeral preview projection. Stored host-side in
/// `Editor::preview_overrides` (keyed by `PaneId`) so the live
/// `Editor::pane_tree` stays committed + geometry-only; the override is
/// baked into the *published* pane-tree leaves each frame.
///
/// The preview cursor / scroll live here (not on `Editor::cursor` /
/// `Editor::scroll`) so entering / leaving preview never disturbs the
/// committed buffer's viewport — exit is dropping this value, not
/// restoring anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewOverride {
    /// The buffer the pane currently *displays*. A real registry buffer
    /// with its own resolved options (computed read-only via
    /// `mount_preview`); the pane renders it exactly like an inactive
    /// split, plus a cursorline at [`Self::cursor`].
    pub buffer_id: BufferId,
    /// The displayed buffer's kind (drives the renderer's per-kind pane
    /// dispatch). Preview is Document-only for now (design §10.4).
    pub buffer: BufferKind,
    /// Preview cursor inside the displayed buffer. Location previews
    /// (`gr` / grep) seat it on the target line; file previews start at
    /// the top.
    pub cursor: Position,
    /// First visible line of the displayed buffer in the pane.
    pub scroll: u32,
}

/// PI.2 (§10.2 option (a)): the `preview-mode` minor. Contributes
/// `ReadOnly = true` to the buffer it is active on; its *presence* on a
/// buffer's mode stack is the ephemeral "previewing" marker (introspect
/// via `:describe-mode`). Activated only on the previewed buffer B's own
/// stack (never A's), so B's `resolved_options` reflect read-only while
/// the committed buffer is untouched.
pub struct PreviewMode;

impl PreviewMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("preview-mode")
    }
}

impl Mode for PreviewMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> lattice_config::OptionOverrideSet {
        // ReadOnly only — CursorLine is intentionally left to resolve
        // from the buffer's own layers so preview keeps the cursorline
        // that marks the target line.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
