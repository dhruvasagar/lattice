//! Declarative contributions on [`crate::Mode`].
//!
//! `Keymap` and `KeymapBinding` moved to `lattice-keymap::contribution`
//! in K.3 (2026-06-07) — re-exported here for backward compatibility.
//!
//! Stubs still pending real impls:
//! - [`DecorationProvider`] -- M.4 / decoration registry.

use lattice_core::BufferId;

use crate::services::ServiceRegistry;

pub use lattice_keymap::{Keymap, KeymapBinding};

/// RAII subscription handle. Unsubscribes from the event bus on drop.
///
/// Acquire in `Mode::on_activate` via `ctx.events_handle()` +
/// `EventBus::subscribe_typed`; store in the mode's `Guard` struct so
/// deactivation cleanup is compiler-enforced. Modes with conditional
/// subscriptions (e.g. skip when no URI) use `Option<Subscription>`.
///
/// MO.4.c: replaces the `_private:()` stub; `Mode::subscriptions()`
/// removed — `on_activate` + Guard IS the subscription mechanism.
pub struct Subscription {
    bus: std::sync::Arc<lattice_runtime::EventBus>,
    id: lattice_runtime::SubscriptionId,
}

impl Subscription {
    pub fn new(
        bus: std::sync::Arc<lattice_runtime::EventBus>,
        id: lattice_runtime::SubscriptionId,
    ) -> Self {
        Self { bus, id }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.unsubscribe(self.id);
    }
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
    Severity {
        line: u32,
        level: GutterSeverityLevel,
    },
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
        Self {
            buffer_id,
            services,
        }
    }

    pub fn service<T: std::any::Any + Send + Sync>(&self) -> Option<std::sync::Arc<T>> {
        self.services.get::<T>()
    }
}

// ML.3: `StatusLineItem` + `StatusLineCtx` retired with the
// `Mode::status_line_items` trait. Modes contribute modeline content as
// registered elements pushed over the event bus
// (`crate::ModelineElementUpdate`), not via a render-path service pull.
