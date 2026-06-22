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
//! SN.3e: the session is keyed **by buffer**
//! (`HashMap<BufferId, ActiveSnippet>`), not a single global slot.
//! Before SN.3e a snippet started in buffer A then switching to B lit
//! `active-snippet-mode` on B and routed `<Tab>` to A's tabstops
//! against B's cursor (the predicate + slot were buffer-agnostic).
//! Snippet state is buffer-local (everything-is-a-buffer ⇒ per-buffer,
//! not a singleton), so every operation names its buffer: handlers use
//! `core_buffer_id(ctx.buffer_id)`, host paths use the document buffer.
//!
//! One `Mutex` guards the map. Snippet operations are rare (per
//! `<Tab>`, never per render), so the lock is uncontended and off any
//! hot path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;

use crate::active::ActiveSnippet;

/// The live snippet sessions, keyed by the buffer each is expanding in
/// (a buffer is absent from the map when no snippet is live there).
#[derive(Default)]
pub struct SnippetSession {
    inner: Mutex<HashMap<BufferId, ActiveSnippet>>,
}

impl std::fmt::Debug for SnippetSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock` so a `{:?}` on the owning `Editor` can never
        // deadlock against a concurrent navigation lock.
        let active = self.inner.try_lock().map(|g| g.len());
        f.debug_struct("SnippetSession")
            .field("active_buffers", &active)
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

    /// `true` while a snippet is expanding **in `buffer`**. Backs the
    /// host's many readers (keymap overlay sync, render state, …),
    /// each of which names the buffer it cares about so a session in
    /// one buffer never activates the mode in another.
    pub fn is_active(&self, buffer: BufferId) -> bool {
        self.lock().contains_key(&buffer)
    }

    /// Install a freshly-expanded session in `buffer`, replacing any
    /// prior session for that same buffer.
    pub fn set(&self, buffer: BufferId, active: ActiveSnippet) {
        self.lock().insert(buffer, active);
    }

    /// End the session in `buffer` (reached `$0`, or the buffer/mode
    /// tore down). A no-op if `buffer` had no live session.
    pub fn clear(&self, buffer: BufferId) {
        self.lock().remove(&buffer);
    }

    /// Mutate the live session for `buffer` in place. The closure sees
    /// an `Option` so it can advance the focused tabstop *and* end the
    /// session (set it to `None`) in one critical section — setting it
    /// to `None` removes the buffer's entry from the map.
    pub fn with_mut<R>(&self, buffer: BufferId, f: impl FnOnce(&mut Option<ActiveSnippet>) -> R) -> R {
        let mut map = self.lock();
        // Present the entry as an `Option` so the existing closure
        // contract (`*s = None` ends the session) is preserved exactly.
        let mut slot = map.remove(&buffer);
        let r = f(&mut slot);
        if let Some(active) = slot {
            map.insert(buffer, active);
        }
        r
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<BufferId, ActiveSnippet>> {
        self.inner.lock().expect("SnippetSession mutex poisoned")
    }
}

/// Predicate for the host's generic session-backed-minor
/// reconciler: `active-snippet-mode` should be active on the
/// active buffer exactly while a snippet session is live. The host
/// pairs this with `SnippetActiveMode::mode_id()` at boot and
/// reconciles it each overlay-sync cycle — so the host's generic
/// sync carries no `snippet_session.is_active()` literal. Keeps the
/// "when is my mode active?" policy in this crate per
/// `feedback_mode_owns_its_surface`.
pub fn snippet_active_predicate(
    session: SnippetSessionHandle,
) -> Arc<dyn Fn(BufferId) -> bool + Send + Sync> {
    Arc::new(move |buffer| session.is_active(buffer))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn buf(n: u32) -> BufferId {
        BufferId(n)
    }

    #[test]
    fn empty_session_is_inactive() {
        let s = SnippetSession::new();
        assert!(!s.is_active(buf(0)));
        s.clear(buf(0));
        assert!(!s.is_active(buf(0)));
    }

    #[test]
    fn unknown_buffer_is_inactive_never_panics() {
        let s = SnippetSession::new();
        // `is_active` / `clear` / `with_mut` on a buffer with no live
        // session are graceful no-ops (SN.3e.2 graceful contract).
        assert!(!s.is_active(buf(42)));
        s.clear(buf(42));
        let seen = s.with_mut(buf(42), |slot| slot.is_some());
        assert!(!seen);
        assert!(!s.is_active(buf(42)));
    }
}
