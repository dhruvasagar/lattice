//! File-tree buffer App surface.
//!
//! Methods that live here:
//! - `do_open_file_tree` (`:Tree [path]` entry point;
//!   dedup-by-root, register, activate).
//! - `dismiss_file_tree` (`:TreeClose` -- swap active pane
//!   back to a Document, drop the tree from the registry).
//! - `do_file_tree_follow` (`<CR>` on a tree row -- toggle
//!   directory expand or `:e` the file).
//! - `seed_file_tree_locals` (M.3.2.c.2: mirror
//!   tree-mode-owned data into buffer-locals at creation
//!   time, before the buffer moves into the registry).
//!
//! What does NOT live here: `FileTreeBuffer` itself
//! (`crate::file_tree::FileTreeBuffer`), the directory
//! scanner, the per-row icon picker -- those live in
//! `crate::file_tree`. Activation flow (`activate_file_tree`)
//! lives in app.rs as part of lifecycle.

use lattice_protocol::position::Position;

use super::{
    App, BufferData, BufferEntry, BufferFlags, BufferKind, EchoLevel, PositionSource,
};
use crate::file_tree::{FileTreeBuffer, FileTreeEntryKind};

impl App {
    /// Mirror tree-mode-owned data from a `FileTreeBuffer` into the
    /// buffer-locals map for `buffer_id` (M.3.2.c.2). Called at
    /// buffer creation time before the buffer moves into the
    /// registry.
    pub(super) fn seed_file_tree_locals(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        buffer: &crate::file_tree::FileTreeBuffer,
    ) {
        let locals = self
            .buffer_locals
            .entry(buffer_id)
            .or_default();
        locals.insert(crate::modes::FileTreeRoot(buffer.root.clone()));
        locals.insert(crate::modes::FileTreeEntries(buffer.entries.clone()));
        locals.insert(crate::modes::FileTreeNerdFonts(buffer.nerd_fonts));
    }

    /// `:Tree [path]` (DESIGN.md §5.9 buffer-as-content). Opens a
    /// `FileTreeBuffer` rooted at `path` (or the current document's
    /// parent dir / cwd if absent) and inserts it into the unified
    /// buffer registry. If a tree at the same root is already open,
    /// the active pane switches to it instead of spawning a duplicate
    /// -- matching `:e FILE`'s "already open" semantics. The active
    /// pane flips to the new (or existing) tree buffer.
    pub(super) fn do_open_file_tree(&mut self, root: Option<std::path::PathBuf>) {
        let root = match root {
            Some(p) => p,
            None => match self
                .document
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
        // De-dup: if the same root is already open, just switch.
        if let Some(existing_id) = self.buffers.file_tree_with_root(&root) {
            self.activate_file_tree(existing_id);
            self.set_message(
                EchoLevel::Info,
                format!("tree: {} (already open)", root.display()),
            );
            return;
        }
        let tree = match FileTreeBuffer::open(&root, self.theme.nerd_fonts) {
            Ok(t) => t,
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("tree open error: {}: {e}", root.display()),
                );
                return;
            }
        };
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        let new_id = tree.id;
        // M.3.2.c.2: mirror file-tree-mode-owned data
        // (`root` / `entries` / `nerd_fonts`) into the
        // buffer-locals map BEFORE moving `tree` into the
        // registry. The struct fields stay populated as a
        // construction artifact / fallback (M.3.2.c.5
        // retires them).
        self.seed_file_tree_locals(new_id, &tree);
        self.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::FileTree(tree),
        });
        // M.3.1: activate file-tree-mode for this buffer so
        // its ReadOnly = true contribution lands in the
        // resolved options cache.
        self.activate_major_for_buffer_kind(new_id, BufferKind::FileTree);
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.active_buffer = BufferKind::FileTree;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::FileTree;
        pane.buffer_id = new_id;
        pane.cursor = Position::ZERO;
        pane.scroll = 0;
        self.set_message(EchoLevel::Info, format!("tree: {}", root.display()));
    }

    /// `:TreeClose` -- close the active pane's tree by swapping
    /// the active pane back to a Document buffer (the original
    /// document if available; whichever document is registered
    /// otherwise) and dropping the tree from the registry.
    pub(super) fn dismiss_file_tree(&mut self) {
        if !matches!(self.active_buffer, BufferKind::FileTree) {
            return;
        }
        let tree_id = self.active_pane_buffer_id();
        let successor = self
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap_or(self.document_buffer_id);
        self.activate_buffer(successor);
        self.buffers.remove(tree_id);
        let new_kind = self.active_buffer;
        let new_id = self.active_pane_buffer_id();
        for pane in self.pane_tree.leaves_mut() {
            if pane.buffer_id == tree_id {
                pane.buffer = new_kind;
                pane.buffer_id = new_id;
            }
        }
    }

    /// `<CR>` while the active pane shows a file-tree buffer: if
    /// the cursor is on a directory row, toggle expansion; if on
    /// a file, open it via the standard `:e FILE` path (which
    /// switches to / spawns a Document buffer in the active pane).
    pub(super) fn do_file_tree_follow(&mut self) {
        let active_id = self.active_pane_buffer_id();
        let idx = self.cursor.line as usize;
        // M.3.2.c.2: prefer entries from buffer-locals; fall
        // back to the tree's struct field. The toggle below
        // mutates the struct field in place; we re-mirror
        // afterwards to keep the locals in sync.
        let entry = {
            let from_locals = self
                .buffer_locals
                .get(&active_id)
                .and_then(|locals| locals.get::<crate::modes::FileTreeEntries>())
                .and_then(|e| e.0.get(idx).cloned());
            from_locals.or_else(|| {
                self.buffers
                    .file_tree(active_id)
                    .and_then(|t| t.entries.get(idx).cloned())
            })
        };
        let Some(entry) = entry else {
            return;
        };
        match entry.kind {
            FileTreeEntryKind::Directory { .. } => {
                let toggle_result = self
                    .buffers
                    .file_tree_mut(active_id)
                    .map(|t| t.toggle_at(idx));
                match toggle_result {
                    Some(Err(e)) => {
                        self.set_message(EchoLevel::Error, format!("toggle error: {e}"));
                    }
                    Some(Ok(_)) => {
                        if let Some(t) = self.buffers.file_tree(active_id) {
                            let entries = t.entries.clone();
                            if let Some(locals) = self.buffer_locals.get_mut(&active_id) {
                                locals.insert(crate::modes::FileTreeEntries(entries));
                            }
                        }
                    }
                    None => {}
                }
            }
            FileTreeEntryKind::File => {
                let path = entry.path.clone();
                self.do_edit(Some(path), false);
            }
        }
    }
}

