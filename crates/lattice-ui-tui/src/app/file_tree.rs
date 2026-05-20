//! File-tree App surface -- thin delegates over the host's
//! file-tree methods. Phase 5.8.AD.1 migrated every body to
//! `lattice_host::dispatch::Editor::do_open_file_tree` etc., so
//! this file is just the renderer-coupled fan-out for the few
//! sites that need to hop through `handle_renderer_signal`.
//!
//! Phase 5.8.AF.5 / Slice 3c.final.E.2: mutating delegates route
//! through `mutate_editor` / `mutate_editor_with` so the swap to
//! actor-owned Editor is a one-line change in those helpers. The
//! 4 read-only `&self` accessors (`file_tree_root_for`,
//! `file_tree_entries_for`, `file_tree_nerd_fonts_for`,
//! `file_tree_with_root`) stay on `self.editor` pre-swap; the
//! final swap routes them through actor read-side accessors.

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
        self.mutate_editor(move |e| e.set_file_tree_root(buffer_id, root));
    }

    /// Delegate to [`lattice_host::dispatch::Editor::set_file_tree_entries`].
    pub(super) fn set_file_tree_entries(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        entries: Vec<FileTreeEntry>,
    ) {
        self.mutate_editor(move |e| e.set_file_tree_entries(buffer_id, entries));
    }

    /// Delegate to [`lattice_host::dispatch::Editor::set_file_tree_nerd_fonts`].
    pub(super) fn set_file_tree_nerd_fonts(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        nerd_fonts: bool,
    ) {
        self.mutate_editor(move |e| e.set_file_tree_nerd_fonts(buffer_id, nerd_fonts));
    }

    pub(super) fn file_tree_root_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<PathBuf> {
        self.read_editor(move |e| e.file_tree_root_for(buffer_id))
    }

    pub(super) fn file_tree_entries_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<Vec<FileTreeEntry>> {
        self.read_editor(move |e| e.file_tree_entries_for(buffer_id))
    }

    pub(super) fn file_tree_nerd_fonts_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<bool> {
        self.read_editor(move |e| e.file_tree_nerd_fonts_for(buffer_id))
    }

    pub(super) fn file_tree_with_root(&self, root: &Path) -> Option<crate::buffers::BufferId> {
        // Slice 3c.final.E.5e: clone `root` to owned `PathBuf` so
        // the closure satisfies `Send + 'static`. `BufferId` is Copy.
        let root = root.to_path_buf();
        self.read_editor(move |e| e.file_tree_with_root(&root))
    }

    /// `:Tree [path]`. Phase 5.8.AD.1: body migrated to
    /// [`lattice_host::dispatch::Editor::do_open_file_tree`].
    pub(super) fn do_open_file_tree(&mut self, root: Option<PathBuf>) {
        let signals = self.mutate_editor_with(move |e| e.do_open_file_tree(root));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// `:TreeClose` (5.5.G.12 + 5.8.AD.1 alignment).
    pub(super) fn dismiss_file_tree(&mut self) {
        let signals = self.mutate_editor_with(|e| e.dismiss_file_tree());
        for signal in signals {
            self.handle_renderer_signal(signal);
        }
    }

    /// `<CR>` on a tree row. Phase 5.8.AD.1: body migrated to
    /// [`lattice_host::dispatch::Editor::do_file_tree_follow`].
    pub(super) fn do_file_tree_follow(&mut self) {
        let signals = self.mutate_editor_with(|e| e.do_file_tree_follow());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }
}
