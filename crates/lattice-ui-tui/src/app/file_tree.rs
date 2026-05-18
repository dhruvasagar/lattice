//! File-tree App surface -- thin delegates over the host's
//! file-tree methods. Phase 5.8.AD.1 migrated every body to
//! `lattice_host::dispatch::Editor::do_open_file_tree` etc., so
//! this file is just the renderer-coupled fan-out for the few
//! sites that need to hop through `handle_renderer_signal`.

use std::path::{Path, PathBuf};

use super::App;
use lattice_file_tree::FileTreeEntry;

impl App {
    /// Delegate to [`lattice_host::dispatch::Editor::set_file_tree_root`].
    pub(super) fn set_file_tree_root(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        root: PathBuf,
    ) {
        self.editor.set_file_tree_root(buffer_id, root);
    }

    /// Delegate to [`lattice_host::dispatch::Editor::set_file_tree_entries`].
    pub(super) fn set_file_tree_entries(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        entries: Vec<FileTreeEntry>,
    ) {
        self.editor.set_file_tree_entries(buffer_id, entries);
    }

    /// Delegate to [`lattice_host::dispatch::Editor::set_file_tree_nerd_fonts`].
    pub(super) fn set_file_tree_nerd_fonts(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        nerd_fonts: bool,
    ) {
        self.editor.set_file_tree_nerd_fonts(buffer_id, nerd_fonts);
    }

    pub(super) fn file_tree_root_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<PathBuf> {
        self.editor.file_tree_root_for(buffer_id)
    }

    pub(super) fn file_tree_entries_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<Vec<FileTreeEntry>> {
        self.editor.file_tree_entries_for(buffer_id)
    }

    pub(super) fn file_tree_nerd_fonts_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<bool> {
        self.editor.file_tree_nerd_fonts_for(buffer_id)
    }

    pub(super) fn file_tree_with_root(&self, root: &Path) -> Option<crate::buffers::BufferId> {
        self.editor.file_tree_with_root(root)
    }

    /// `:Tree [path]`. Phase 5.8.AD.1: body migrated to
    /// [`lattice_host::dispatch::Editor::do_open_file_tree`].
    pub(super) fn do_open_file_tree(&mut self, root: Option<PathBuf>) {
        let signals = self.editor.do_open_file_tree(root);
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// `:TreeClose` (5.5.G.12 + 5.8.AD.1 alignment).
    pub(super) fn dismiss_file_tree(&mut self) {
        let signals = self.editor.dismiss_file_tree();
        for signal in signals {
            self.handle_renderer_signal(signal);
        }
    }

    /// `<CR>` on a tree row. Phase 5.8.AD.1: body migrated to
    /// [`lattice_host::dispatch::Editor::do_file_tree_follow`].
    pub(super) fn do_file_tree_follow(&mut self) {
        let signals = self.editor.do_file_tree_follow();
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }
}
