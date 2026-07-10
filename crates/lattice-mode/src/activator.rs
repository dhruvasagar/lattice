//! `ModeActivator`: synchronous activation surface for extension
//! crates that create buffers and need their major / minor modes
//! activated on the host side.
//!
//! M.2.b.2 (2026-06-01) introduces this trait so `lattice-multibuffer`
//! (and every future in-tree provider that ships within it) can
//! atomically insert + activate-major a buffer without depending on
//! `lattice-host` directly. The activation cascade requires `&mut Editor`
//! (it mutates per-buffer mode state, options cache, completion sources,
//! LSP wiring) — but extension crates can't import `lattice-host`'s
//! `Editor` type. This trait, living in `lattice-mode`, is the seam
//! both crates depend on.
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.7 + the H-series
//! deferral note in `docs/dev/architecture/kind-agnostic-buffers.md`
//! §10 for the design.

use std::sync::Arc;

use lattice_core::{BufferId, BufferKind};

use crate::mode::ModeId;
use crate::services::ServiceRegistry;

/// Activation surface implemented by `lattice-host::Editor` and
/// consumed by extension crates that need to drive activation
/// without holding a typed `&mut Editor`.
///
/// All three methods run synchronously on the App thread (caller
/// owns `&mut Self`). The implementor is responsible for routing
/// the cascade's renderer signals into its own pending-signals
/// queue — extension crates never see `RendererSignal`, keeping
/// host-layer types out of `lattice-mode`.
///
/// Failures (mode not registered, missing capability, conflict)
/// are logged + swallowed by the impl (matching the existing
/// `Editor::activate_*` helpers' shape). Callers that need to
/// observe activation outcome subscribe to
/// [`crate::ModeEvent`] on the event bus.
pub trait ModeActivator {
    /// Activate the major mode bound to `kind` on `buffer`. Uses
    /// the [`crate::ModeRegistry::find_major_for_kind`] lookup
    /// (populated by `Mode::target_buffer_kind` declarations)
    /// to resolve the mode id. No-ops if a major is already
    /// active on the buffer (idempotency / preserve-intent).
    fn activate_major_for_kind(&mut self, buffer: BufferId, kind: BufferKind);

    /// Activate a minor mode by id on `buffer`. The mode must
    /// be registered. Idempotent: re-activating an already-active
    /// minor is a no-op.
    fn activate_minor_by_id(&mut self, buffer: BufferId, mode: ModeId);

    /// Cheap-clone handle to the App's [`ServiceRegistry`]. Used
    /// by extension-crate trigger functions that need to look up
    /// service handles (`BufferStoreHandle`, per-provider
    /// services, the per-extension-crate registries) without
    /// fighting the borrow checker against the `&mut Self`
    /// activator borrow.
    fn services(&self) -> Arc<ServiceRegistry>;

    /// K.4.6 (2026-06-02): register a virtual-row provider against
    /// `buffer`. Used by extension crates that contribute virtual
    /// rows for their own buffer kinds (multibuffer excerpt
    /// headers — `MultibufferHeaderProvider`; future fold-range
    /// providers, diff-hunk overlays, LSP code-lens, ...). The
    /// host-side impl forwards to
    /// `Editor::virtual_row_providers.register(buffer, provider)`;
    /// the worker picks the provider up on its next wake.
    ///
    /// Returns `true` on registration, `false` if a provider with
    /// the same `ProviderId` was already registered in the same
    /// buffer scope (no replacement — caller `unregister`s first
    /// via the existing registry handle). The default impl
    /// returns `false` so test activators that don't wire the
    /// virtual-row pipeline behave as no-ops; production impls
    /// (Editor) override.
    ///
    /// Paramount-#2 anchor: every mode contributing to a buffer
    /// registers its own virtual rows via this seam, matching
    /// the mode-owns-its-surface principle (keymaps via
    /// `register_<mode>_keymap`, virtual rows via this method,
    /// future status-line items via similar). WIT plugin path
    /// inherits the trait surface.
    fn register_virtual_row_provider(
        &mut self,
        buffer: lattice_core::BufferId,
        provider: Arc<dyn lattice_cells::VirtualRowProvider>,
    ) -> bool {
        let _ = (buffer, provider);
        false
    }
}

/// AUX‑2: service-accessible interface for registering virtual row providers
/// on a buffer. The host registers an `Arc<dyn VirtualRowRegistrar>` during boot
/// so subsystems (e.g. `lattice-ai`) can register headerlines without depending
/// on `lattice-host`'s concrete `VirtualRowProviderRegistry`.
pub trait VirtualRowRegistrar: Send + Sync {
    /// Register `provider` against `buffer`. Returns `false` if a provider with
    /// the same `ProviderId` is already registered in the same buffer scope.
    fn register(&self, buffer: BufferId, provider: Arc<dyn lattice_cells::VirtualRowProvider>)
    -> bool;
    /// Remove the provider identified by `id` from `buffer`'s scope.
    fn unregister(&self, buffer: BufferId, id: lattice_cells::ProviderId) -> bool;
}
