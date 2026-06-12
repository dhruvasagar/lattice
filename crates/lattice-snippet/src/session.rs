//! SN.2: the live snippet-expansion session, relocated off the host
//! `Editor` so the mode-owned placeholder-navigation handlers can
//! reach it.
//!
//! While a snippet is expanding, exactly one [`ActiveSnippet`] is
//! live (the tabstop groups + the focused index). Before SN.2 this
//! lived as `Editor.active_snippet: Option<ActiveSnippet>` and the
//! `<Tab>` / `<S-Tab>` handlers were `Editor::do_snippet_*` methods.
//! Per `feedback_mode_owns_its_surface`, the handler bodies belong to
//! the mode that owns the chords (`SnippetActiveMode`). The
//! `ActionHandlerRegistry` seam hands a handler only an
//! [`ActionContext`](lattice_mode::ActionContext) (no `&mut Editor`),
//! so the session it mutates must live somewhere both the host (which
//! still creates the session on expand — SN.3 moves that too) and the
//! mode handler can reach: this service, registered in the
//! `ServiceRegistry` under [`SnippetSessionHandle`].
//!
//! One `Mutex` guards the `Option<ActiveSnippet>`. Snippet operations
//! are rare (per `<Tab>`, never per render), so the lock is
//! uncontended and off any hot path.

use std::sync::{Arc, Mutex};

use crate::active::ActiveSnippet;

/// The live snippet session (`None` when no snippet is expanding).
#[derive(Default)]
pub struct SnippetSession {
    inner: Mutex<Option<ActiveSnippet>>,
}

impl std::fmt::Debug for SnippetSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock` so a `{:?}` on the owning `Editor` can never
        // deadlock against a concurrent navigation lock.
        let active = self.inner.try_lock().map(|g| g.is_some());
        f.debug_struct("SnippetSession")
            .field("active", &active)
            .finish()
    }
}

/// Shared handle, registered in `ServiceRegistry` and held by the
/// host. Per `feedback_servicesregistry_arc_typeid`: register and
/// look up under this exact alias so the `TypeId` matches.
pub type SnippetSessionHandle = Arc<SnippetSession>;

impl SnippetSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` while a snippet is expanding. Backs the host's many
    /// `active_snippet.is_some()` readers (keymap overlay sync,
    /// render state, …).
    pub fn is_active(&self) -> bool {
        self.lock().is_some()
    }

    /// Install a freshly-expanded session, replacing any prior one.
    pub fn set(&self, active: ActiveSnippet) {
        *self.lock() = Some(active);
    }

    /// End the session (reached `$0`, or the buffer/mode tore down).
    pub fn clear(&self) {
        *self.lock() = None;
    }

    /// Mutate the live session in place. The closure sees the whole
    /// `Option` so it can advance the focused tabstop *and* end the
    /// session (set it to `None`) in one critical section.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Option<ActiveSnippet>) -> R) -> R {
        f(&mut self.lock())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ActiveSnippet>> {
        self.inner.lock().expect("SnippetSession mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn empty_session_is_inactive() {
        let s = SnippetSession::new();
        assert!(!s.is_active());
        s.clear();
        assert!(!s.is_active());
    }
}
