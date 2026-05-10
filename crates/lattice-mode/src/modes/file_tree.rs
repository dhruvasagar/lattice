//! `file-tree-mode` -- major mode for the file-tree buffer.
//! Read-only navigation surface; the buffer's content is a
//! rendered tree-of-entries that motions / `<CR>` / `o` operate
//! on. Filesystem ops (expand, collapse, file open) live in the
//! file-tree owner crate; this mode is the metadata + read-only
//! contribution.

use lattice_config::OptionOverrideSet;

use crate::{CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind};

pub struct FileTreeMode;

impl FileTreeMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("file-tree-mode")
    }
}

impl Mode for FileTreeMode {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(FileTreeMode.id(), FileTreeMode::mode_id());
        assert_eq!(FileTreeMode::mode_id().as_str(), "file-tree-mode");
        assert_eq!(FileTreeMode.kind(), ModeKind::Major);
    }

    #[test]
    fn contributes_read_only() {
        let opts = FileTreeMode.options();
        assert!(!opts.is_empty());
    }
}
