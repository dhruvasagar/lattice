//! Declarative contributions on [`crate::Mode`].
//!
//! `Keymap` and `KeymapBinding` moved to `lattice-keymap::contribution`
//! in K.3 (2026-06-07) — re-exported here for backward compatibility.
//!
//! Stubs still pending real impls:
//! - [`Subscription`] -- when the typed event bus stabilises.
//! - [`DecorationProvider`] -- M.4 / decoration registry.

use lattice_core::BufferId;

use crate::services::ServiceRegistry;

pub use lattice_keymap::{Keymap, KeymapBinding};

/// Stub. Real type lands when the typed event bus stabilises
/// a subscription shape for modes.
#[derive(Debug, Clone)]
pub struct Subscription {
    _private: (),
}

/// Stub. M.4 replaces with the real decoration-provider type.
#[derive(Debug, Clone)]
pub struct DecorationProvider {
    _private: (),
}

/// A single status-line segment contributed by a [`crate::Mode`].
/// `text` is the rendered string (e.g. `"lsp"`, `"+5 ~3"`,
/// `"[REC @q]"`). `priority` controls left-to-right ordering:
/// lower values appear closer to the path label; higher values
/// appear closer to the position indicator. The renderer sorts
/// ascending and joins with two spaces.
#[derive(Debug, Clone)]
pub struct StatusLineItem {
    pub text: String,
    pub priority: u8,
}

/// Read-only context passed to [`crate::Mode::status_line_items`].
/// Carries the buffer id the status line is being composed for,
/// plus a service registry the App populates with render-state
/// snapshots (LSP progress map, diff sign map, etc.) before
/// calling into each mode. Modes call [`Self::service`] to pull
/// their own typed data without `lattice-mode` importing
/// feature-crate types — same dep-inversion pattern as
/// [`crate::ModeContext::service`].
pub struct StatusLineCtx<'a> {
    pub buffer_id: BufferId,
    services: &'a ServiceRegistry,
}

impl<'a> StatusLineCtx<'a> {
    pub fn new(buffer_id: BufferId, services: &'a ServiceRegistry) -> Self {
        Self { buffer_id, services }
    }

    /// Look up a service by type. Returns `None` when no service
    /// of that type was registered for this call. Convention:
    /// register with `T = ConcreteStruct`; the registry wraps in
    /// `Arc` internally and `get` returns `Option<Arc<T>>`.
    pub fn service<T: std::any::Any + Send + Sync>(
        &self,
    ) -> Option<std::sync::Arc<T>> {
        self.services.get::<T>()
    }
}
