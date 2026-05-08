//! LSP log buffer major modes.
//!
//! Three majors for the read-only buffers backing LSP
//! observability surfaces:
//!
//! - `lsp-log-mode` -- the per-server `*lsp:<server>*` log
//!   (records produced by `LspLogger::log`).
//! - `lsp-trace-log-mode` -- per-server JSON-RPC wire trace
//!   (`*lsp:<server>:trace*`); only populated when
//!   `:lsp-trace <server>` is on.
//! - `lsp-server-log-mode` -- per-server stderr feed
//!   (`*lsp:<server>:server*`).
//!
//! Pure declarations in this slice (M.3.0). Real behavior (the
//! follow-tail, the JSON-RPC syntax highlighting overlay, the
//! per-record severity decoration) lives in the existing
//! lattice-ui-tui code paths today; M.4 routes the rendering
//! through the unified mode-resolved pipeline so these modes'
//! `decorations()` impls become non-empty.

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
            fn on_activate(&self, _ctx: &ModeContext) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &ModeContext) -> Result<(), ModeActivationError> {
                Ok(())
            }
        }
    };
}

lsp_log_mode!(LspLogMode, "lsp-log-mode");
lsp_log_mode!(LspTraceLogMode, "lsp-trace-log-mode");
lsp_log_mode!(LspServerLogMode, "lsp-server-log-mode");

/// Register every LSP log major mode against `registry`.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_log_mode_has_distinct_id() {
        let ids = [
            LspLogMode::mode_id(),
            LspTraceLogMode::mode_id(),
            LspServerLogMode::mode_id(),
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
    }
}
