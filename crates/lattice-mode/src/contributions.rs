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

/// Stub. Reserved for the WIT plugin-facing contribution surface
/// (M.10). Not used by the `Mode` trait today — see
/// `Mode::gutter_decorations` for the live decoration path.
#[derive(Debug, Clone)]
pub struct DecorationProvider {
    _private: (),
}

/// Renderer-agnostic diff-sign kind for the gutter diff column.
/// Mirrors `DiffSignKind` without importing `lattice-host`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GutterDiffKind {
    Add,
    Remove,
    Change,
    Conflict,
}

/// Renderer-agnostic diagnostic severity level for the gutter
/// severity column. Ordered ascending by severity so `max()` selects
/// the most severe: `Hint < Info < Warning < Error`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GutterSeverityLevel {
    Hint,
    Info,
    Warning,
    Error,
}

/// A single gutter decoration contributed by a [`crate::Mode`].
/// Each variant maps to one physical gutter column.
#[derive(Copy, Clone, Debug)]
pub enum GutterDecoration {
    /// Diff-sign column (between severity and line numbers).
    Diff { line: u32, kind: GutterDiffKind },
    /// LSP diagnostic severity column (leftmost gutter cell).
    Severity { line: u32, level: GutterSeverityLevel },
}

/// Read-only context passed to [`crate::Mode::gutter_decorations`].
/// Same dep-inversion pattern as [`StatusLineCtx`]: the App populates
/// a `ServiceRegistry` with typed render-state snapshots; modes pull
/// their own data via [`Self::service`].
pub struct DecorationCtx<'a> {
    pub buffer_id: BufferId,
    services: &'a ServiceRegistry,
}

impl<'a> DecorationCtx<'a> {
    pub fn new(buffer_id: BufferId, services: &'a ServiceRegistry) -> Self {
        Self { buffer_id, services }
    }

    pub fn service<T: std::any::Any + Send + Sync>(
        &self,
    ) -> Option<std::sync::Arc<T>> {
        self.services.get::<T>()
    }
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
