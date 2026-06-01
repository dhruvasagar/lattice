//! M.2.b.2 (2026-06-01): `MultibufferMode` — the major mode bound
//! to `BufferKind::Multibuffer` via H.2's `Mode::target_buffer_kind`
//! declaration.
//!
//! Thin major: contributes `ReadOnly = true` + `NoFile = true`
//! (M.3 will make `ReadOnly` conditional once edit propagation
//! lands). Excerpt-jump motion keymap (`]e` / `[e` / `]E` / `[E`)
//! arrives in M.2.b.3. Provider-specific behaviour layers on as
//! minor modes (`ProjectSearchMultibufferMode` etc., M.6+).
//!
//! `register_multibuffer_modes(®istry, &events, mb_registry)`
//! is the single boot-wiring entry point the host calls. It
//! registers `MultibufferMode` AND wires the
//! `Event::DocumentClosed` subscriber that removes the closed
//! multibuffer's entry from the `MultibufferRegistry` (cleanup
//! contract per `multibuffer-views.md` §3.7).
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.7.

use std::sync::Arc;

use lattice_config::{OptionOverrideSet, overrides};
use lattice_core::BufferKind;
use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

use crate::registry::MultibufferRegistryHandle;

/// Major mode for buffers of [`BufferKind::Multibuffer`]. Generic;
/// knows nothing about *why* excerpts exist. Provider-specific
/// behaviour (project-search, lsp-references, etc.) is layered as
/// minor modes registered by each provider's own
/// `register_<provider>` helper.
pub struct MultibufferMode;

impl MultibufferMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("multibuffer-mode")
    }
}

impl Mode for MultibufferMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// H.2 (2026-05-31) + M.2.b.2 (2026-06-01): buffers whose
    /// `BufferKind` is `Multibuffer` dispatch to this major via
    /// `ModeRegistry::find_major_for_kind`.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Multibuffer)
    }

    fn options(&self) -> OptionOverrideSet {
        // M.2.b.2: views are read-only until M.3 (edit
        // propagation) lands the back-translation path. NoFile
        // because multibuffers aren't on-disk files — `:w` is a
        // no-op (M.3 may change this to "apply edits to sources"
        // semantics).
        overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        // Marker mode for M.2.b.2 — no per-buffer subscriptions
        // or resource grabs. M.4 will likely keep this empty too
        // (live-update subscriptions are owned by the handle
        // itself, not the mode).
        Box::pin(async { Ok(()) })
    }
}

/// Boot wiring entry point. Called once from `lattice-host`'s
/// `editor_boot::boot` after the `ServiceRegistry` is populated
/// with [`MultibufferRegistryHandle`] and the `EventBus` exists.
///
/// 1. Registers [`MultibufferMode`] against `registry` (so
///    `ModeRegistry::find_major_for_kind(BufferKind::Multibuffer)`
///    returns its id post-H.2).
/// 2. Subscribes a `DocumentClosed` cleanup task that removes a
///    closed multibuffer's entry from `multibuffer_registry`.
///    Uses the existing `SubscriptionTarget::Channel` shape +
///    `tokio::spawn` for the drain loop (same pattern as the
///    LSP / mode-lifecycle drains).
pub fn register_multibuffer_modes(
    registry: &mut ModeRegistry,
    events: &Arc<EventBus>,
    multibuffer_registry: MultibufferRegistryHandle,
) {
    registry
        .register(MultibufferMode)
        .expect("multibuffer-mode registers without conflict at boot");

    // Cleanup subscriber: only wire when a tokio runtime is in
    // scope. Production boot runs inside the App's runtime so
    // this fires; tests that construct `Editor` outside a runtime
    // (`lattice-host` lib tests) gracefully skip the subscriber
    // wiring — the registry simply leaks entries for the
    // (short-lived) test process, which is observably fine
    // because no test asserts cleanup behaviour.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!(
            "register_multibuffer_modes: no tokio runtime in scope; \
             skipping DocumentClosed cleanup subscriber wiring \
             (expected in test paths)"
        );
        return;
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    events.subscribe(
        EventFilter::kind(EventKind::DocumentClosed),
        SubscriptionTarget::Channel(tx),
    );

    let reg = multibuffer_registry;
    handle.spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Event::DocumentClosed { id } = event {
                reg.remove_by_document_id(id);
            }
        }
    });
}
