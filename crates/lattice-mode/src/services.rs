//! `ServiceRegistry`: typed lookup for subsystem handles that
//! modes need access to from their lifecycle hooks (Phase 3 of
//! the mode-architecture `ModeContext` extension).
//!
//! Used by `lsp-mode`: its `on_deactivate` needs the LSP
//! supervisor handle to send `textDocument/didClose`, plus a
//! buffer-id → URI resolver to know which URI to close. The
//! App registers these at boot via [`ServiceRegistry::register`];
//! modes pull them at activation/deactivation via
//! [`crate::ModeContext::service`].
//!
//! Why typed (not enum / not free-form Any): subsystem handles
//! are stable across renderers. A TUI host and a future GPUI
//! host register the same `LspSupervisorHandle` type; modes pull
//! by type without caring which host installed it. Adding a new
//! service is one `register::<NewService>(...)` call at boot
//! plus a `ctx.service::<NewService>()` consumption — no enum
//! to extend, no breaking change to the trait surface.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// Typed map keyed by [`TypeId`]. Stores `Arc<dyn Any + Send +
/// Sync>` per slot; clones on lookup (cheap — services are
/// `Arc` internally). Built at boot, read-only thereafter.
///
/// **Look up with exactly the type you registered.** The key is
/// `TypeId::of::<T>()` of the value passed to [`register`](Self::register),
/// so registering an `Arc<X>` stores it under `Arc<X>`, and a later
/// `get::<X>()` silently returns `None`. The convention for an
/// already-shared handle is a named alias (`type XHandle = Arc<X>`) used on
/// both sides — see [`BufferStoreHandle`](crate::BufferStoreHandle),
/// [`ModeRegistryHandle`](crate::ModeRegistryHandle) and the other
/// `*Handle` aliases in this crate.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use lattice_mode::ServiceRegistry;
///
/// struct Clock(u64);
/// type ClockHandle = Arc<Clock>;
///
/// let mut services = ServiceRegistry::new();
/// services.register::<ClockHandle>(Arc::new(Clock(42)));
///
/// // Same type on both sides: found (an `Arc<ClockHandle>`, i.e. two layers).
/// assert_eq!(services.get::<ClockHandle>().unwrap().0, 42);
/// // The TypeId pitfall: `Clock` was never registered, only `Arc<Clock>`.
/// assert!(services.get::<Clock>().is_none());
/// ```
#[derive(Default)]
pub struct ServiceRegistry {
    services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ServiceRegistry {
    /// An empty registry. The host fills it at boot; tests build one
    /// with only the services the code under test reads.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a service. Overwrites if a service of the same
    /// type was already registered (last-write-wins — typically
    /// called once per type at boot).
    pub fn register<T: Any + Send + Sync>(&mut self, service: T) {
        self.services.insert(TypeId::of::<T>(), Arc::new(service));
    }

    /// Look up a service by type. Returns a fresh `Arc<T>` clone
    /// when found; `None` when no service of that type is
    /// registered (calling renderer hasn't wired this subsystem
    /// yet, or the mode is running in a stripped-down test
    /// harness).
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let entry = self.services.get(&TypeId::of::<T>())?;
        Arc::clone(entry).downcast::<T>().ok()
    }
}

impl std::fmt::Debug for ServiceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceRegistry")
            .field("registered_count", &self.services.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[derive(Debug)]
    struct ServiceA {
        value: i32,
    }

    #[derive(Debug)]
    struct ServiceB {
        label: &'static str,
    }

    #[test]
    fn register_and_lookup_by_type() {
        let mut r = ServiceRegistry::new();
        r.register(ServiceA { value: 42 });
        r.register(ServiceB { label: "hello" });
        let a = r.get::<ServiceA>().unwrap();
        assert_eq!(a.value, 42);
        let b = r.get::<ServiceB>().unwrap();
        assert_eq!(b.label, "hello");
    }

    #[test]
    fn missing_service_returns_none() {
        let r = ServiceRegistry::new();
        assert!(r.get::<ServiceA>().is_none());
    }

    #[test]
    fn register_overwrites_previous_of_same_type() {
        let mut r = ServiceRegistry::new();
        r.register(ServiceA { value: 1 });
        r.register(ServiceA { value: 2 });
        assert_eq!(r.get::<ServiceA>().unwrap().value, 2);
    }
}
