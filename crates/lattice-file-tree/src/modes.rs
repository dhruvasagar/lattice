//! `file-tree-mode` -- major mode for the file-tree buffer + its
//! `BufferLocal`-owned per-buffer state (root, entries,
//! nerd-fonts toggle).
//!
//! Lives here (rather than in `lattice-mode`) per the
//! mode-architecture convention: "a mode lives with the crate
//! that owns its associated feature." `FileTreeBuffer` lives
//! in this crate, so the mode + the three pieces of
//! `file-tree-mode`-owned state live here too.
//!
//! ## What the `BufferLocal`s carry
//!
//! M.3.2.c.5 made `BufferLocal`s the **single** source of
//! truth for per-buffer mode-owned state. `FileTreeBuffer`
//! itself carries only the rendered rope + cursor / scroll;
//! every piece of "what does this buffer point at"
//! information lives in the three locals declared below:
//!
//! - [`FileTreeRoot`] -- the directory the tree is rooted at.
//! - [`FileTreeEntries`] -- the flat list of visible entries
//!   (root + every expanded subdir's children). Mutated
//!   through the App-side toggle chokepoint that re-renders
//!   the rope.
//! - [`FileTreeNerdFonts`] -- whether the rendered rope
//!   embeds nerd-font glyphs.
//!
//! The App reads through these directly; there is no struct
//! mirror to drift.

use std::path::PathBuf;

use lattice_config::OptionOverrideSet;
use lattice_mode::{
    BufferLocal, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};

use crate::FileTreeEntry;

/// Major mode for file-tree buffers. Read-only contribution
/// (`ReadOnly = true`); any buffer whose major is
/// `file-tree-mode` rejects mutating operators.
pub struct FileTreeMode;

impl FileTreeMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("file-tree-mode")
    }
}

impl Mode for FileTreeMode {
    type Guard = ();
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
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Filesystem path the file-tree buffer is rooted at.
/// Single source of truth (M.3.2.c.5).
#[derive(Debug, Clone)]
pub struct FileTreeRoot(pub PathBuf);

impl BufferLocal for FileTreeRoot {
    const NAME: &'static str = "file-tree-mode.root";
    const DOC: &'static str = "Directory the file-tree buffer is rooted at -- the path \
         the user passed to `:Tree` (or the workspace root for \
         the default tree).";
    const OWNER_MODE: &'static str = "file-tree-mode";
    fn describe(&self) -> String {
        self.0.display().to_string()
    }
}

/// Flat tree-of-entries backing the file-tree buffer.
/// Each entry carries its depth + expansion state. The rendered
/// rope is derived from this list -- App-side toggle handlers
/// mutate the entries through the
/// [`crate::toggle_entries_at`] helper and re-write this local
/// + the buffer's rope as one update.
#[derive(Debug, Clone)]
pub struct FileTreeEntries(pub Vec<FileTreeEntry>);

impl BufferLocal for FileTreeEntries {
    const NAME: &'static str = "file-tree-mode.entries";
    const DOC: &'static str = "Flat list of tree entries (directories + files), each \
         carrying its depth + expansion state. The file-tree \
         renderer iterates this in order; the rope content is \
         derived from it.";
    const OWNER_MODE: &'static str = "file-tree-mode";
    fn describe(&self) -> String {
        format!("{} entries", self.0.len())
    }
}

/// Whether the file-tree renders nerd-font icon glyphs inline.
#[derive(Debug, Clone, Copy)]
pub struct FileTreeNerdFonts(pub bool);

impl BufferLocal for FileTreeNerdFonts {
    const NAME: &'static str = "file-tree-mode.nerd-fonts";
    const DOC: &'static str = "Whether the file-tree buffer's rendered rope embeds \
         nerd-font icon glyphs alongside file names.";
    const OWNER_MODE: &'static str = "file-tree-mode";
    fn describe(&self) -> String {
        if self.0 { "enabled" } else { "disabled" }.to_string()
    }
}

/// Register every `lattice-file-tree`-owned mode against
/// `registry`. Called from the App's boot path alongside
/// `lattice_mode::register_foundation_modes` etc. Mirrors
/// `lattice_oil::register_oil_modes`.
pub fn register_file_tree_modes(registry: &mut ModeRegistry) {
    registry
        .register(FileTreeMode)
        .expect("file-tree-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_tree_mode_id_kind_and_options() {
        assert_eq!(FileTreeMode.id(), FileTreeMode::mode_id());
        assert_eq!(FileTreeMode::mode_id().as_str(), "file-tree-mode");
        assert_eq!(FileTreeMode.kind(), ModeKind::Major);
        let opts = FileTreeMode.options();
        assert_eq!(opts.iter().count(), 1, "expected ReadOnly contribution");
    }

    #[test]
    fn buffer_local_metadata_owner_mode() {
        assert_eq!(<FileTreeRoot as BufferLocal>::OWNER_MODE, "file-tree-mode");
        assert_eq!(
            <FileTreeEntries as BufferLocal>::OWNER_MODE,
            "file-tree-mode"
        );
        assert_eq!(
            <FileTreeNerdFonts as BufferLocal>::OWNER_MODE,
            "file-tree-mode"
        );
        assert_eq!(FileTreeRoot(PathBuf::from("/x")).describe(), "/x");
        assert_eq!(FileTreeNerdFonts(true).describe(), "enabled");
        assert_eq!(FileTreeNerdFonts(false).describe(), "disabled");
    }

    #[test]
    fn register_file_tree_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_file_tree_modes(&mut registry);
        assert!(registry.is_registered(FileTreeMode::mode_id()));
    }
}
