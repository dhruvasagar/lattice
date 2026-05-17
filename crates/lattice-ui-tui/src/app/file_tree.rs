//! File-tree buffer App surface.
//!
//! Methods that live here:
//! - `set_file_tree_root` / `set_file_tree_entries` /
//!   `set_file_tree_nerd_fonts` (M.3.2.c.5 chokepoints: the
//!   single write paths for the three file-tree buffer-locals).
//! - `file_tree_root_for` / `file_tree_entries_for` /
//!   `file_tree_nerd_fonts_for` (read accessors).
//! - `file_tree_with_root` (dedup lookup that walks the
//!   registry's file-tree ids and probes each one's
//!   `FileTreeRoot` -- App-side because the registry doesn't
//!   own buffer-locals).
//! - `do_open_file_tree` (`:Tree [path]` entry point).
//! - `dismiss_file_tree` (`:TreeClose`).
//! - `do_file_tree_follow` (`<CR>` on a row -- toggle a
//!   directory or `:e` a file).
//!
//! ## The state model (post-M.3.2.c.5)
//!
//! `FileTreeBuffer` carries only the rendered rope + cursor +
//! scroll. The three pieces of "where does this buffer point"
//! information live exclusively in buffer-locals:
//!
//! - `FileTreeRoot` -- the rooted directory.
//! - `FileTreeEntries` -- the flat entry list backing the
//!   rope.
//! - `FileTreeNerdFonts` -- the nerd-font toggle the rope was
//!   rendered with.
//!
//! Mutations to entries go through [`Self::set_file_tree_entries`]
//! (and the chokepoint also re-renders the rope via
//! `lattice_file_tree::render_to_buffer`). Read paths --
//! renderer, picker, follow -- read through the canonical
//! buffer-local accessors. No struct mirror, no drift class.

use std::path::{Path, PathBuf};

use lattice_protocol::position::Position;

use super::{App, BufferData, BufferEntry, BufferFlags, BufferKind, EchoLevel, PositionSource};
use crate::file_tree::{FileTreeBuffer, FileTreeEntry, FileTreeEntryKind};

impl App {
    /// Write the `FileTreeRoot` buffer-local. **Single chokepoint**
    /// for every file-tree-buffer root mutation.
    pub(super) fn set_file_tree_root(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        root: PathBuf,
    ) {
        self.editor.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(crate::modes::FileTreeRoot(root));
    }

    /// Write the `FileTreeEntries` buffer-local **and** re-render
    /// the buffer's rope from the new entries + the buffer's
    /// nerd-fonts setting. Single chokepoint for every entries
    /// mutation; callers that compute new entries (e.g. the
    /// toggle path) hand them in here.
    pub(super) fn set_file_tree_entries(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        entries: Vec<FileTreeEntry>,
    ) {
        let nerd_fonts = self.file_tree_nerd_fonts_for(buffer_id).unwrap_or(false);
        let content = crate::file_tree::render_to_buffer(&entries, nerd_fonts);
        self.editor.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(crate::modes::FileTreeEntries(entries));
        self.editor.buffers
            .with_file_tree_mut(buffer_id, |tree| tree.content = content);
    }

    /// Write the `FileTreeNerdFonts` buffer-local. Re-renders
    /// the rope so the new toggle takes visible effect.
    pub(super) fn set_file_tree_nerd_fonts(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        nerd_fonts: bool,
    ) {
        self.editor.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(crate::modes::FileTreeNerdFonts(nerd_fonts));
        if let Some(entries) = self.file_tree_entries_for(buffer_id) {
            let content = crate::file_tree::render_to_buffer(&entries, nerd_fonts);
            self.editor.buffers
                .with_file_tree_mut(buffer_id, |tree| tree.content = content);
        }
    }

    pub(super) fn file_tree_root_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<PathBuf> {
        self.editor.buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::modes::FileTreeRoot>())
            .map(|r| r.0.clone())
    }

    pub(super) fn file_tree_entries_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<Vec<FileTreeEntry>> {
        self.editor.buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::modes::FileTreeEntries>())
            .map(|e| e.0.clone())
    }

    pub(super) fn file_tree_nerd_fonts_for(
        &self,
        buffer_id: crate::buffers::BufferId,
    ) -> Option<bool> {
        self.editor.buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::modes::FileTreeNerdFonts>())
            .map(|n| n.0)
    }

    /// Find a registered file-tree buffer whose `FileTreeRoot`
    /// matches `root`. Mirrors `App::oil_with_dir` -- registry
    /// can't answer on its own (root lives in buffer-locals).
    pub(super) fn file_tree_with_root(&self, root: &Path) -> Option<crate::buffers::BufferId> {
        self.editor.buffers
            .file_tree_ids()
            .into_iter()
            .find(|&id| self.file_tree_root_for(id).as_deref() == Some(root))
    }

    /// `:Tree [path]` (DESIGN.md §5.9 buffer-as-content). Opens a
    /// `FileTreeBuffer` rooted at `path` (or the current document's
    /// parent dir / cwd if absent). De-dup: if a tree at the same
    /// root is already open, switch to it.
    pub(super) fn do_open_file_tree(&mut self, root: Option<PathBuf>) {
        let root = match root {
            Some(p) => p,
            None => match self
                .editor.document
                .path()
                .and_then(|p| p.parent().map(Into::into))
            {
                Some(parent) => parent,
                None => match std::env::current_dir() {
                    Ok(p) => p,
                    Err(e) => {
                        self.set_message(EchoLevel::Error, format!("cwd error: {e}"));
                        return;
                    }
                },
            },
        };
        if let Some(existing_id) = self.file_tree_with_root(&root) {
            self.activate_file_tree(existing_id);
            self.set_message(
                EchoLevel::Info,
                format!("tree: {} (already open)", root.display()),
            );
            return;
        }
        let nerd_fonts = self.theme.nerd_fonts;
        let (tree, entries) = match FileTreeBuffer::open(&root, nerd_fonts) {
            Ok(t) => t,
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("tree open error: {}: {e}", root.display()),
                );
                return;
            }
        };
        if matches!(self.editor.active_buffer, BufferKind::Document) {
            let cur = self.editor.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        let new_id = tree.id;
        // M.3.2.c.5: seed all three buffer-locals through the
        // chokepoint helpers. There is no struct mirror.
        self.set_file_tree_root(new_id, root.clone());
        self.set_file_tree_nerd_fonts(new_id, nerd_fonts);
        // Insert the buffer first so `set_file_tree_entries`'s
        // rope-rewrite has somewhere to land. The entries write
        // immediately overwrites the rope rendered at open time
        // with the same content -- the rendered rope from
        // `FileTreeBuffer::open` is correct; the chokepoint
        // call is here for symmetry + uniform write path.
        self.editor.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::FileTree(tree),
            name: None,
        });
        self.set_file_tree_entries(new_id, entries);
        self.activate_major_for_buffer_kind(new_id, BufferKind::FileTree);
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.editor.active_buffer = BufferKind::FileTree;
        let pane = self.editor.pane_tree.active_mut();
        pane.buffer = BufferKind::FileTree;
        pane.buffer_id = new_id;
        pane.cursor = Position::ZERO;
        pane.scroll = 0;
        self.set_message(EchoLevel::Info, format!("tree: {}", root.display()));
    }

    /// `:TreeClose` -- close the active pane's tree by swapping
    /// the active pane back to a Document buffer and dropping the
    /// tree from the registry.
    /// 5.5.G.12: body migrated to
    /// [`lattice_host::dispatch::Editor::dismiss_file_tree`]. Kept
    /// as a delegate that fans the returned `RendererSignal`s
    /// through `handle_renderer_signal` so the `Effect::CloseFileTree`
    /// apply_effect arm continues to compile.
    pub(super) fn dismiss_file_tree(&mut self) {
        let signals = self.editor.dismiss_file_tree();
        for signal in signals {
            self.handle_renderer_signal(signal);
        }
    }

    /// `<CR>` on a tree row: directory → toggle expansion; file →
    /// `:e` it. Reads entries from the canonical `FileTreeEntries`
    /// buffer-local; mutations route through
    /// [`Self::set_file_tree_entries`].
    pub(super) fn do_file_tree_follow(&mut self) {
        let active_id = self.active_pane_buffer_id();
        let idx = self.editor.cursor.line as usize;
        // Canonical read: entries live in buffer-locals.
        let Some(mut entries) = self.file_tree_entries_for(active_id) else {
            return;
        };
        let Some(entry) = entries.get(idx).cloned() else {
            return;
        };
        match entry.kind {
            FileTreeEntryKind::Directory { .. } => {
                if let Err(e) = crate::file_tree::toggle_entries_at(&mut entries, idx) {
                    self.set_message(EchoLevel::Error, format!("toggle error: {e}"));
                    return;
                }
                // Single chokepoint write: updates the
                // buffer-local AND re-renders the rope.
                self.set_file_tree_entries(active_id, entries);
            }
            FileTreeEntryKind::File => {
                let path = entry.path.clone();
                self.do_edit(Some(path), false);
            }
        }
    }
}
