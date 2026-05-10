//! LSP modes.
//!
//! - `lsp-mode` (minor; M.5.0) -- the umbrella gate. When active
//!   on a buffer, LSP traffic flows: requests are issued,
//!   diagnostics are applied, document sync runs. When inactive,
//!   every LSP entry point is a silent no-op for that buffer.
//!   Activation lifecycle (didOpen / didClose) wired in M.5.3.
//!
//! - LSP **sub-modes** (minors; M.6.0) -- one per LSP feature
//!   surface. Each is independently toggleable on top of
//!   `lsp-mode`; the umbrella is the gate, the sub-modes are
//!   per-feature switches. M.6.0 ships pure declarations +
//!   registration; M.6.1 wires capability-driven auto-activation
//!   (when the umbrella turns on, sub-modes whose capability the
//!   attached server advertises auto-activate); M.6.2/M.6.3 wire
//!   per-feature gates at the request entry points and the
//!   diagnostic / completion-source pipelines.
//!
//!   The nine sub-modes:
//!   - `lsp-completion-mode` -- LSP-driven insert-mode completion +
//!     palette `:complete` issuing.
//!   - `lsp-diagnostics-mode` -- inline + gutter diagnostic paint;
//!     `:diag-next` / `:diag-prev` navigation.
//!   - `lsp-hover-mode` -- `K` hover popup.
//!   - `lsp-signature-mode` -- auto signature help on `(` / `,`.
//!   - `lsp-format-mode` -- `:lsp-format` / `:lsp-format-range` +
//!     `textDocument/onTypeFormatting` + format-on-save.
//!   - `lsp-rename-mode` -- `:lsp-rename` + workspaceEdit apply.
//!   - `lsp-symbols-mode` -- `:lsp-symbols`,
//!     `:lsp-workspace-symbol`.
//!   - `lsp-code-action-mode` -- `:lsp-code-action`.
//!   - `lsp-nav-mode` -- go-to definition / declaration / type-def
//!     / implementation; references.
//!
//! - `lsp-log-mode` / `lsp-trace-log-mode` / `lsp-server-log-mode`
//!   (majors; M.3.0) -- the read-only buffers backing the LSP
//!   observability surfaces:
//!   - `lsp-log-mode` -- the per-server `*lsp:<server>*` log
//!     (records produced by `LspLogger::log`).
//!   - `lsp-trace-log-mode` -- per-server JSON-RPC wire trace
//!     (`*lsp:<server>:trace*`); only populated when
//!     `:lsp-trace <server>` is on.
//!   - `lsp-server-log-mode` -- per-server stderr feed
//!     (`*lsp:<server>:server*`).
//!
//! Each log major's `decorations()` impl will become non-empty
//! when M.4's renderer pipeline consumes them; today they're
//! pure declarations.

use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
    OptionOverrideSet,
};

/// All three LSP log majors are read-only buffers (records
/// stream in from the LSP subsystem; the user navigates but
/// doesn't edit). Each contributes `ReadOnly = true` via its
/// declarative options.
macro_rules! lsp_log_mode {
    ($struct_name:ident, $mode_name:literal) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Major
            }
            fn options(&self) -> OptionOverrideSet {
                lattice_config::overrides! {
                    lattice_config::ReadOnly = true,
                }
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
        }
    };
}

lsp_log_mode!(LspLogMode, "lsp-log-mode");
lsp_log_mode!(LspTraceLogMode, "lsp-trace-log-mode");
lsp_log_mode!(LspServerLogMode, "lsp-server-log-mode");

/// `lsp-mode` -- the umbrella minor that gates LSP traffic on a
/// buffer. M.5.0 ships the pure declaration; M.5.3 wires the
/// activation / deactivation lifecycle (attach / didOpen on
/// activate; detach / didClose on deactivate). Subsequent M.5
/// slices route every LSP entry point (request issuing, document
/// sync, diagnostic rendering, completion source) through the
/// gate.
///
/// Capabilities are intentionally empty: standalone-server use
/// cases (snippets, scratch buffers without a backing file) want
/// to activate `lsp-mode` on un-named buffers. The capability
/// lattice can tighten in a later slice once we have a clearer
/// per-server minimum-requirement story.
pub struct LspMode;

impl LspMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-mode")
    }
}

impl Mode for LspMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // `lsp-mode` doesn't contribute any typed options today.
        // The gate is checked via direct mode-state lookup
        // (`App::lsp_mode_enabled_for`) rather than through a
        // resolved option, since the "is this mode active?"
        // question is the gate itself, not a knob the mode
        // happens to flip.
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // M.5.3 will attach the buffer to a server here (didOpen +
        // editor `LspBufferAttached` event). M.5.0 ships a no-op so
        // the surface lands without behavioural surprises.
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // M.5.3 will detach the buffer from its server here
        // (didClose + `LspBufferDetached`). The server connection
        // itself stays up if other buffers are still attached.
        Ok(())
    }
}

/// M.6.0: declare an LSP sub-mode. Each sub-mode is a minor with
/// no contributed options, no capability requirements, and no-op
/// lifecycle hooks. The hooks stay no-op even after M.6.1 lands
/// auto-activation -- the *cascade* (umbrella → sub-modes) lives
/// on the App, not in `Mode::on_activate` (which only sees a
/// `ModeContext`, not the LSP servers + capabilities). Sub-modes
/// are pure markers; the gating logic lives at the request
/// entry points and the publish-diagnostics / completion-source
/// sites that consult `App::<feature>_mode_enabled_for`.
macro_rules! lsp_sub_mode {
    ($struct_name:ident, $mode_name:literal) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn options(&self) -> OptionOverrideSet {
                OptionOverrideSet::default()
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
        }
    };
}

lsp_sub_mode!(LspCompletionMode, "lsp-completion-mode");
lsp_sub_mode!(LspDiagnosticsMode, "lsp-diagnostics-mode");
lsp_sub_mode!(LspHoverMode, "lsp-hover-mode");
lsp_sub_mode!(LspSignatureMode, "lsp-signature-mode");
lsp_sub_mode!(LspFormatMode, "lsp-format-mode");
lsp_sub_mode!(LspRenameMode, "lsp-rename-mode");
lsp_sub_mode!(LspSymbolsMode, "lsp-symbols-mode");
lsp_sub_mode!(LspCodeActionMode, "lsp-code-action-mode");
lsp_sub_mode!(LspNavMode, "lsp-nav-mode");

/// Register every LSP mode (the three log majors, the umbrella
/// `lsp-mode` minor, and the nine M.6 sub-mode minors) against
/// `registry`. The name is kept for backwards compatibility --
/// any existing call sites continue to compile, and the function
/// is the single boot-time registration entry point for
/// everything LSP-related.
pub fn register_lsp_log_modes(registry: &mut ModeRegistry) {
    registry
        .register(LspLogMode)
        .expect("lsp-log-mode register");
    registry
        .register(LspTraceLogMode)
        .expect("lsp-trace-log-mode register");
    registry
        .register(LspServerLogMode)
        .expect("lsp-server-log-mode register");
    registry.register(LspMode).expect("lsp-mode register");
    // M.6.0: nine LSP sub-mode minors. Pure declarations today;
    // M.6.1+ wires auto-activation + per-feature gating.
    registry
        .register(LspCompletionMode)
        .expect("lsp-completion-mode register");
    registry
        .register(LspDiagnosticsMode)
        .expect("lsp-diagnostics-mode register");
    registry
        .register(LspHoverMode)
        .expect("lsp-hover-mode register");
    registry
        .register(LspSignatureMode)
        .expect("lsp-signature-mode register");
    registry
        .register(LspFormatMode)
        .expect("lsp-format-mode register");
    registry
        .register(LspRenameMode)
        .expect("lsp-rename-mode register");
    registry
        .register(LspSymbolsMode)
        .expect("lsp-symbols-mode register");
    registry
        .register(LspCodeActionMode)
        .expect("lsp-code-action-mode register");
    registry
        .register(LspNavMode)
        .expect("lsp-nav-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_lsp_mode_has_distinct_id() {
        let ids = [
            LspLogMode::mode_id(),
            LspTraceLogMode::mode_id(),
            LspServerLogMode::mode_id(),
            LspMode::mode_id(),
            // M.6.0 sub-modes.
            LspCompletionMode::mode_id(),
            LspDiagnosticsMode::mode_id(),
            LspHoverMode::mode_id(),
            LspSignatureMode::mode_id(),
            LspFormatMode::mode_id(),
            LspRenameMode::mode_id(),
            LspSymbolsMode::mode_id(),
            LspCodeActionMode::mode_id(),
            LspNavMode::mode_id(),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn register_lsp_log_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_lsp_log_modes(&mut registry);
        assert!(registry.is_registered(LspLogMode::mode_id()));
        assert!(registry.is_registered(LspTraceLogMode::mode_id()));
        assert!(registry.is_registered(LspServerLogMode::mode_id()));
        assert!(registry.is_registered(LspMode::mode_id()));
        // M.6.0 sub-modes are picked up by the same registration
        // entry point.
        assert!(registry.is_registered(LspCompletionMode::mode_id()));
        assert!(registry.is_registered(LspDiagnosticsMode::mode_id()));
        assert!(registry.is_registered(LspHoverMode::mode_id()));
        assert!(registry.is_registered(LspSignatureMode::mode_id()));
        assert!(registry.is_registered(LspFormatMode::mode_id()));
        assert!(registry.is_registered(LspRenameMode::mode_id()));
        assert!(registry.is_registered(LspSymbolsMode::mode_id()));
        assert!(registry.is_registered(LspCodeActionMode::mode_id()));
        assert!(registry.is_registered(LspNavMode::mode_id()));
    }

    #[test]
    fn each_lsp_sub_mode_is_minor_no_caps_no_options() {
        // M.6.0 invariant: every sub-mode is a pure marker.
        // Capabilities and option contributions are empty; the
        // gating logic lives on the App side at the request
        // entry points + diagnostic / completion-source sites.
        let modes: Vec<&dyn Mode> = vec![
            &LspCompletionMode,
            &LspDiagnosticsMode,
            &LspHoverMode,
            &LspSignatureMode,
            &LspFormatMode,
            &LspRenameMode,
            &LspSymbolsMode,
            &LspCodeActionMode,
            &LspNavMode,
        ];
        for m in modes {
            assert_eq!(m.kind(), ModeKind::Minor, "{} not minor", m.id());
            assert_eq!(
                m.required_capabilities(),
                CapabilitySet::empty(),
                "{} declared caps",
                m.id(),
            );
            assert!(
                m.options().iter().count() == 0,
                "{} contributed options",
                m.id(),
            );
        }
    }

    #[test]
    fn lsp_mode_is_minor_with_no_capability_requirements() {
        // M.5.0: `lsp-mode` is a minor (it overlays the buffer's
        // language major); standalone-server use cases want to
        // activate without a `BUFFER_URI` capability requirement.
        assert_eq!(LspMode.kind(), ModeKind::Minor);
        assert_eq!(LspMode.required_capabilities(), CapabilitySet::empty());
    }

    #[test]
    fn lsp_mode_activates_through_registry_as_minor() {
        // M.5.0 ships the surface; M.5.3 wires the actual
        // attach / detach + didOpen / didClose. The hooks
        // are no-ops today; activation through the registry
        // succeeds and `has_minor` reports true.
        use lattice_mode::ActiveModes;
        use lattice_mode::BufferLocals;
        use lattice_protocol::ids::BufferId;
        let mut registry = ModeRegistry::new();
        register_lsp_log_modes(&mut registry);
        let mut active = ActiveModes::new();
        let mut locals = BufferLocals::new();
        registry
            .activate_minor(
                &mut active,
                &mut locals,
                BufferId::new(1),
                LspMode::mode_id(),
                CapabilitySet::empty(),
            )
            .expect("activate lsp-mode");
        assert!(active.has_minor(LspMode::mode_id()));
    }
}
