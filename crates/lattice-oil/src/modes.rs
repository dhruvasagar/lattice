//! `oil-mode` -- major mode for the oil.nvim-style editable
//! directory listing buffer.
//!
//! Lives here (rather than in `lattice-mode`) per the
//! mode-architecture convention: "a mode lives with the crate
//! that owns its associated feature." `OilBuffer` lives in
//! this crate, so the mode + the oil-mode-owned
//! [`BufferLocal`] state ([`OilDir`]) live here too.
//!
//! The mode itself is metadata only: kind = Major, no
//! contributed options (oil is writable so it
//! contributes no `ReadOnly` override), no capability
//! requirements, no-op lifecycle hooks. Behaviour --
//! navigation, the rope-vs-snapshot diff, the
//! filesystem-op planner -- lives on `OilBuffer` and the
//! consumer's App surface.

use std::path::PathBuf;

use lattice_mode::{
    BufferLocal, CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};

/// Major mode for oil-style directory-listing buffers. Any
/// buffer whose major is `oil-mode` is an `OilBuffer`; the
/// renderer dispatches accordingly.
pub struct OilMode;

impl OilMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("oil-mode")
    }
}

impl Mode for OilMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
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

/// `BufferLocal` carrying the filesystem path the oil
/// buffer's listing represents (M.3.2.c.3 mirror of
/// `OilBuffer::dir`). Renderers / writers read through this
/// rather than poking `OilBuffer::dir` directly so the canonical
/// "what directory does this oil buffer represent" lookup is
/// uniform with the rest of the mode-owned per-buffer state.
#[derive(Debug, Clone)]
pub struct OilDir(pub PathBuf);

impl BufferLocal for OilDir {
    const NAME: &'static str = "oil-mode.dir";
    const DOC: &'static str =
        "Directory the oil buffer's editable listing represents. \
         Diff-on-:write applies filesystem ops relative to this \
         path; status line shows it.";
    const OWNER_MODE: &'static str = "oil-mode";
    fn describe(&self) -> String {
        self.0.display().to_string()
    }
}

/// Register every `lattice-oil`-owned mode against `registry`.
/// Called from the App's boot path alongside
/// `lattice_mode::register_foundation_modes`,
/// `lattice_syntax::register_language_modes`, and
/// `lattice_lsp::register_lsp_log_modes`. Mirrors the same
/// per-feature-crate registration pattern.
pub fn register_oil_modes(registry: &mut ModeRegistry) {
    registry
        .register(OilMode)
        .expect("oil-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oil_mode_id_and_kind() {
        assert_eq!(OilMode.id(), OilMode::mode_id());
        assert_eq!(OilMode::mode_id().as_str(), "oil-mode");
        assert_eq!(OilMode.kind(), ModeKind::Major);
    }

    #[test]
    fn oil_dir_buffer_local_owner_mode_is_oil_mode() {
        assert_eq!(<OilDir as BufferLocal>::OWNER_MODE, "oil-mode");
        assert_eq!(<OilDir as BufferLocal>::NAME, "oil-mode.dir");
        let d = OilDir(PathBuf::from("/tmp/x"));
        assert_eq!(d.describe(), "/tmp/x");
    }

    #[test]
    fn register_oil_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_oil_modes(&mut registry);
        assert!(registry.is_registered(OilMode::mode_id()));
    }
}
