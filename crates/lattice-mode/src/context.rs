//! `ModeContext`: the handle passed to
//! [`crate::Mode::on_activate`] / [`crate::Mode::on_deactivate`].
//!
//! Hooks own their own work. Modes that need to mutate options
//! (e.g. `lsp-folding-mode` swapping `foldmethod=lsp` on
//! activate) reach the typed-options registry through
//! [`Self::config`]. Hooks must still leave OTHER modes' state
//! alone; `BufferLocals` writes go through [`Self::set_local`]
//! which enforces the [`crate::BufferLocal::OWNER_MODE`] rule.
//!
//! Why the context owns these handles (rather than the App):
//! a mode's activation behaviour shouldn't depend on which
//! renderer hosts it. A TUI App and a future GPUI App should
//! both call `registry.activate_minor(...)` and get identical
//! side effects. So everything a mode needs to do its own work
//! lives on `ModeContext`.
//!
//! Today the context exposes:
//!
//! - The [`BufferId`] the activation is operating on.
//! - The [`ModeId`] of the current mode.
//! - A typed-map ([`crate::BufferLocals`]) of buffer-local
//!   mode-internal data (M.3.2.a, Shape A from
//!   `mode-architecture.md` §9.4).
//! - The shared typed-options registry
//!   ([`lattice_config::ConfigRegistry`]) -- modes use this to
//!   set/get options whose values are coupled to the mode's
//!   active state (Phase 1).
//! - The shared typed event bus ([`lattice_runtime::EventBus`])
//!   -- modes publish typed events on activate / deactivate
//!   (e.g. `LspMode` publishes `LspBufferAttached` /
//!   `LspBufferDetached`). Phase 2.
//!
//! The borrow lifetime `'a` ties the context to the underlying
//! borrows so the borrow checker prevents the mode from
//! holding the context past the lifecycle hook's return.

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_protocol::ids::BufferId;
use lattice_runtime::EventBus;

use crate::error::ModeActivationError;
use crate::locals::{BufferLocal, BufferLocals};
use crate::mode::ModeId;

/// Lifecycle context. Carries the current activation's
/// metadata + a borrow of the buffer's locals map +
/// references to system-wide registries the mode may need to
/// mutate.
///
/// Borrowed lifetime: a context cannot outlive the
/// references it carries. Lifecycle hooks receive this by
/// `&mut` reference and must not stash it.
pub struct ModeContext<'a> {
    buffer_id: BufferId,
    current_mode: ModeId,
    locals: &'a mut BufferLocals,
    config: &'a ConfigRegistry,
    events: &'a Arc<EventBus>,
}

impl<'a> ModeContext<'a> {
    /// Construct a new context. Crate-private because only the
    /// registry should build one (during activation /
    /// deactivation); external code reads through the trait
    /// methods on `Mode`.
    pub(crate) fn new(
        buffer_id: BufferId,
        current_mode: ModeId,
        locals: &'a mut BufferLocals,
        config: &'a ConfigRegistry,
        events: &'a Arc<EventBus>,
    ) -> Self {
        Self {
            buffer_id,
            current_mode,
            locals,
            config,
            events,
        }
    }

    /// Shared typed-options registry. Mutations propagate
    /// through the registry's `OptionChanged` event stream the
    /// same way `:set` does, so subscribers (option cache
    /// recompute, side-effect cascades like `foldmethod ⇒
    /// recompute_folds`) fire automatically.
    pub fn config(&self) -> &ConfigRegistry {
        self.config
    }

    /// Shared typed event bus. Modes publish lifecycle events
    /// (e.g. `LspBufferAttached` / `LspBufferDetached` on
    /// `LspMode::on_activate` / `on_deactivate`) via
    /// `ctx.events().publish_typed(...)`. Subscribers are
    /// wired through the App at boot; modes don't subscribe
    /// from `on_activate` (lifetime semantics don't survive
    /// the hook return).
    pub fn events(&self) -> &Arc<EventBus> {
        self.events
    }

    /// Buffer the activation is operating on.
    pub fn buffer_id(&self) -> BufferId {
        self.buffer_id
    }

    /// The mode whose lifecycle hook is currently running.
    /// Used internally for the `OWNER_MODE` check on local
    /// writes; lifecycle hooks can also call this for logging
    /// / introspection.
    pub fn current_mode(&self) -> ModeId {
        self.current_mode
    }

    /// Read a buffer-local, regardless of which mode owns it.
    /// Reads are unrestricted -- a mode can read any local
    /// any other mode populated, which lets, e.g.,
    /// `lsp-completion-mode` read `file-tree-mode`'s entries
    /// for path completion without a special handshake.
    pub fn get_local<T: BufferLocal>(&self) -> Option<&T> {
        self.locals.get::<T>()
    }

    /// Write the buffer-local of type `T`. Enforces the
    /// `OWNER_MODE` rule: the current mode's id must match
    /// `T::OWNER_MODE`, otherwise returns
    /// [`ModeActivationError::WrongOwnerMode`] without
    /// touching the map.
    pub fn set_local<T: BufferLocal>(&mut self, value: T) -> Result<(), ModeActivationError> {
        if T::OWNER_MODE != self.current_mode.as_str() {
            return Err(ModeActivationError::WrongOwnerMode {
                current: self.current_mode,
                local: T::NAME,
                owner: T::OWNER_MODE,
            });
        }
        self.locals.insert(value);
        Ok(())
    }

    /// Mutably borrow a buffer-local owned by the current
    /// mode. Same `OWNER_MODE` enforcement as [`Self::set_local`].
    /// Returns `Ok(None)` if no local of type `T` is currently
    /// stored (the caller can decide whether that's an error
    /// or just initial state).
    pub fn get_local_mut<T: BufferLocal>(
        &mut self,
    ) -> Result<Option<&mut T>, ModeActivationError> {
        if T::OWNER_MODE != self.current_mode.as_str() {
            return Err(ModeActivationError::WrongOwnerMode {
                current: self.current_mode,
                local: T::NAME,
                owner: T::OWNER_MODE,
            });
        }
        Ok(self.locals.get_mut::<T>())
    }

    /// Remove the buffer-local of type `T`, returning its
    /// owned value. Same `OWNER_MODE` enforcement; useful in
    /// `on_deactivate` to clean up.
    pub fn remove_local<T: BufferLocal>(
        &mut self,
    ) -> Result<Option<T>, ModeActivationError> {
        if T::OWNER_MODE != self.current_mode.as_str() {
            return Err(ModeActivationError::WrongOwnerMode {
                current: self.current_mode,
                local: T::NAME,
                owner: T::OWNER_MODE,
            });
        }
        Ok(self.locals.remove::<T>())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[derive(Debug)]
    struct OwnedByA(i64);
    impl BufferLocal for OwnedByA {
        const NAME: &'static str = "a.value";
        const DOC: &'static str = "Owned by mode-a";
        const OWNER_MODE: &'static str = "a-mode";
        fn describe(&self) -> String {
            format!("{}", self.0)
        }
    }

    #[derive(Debug)]
    struct OwnedByB(String);
    impl BufferLocal for OwnedByB {
        const NAME: &'static str = "b.text";
        const DOC: &'static str = "Owned by mode-b";
        const OWNER_MODE: &'static str = "b-mode";
        fn describe(&self) -> String {
            self.0.clone()
        }
    }

    fn ctx<'a>(
        mode_name: &str,
        locals: &'a mut BufferLocals,
        config: &'a ConfigRegistry,
        events: &'a Arc<EventBus>,
    ) -> ModeContext<'a> {
        ModeContext::new(
            BufferId::new(1),
            ModeId::new(mode_name),
            locals,
            config,
            events,
        )
    }

    #[test]
    fn buffer_id_and_current_mode_round_trip() {
        let mut locals = BufferLocals::new();
        let cfg = ConfigRegistry::new();
        let evt = Arc::new(EventBus::new());
        let c = ctx("a-mode", &mut locals, &cfg, &evt);
        assert_eq!(c.buffer_id(), BufferId::new(1));
        assert_eq!(c.current_mode().as_str(), "a-mode");
    }

    #[test]
    fn set_local_succeeds_when_owner_matches() {
        let mut locals = BufferLocals::new();
        let cfg = ConfigRegistry::new();
        let evt = Arc::new(EventBus::new());
        let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
        assert!(c.set_local(OwnedByA(42)).is_ok());
        assert_eq!(c.get_local::<OwnedByA>().unwrap().0, 42);
    }

    #[test]
    fn set_local_fails_when_owner_mismatch() {
        let mut locals = BufferLocals::new();
        let cfg = ConfigRegistry::new();
        let evt = Arc::new(EventBus::new());
        let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
        let err = c.set_local(OwnedByB("hi".into())).unwrap_err();
        match err {
            ModeActivationError::WrongOwnerMode {
                current,
                local,
                owner,
            } => {
                assert_eq!(current.as_str(), "a-mode");
                assert_eq!(local, "b.text");
                assert_eq!(owner, "b-mode");
            }
            other => panic!("expected WrongOwnerMode, got {other:?}"),
        }
        assert!(c.get_local::<OwnedByB>().is_none());
    }

    #[test]
    fn get_local_is_unrestricted() {
        let mut locals = BufferLocals::new();
        let cfg = ConfigRegistry::new();
        let evt = Arc::new(EventBus::new());
        // Write as a-mode...
        {
            let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
            c.set_local(OwnedByA(7)).unwrap();
        }
        // ...read as b-mode (cross-mode read OK).
        let c = ctx("b-mode", &mut locals, &cfg, &evt);
        assert_eq!(c.get_local::<OwnedByA>().unwrap().0, 7);
    }

    #[test]
    fn remove_local_owner_check() {
        let mut locals = BufferLocals::new();
        let cfg = ConfigRegistry::new();
        let evt = Arc::new(EventBus::new());
        {
            let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
            c.set_local(OwnedByA(1)).unwrap();
        }
        // Wrong owner: remove fails without removing.
        {
            let mut c = ctx("b-mode", &mut locals, &cfg, &evt);
            let err = c.remove_local::<OwnedByA>().unwrap_err();
            assert!(matches!(err, ModeActivationError::WrongOwnerMode { .. }));
        }
        assert!(locals.contains::<OwnedByA>());
        // Correct owner: remove succeeds.
        {
            let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
            let removed = c.remove_local::<OwnedByA>().unwrap();
            assert_eq!(removed.unwrap().0, 1);
        }
        assert!(!locals.contains::<OwnedByA>());
    }

    #[test]
    fn get_local_mut_owner_check() {
        let mut locals = BufferLocals::new();
        let cfg = ConfigRegistry::new();
        let evt = Arc::new(EventBus::new());
        {
            let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
            c.set_local(OwnedByA(0)).unwrap();
        }
        // Wrong owner: error.
        {
            let mut c = ctx("b-mode", &mut locals, &cfg, &evt);
            assert!(c.get_local_mut::<OwnedByA>().is_err());
        }
        // Right owner: in-place mutation.
        {
            let mut c = ctx("a-mode", &mut locals, &cfg, &evt);
            c.get_local_mut::<OwnedByA>()
                .unwrap()
                .unwrap()
                .0 = 99;
        }
        let c = ctx("a-mode", &mut locals, &cfg, &evt);
        assert_eq!(c.get_local::<OwnedByA>().unwrap().0, 99);
    }
}
