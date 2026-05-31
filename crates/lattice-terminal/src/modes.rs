use std::sync::Arc;

use lattice_config::OptionOverrideSet;
use lattice_core::{BufferId, BufferKind};
use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};

use crate::synthetic::TerminalStoreHandle;

pub struct TerminalMode;

impl TerminalMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("terminal-mode")
    }
}

impl Mode for TerminalMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    /// H.2: terminal buffers (`BufferKind::Terminal`) dispatch to
    /// this major via the registry's kind index.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Terminal)
    }
    fn options(&self) -> OptionOverrideSet {
        // Terminal buffers are PTY-backed cell grids, not
        // on-disk files: `:q` must not warn about unsaved
        // changes, `:w` is a no-op. Mutation flows through the
        // PTY stdin path (T2), not the rope-operator path; we
        // flag read-only here so the dispatcher rejects naive
        // text inserts in Normal-in-terminal until T2's encoder
        // gate is in place.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// 2026-05-26: claim invocation dispatch for terminal panes.
    /// The host's runner registry maps `terminal-mode` to
    /// `Editor::run_terminal_invocation`; `Editor::run_invocation`
    /// looks the runner up via this hook instead of branching
    /// on `BufferKind::Terminal`.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(Self::mode_id())
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Terminal-mode T2.a (2026-05-25): the minor mode that, when
/// active on a Terminal buffer, switches the translate layer
/// from vim-grammar-over-scrollback to keystroke-encoded-PTY-input.
///
/// Conceptually analogous to Insert mode but scoped per buffer
/// (a minor) rather than globally (a `ModalState` variant): the
/// editor's modal state stays `Normal` underneath, and pane
/// switches automatically pick up the destination buffer's mode
/// set — no implicit auto-Esc handshake when leaving the
/// terminal pane mid-Insert.
///
/// Entry chord: `i` (Normal-in-terminal). Exit chord:
/// `<C-\><C-n>`. T2.b adds `a` / `I` / `A` entry variants and
/// the optional `<Esc>` exit gated by `terminal.esc_exits`.
pub struct TerminalInsertMode;

impl TerminalInsertMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("terminal-insert-mode")
    }
}

impl Mode for TerminalInsertMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // No option contributions — the mode is a pure
        // translate-layer discriminator. Read-only / NoFile
        // already come from the underlying terminal-mode major.
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// T-mode-1 (2026-05-27): the minor mode that runs the central
/// vim grammar against a synthetic, read-only Document built from
/// the terminal's scrollback. Mutually exclusive with
/// [`TerminalInsertMode`] — the host's transition path
/// (`do_enter_terminal_insert` / `do_exit_terminal_insert`)
/// flips between them.
///
/// Lifecycle:
/// - `on_activate`: pull `TerminalStoreHandle` from services,
///   call `install_synthetic(buffer_id)`. The store builds the
///   SyntheticDoc via `SharedTerm::build_normal_snapshot` and
///   stashes it on the `TerminalBuffer`.
/// - Drop the returned [`TerminalNormalModeGuard`]: call
///   `clear_synthetic(buffer_id)`. The buffer's `synthetic`
///   field goes back to `None`; PTY output resumes feeding
///   alacritty normally on Insert re-entry.
///
/// When the service is unavailable (test harness without
/// registry wiring), `on_activate` succeeds with a no-op Guard
/// — the mode is still "active" for the cascade's sake but no
/// rope build happens. Matches `LspMode`'s graceful-degradation
/// shape.
///
/// See `docs/dev/architecture/terminal-as-document.md` §3.6 for
/// the architectural framing.
pub struct TerminalNormalMode;

impl TerminalNormalMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("terminal-normal-mode")
    }
}

impl Mode for TerminalNormalMode {
    type Guard = TerminalNormalModeGuard;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // No option contributions — read-only / NoFile already
        // come from the underlying `terminal-mode` major.
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buf_id = BufferId(ctx.buffer_id().0 as u32);
            let store = ctx.service::<TerminalStoreHandle>();
            if let Some(store) = store.as_ref() {
                // Build + stash the SyntheticDoc. The store
                // returns `false` if no terminal buffer exists
                // for this id — we treat that as a graceful
                // no-op (mode still activates), matching the
                // LspMode shape.
                store.install_synthetic(buf_id);
            }
            Ok(TerminalNormalModeGuard {
                store,
                buffer_id: buf_id,
            })
        })
    }
}

/// Guard returned from [`TerminalNormalMode::on_activate`].
/// Holds a clone of the `TerminalStoreHandle` so its `Drop`
/// can clear the SyntheticDoc on the underlying buffer when the
/// mode deactivates. `store = None` when the service wasn't
/// registered at activation time (test harness) — Drop is then
/// a no-op.
pub struct TerminalNormalModeGuard {
    store: Option<Arc<TerminalStoreHandle>>,
    buffer_id: BufferId,
}

impl Drop for TerminalNormalModeGuard {
    fn drop(&mut self) {
        if let Some(store) = &self.store {
            store.clear_synthetic(self.buffer_id);
        }
    }
}

pub fn register_terminal_modes(registry: &mut ModeRegistry) {
    registry
        .register(TerminalMode)
        .expect("terminal-mode register");
    registry
        .register(TerminalInsertMode)
        .expect("terminal-insert-mode register");
    registry
        .register(TerminalNormalMode)
        .expect("terminal-normal-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_mode_id_kind() {
        assert_eq!(TerminalMode.id(), TerminalMode::mode_id());
        assert_eq!(TerminalMode::mode_id().as_str(), "terminal-mode");
        assert_eq!(TerminalMode.kind(), ModeKind::Major);
    }

    #[test]
    fn terminal_insert_mode_id_kind() {
        assert_eq!(TerminalInsertMode.id(), TerminalInsertMode::mode_id());
        assert_eq!(
            TerminalInsertMode::mode_id().as_str(),
            "terminal-insert-mode",
        );
        assert_eq!(TerminalInsertMode.kind(), ModeKind::Minor);
    }

    #[test]
    fn register_terminal_modes_populates_both() {
        let mut registry = ModeRegistry::new();
        register_terminal_modes(&mut registry);
        assert!(registry.is_registered(TerminalMode::mode_id()));
        assert!(registry.is_registered(TerminalInsertMode::mode_id()));
        // T-mode-1 (2026-05-27): the new normal-mode joins both.
        assert!(registry.is_registered(TerminalNormalMode::mode_id()));
    }

    #[test]
    fn terminal_normal_mode_id_kind() {
        assert_eq!(TerminalNormalMode.id(), TerminalNormalMode::mode_id());
        assert_eq!(
            TerminalNormalMode::mode_id().as_str(),
            "terminal-normal-mode",
        );
        assert_eq!(TerminalNormalMode.kind(), ModeKind::Minor);
    }

    /// Mock `TerminalStore` used by the lifecycle tests below.
    /// Records install / clear counts so the test can assert that
    /// `on_activate` triggered an install and that dropping the
    /// returned Guard triggered a clear.
    struct RecordingStore {
        installs: std::sync::atomic::AtomicUsize,
        clears: std::sync::atomic::AtomicUsize,
    }

    impl RecordingStore {
        fn new() -> Self {
            Self {
                installs: std::sync::atomic::AtomicUsize::new(0),
                clears: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn installs(&self) -> usize {
            self.installs.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn clears(&self) -> usize {
            self.clears.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl crate::synthetic::TerminalStore for RecordingStore {
        fn install_synthetic(&self, _id: BufferId) -> bool {
            self.installs
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }
        fn clear_synthetic(&self, _id: BufferId) -> bool {
            self.clears
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }
    }

    #[tokio::test]
    async fn on_activate_installs_synthetic_via_store() {
        let recording = Arc::new(RecordingStore::new());
        let store_dyn: Arc<dyn crate::synthetic::TerminalStore> = recording.clone();
        let handle = crate::synthetic::TerminalStoreHandle::new(store_dyn);

        let mut services = lattice_mode::ServiceRegistry::new();
        services.register(handle);
        let services = Arc::new(services);

        let ctx = ModeContext::new(
            lattice_protocol::ids::BufferId::new(7),
            TerminalNormalMode::mode_id(),
            Arc::new(lattice_config::ConfigRegistry::new()),
            Arc::new(lattice_runtime::EventBus::new()),
            services,
        );

        let _guard = TerminalNormalMode
            .on_activate(ctx)
            .await
            .expect("activate ok");
        assert_eq!(recording.installs(), 1, "install_synthetic should fire once on activate");
        assert_eq!(recording.clears(), 0, "no clear before guard drop");
        // Guard dropped at end of scope below.
        drop(_guard);
        assert_eq!(recording.clears(), 1, "clear_synthetic should fire on guard drop");
    }

    #[tokio::test]
    async fn on_activate_succeeds_with_no_store_service() {
        // No TerminalStoreHandle registered — mode should still
        // activate gracefully (Guard's Drop is a no-op). Same
        // shape as LspMode's graceful-degradation path.
        let services = Arc::new(lattice_mode::ServiceRegistry::new());
        let ctx = ModeContext::new(
            lattice_protocol::ids::BufferId::new(9),
            TerminalNormalMode::mode_id(),
            Arc::new(lattice_config::ConfigRegistry::new()),
            Arc::new(lattice_runtime::EventBus::new()),
            services,
        );
        let guard = TerminalNormalMode
            .on_activate(ctx)
            .await
            .expect("activate ok without store");
        drop(guard); // should not panic
    }
}
