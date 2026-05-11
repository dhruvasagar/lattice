//! Oil-buffer App surface -- opening, navigation, and the
//! per-creation buffer-locals seed.
//!
//! Methods that live here:
//! - `set_oil_dir` (M.3.2.c.5 chokepoint: the *single*
//!   write path for the `OilDir` buffer-local; every
//!   directory change goes through here so the
//!   "post-mutation re-mirror" can't be forgotten).
//! - `oil_dir_for` / `oil_with_dir` (read accessors).
//! - `do_open_oil` (`:Oil` / `:e <dir>` entry).
//! - `do_oil_follow` (`<CR>` on a row -- navigate into
//!   directory or `:e` the file).
//! - `do_oil_navigate_up` (`-`).
//!
//! Stays in app.rs (lifecycle / cmdline write):
//! - `activate_oil` (the pane-flip path for switching back
//!   to an already-registered oil buffer).
//! - The oil-arm of `do_write` (it sits inside `do_write`'s
//!   match on active_buffer; lives with do_write's home in
//!   the lifecycle slice).
//!
//! What does NOT live here: `OilBuffer` itself
//! (`crate::oil::OilBuffer`), the diff algorithm, the
//! filesystem-op planner -- those are content-shape
//! concerns owned by `crate::oil`.
//!
//! ## The dir-lookup model (post-M.3.2.c.5)
//!
//! The directory an oil buffer represents lives in the
//! [`crate::modes::OilDir`] [`lattice_mode::BufferLocal`]
//! owned by `oil-mode`. There is no struct-stored copy on
//! `OilBuffer` anymore. Every reader -- renderer, status
//! line, navigate, apply -- looks up the dir through
//! `buffer_locals[id].get::<OilDir>()` (or
//! [`Self::oil_dir_for`] which wraps it). Mutating the dir
//! goes through [`Self::set_oil_dir`]; that's the single
//! chokepoint that guarantees the buffer-local stays
//! current. Forgetting to update it is impossible -- there's
//! no second copy to drift.

use std::path::{Path, PathBuf};

use lattice_protocol::position::Position;

use super::{App, BufferData, BufferEntry, BufferFlags, BufferKind, EchoLevel, PositionSource};

impl App {
    /// Write the [`crate::modes::OilDir`] buffer-local for
    /// `buffer_id` to `dir`. **Single chokepoint** for every
    /// oil-buffer dir mutation; do not insert `OilDir`
    /// elsewhere. (M.3.2.c.5: buffer-locals are canonical
    /// per-buffer mode-owned state; no struct mirror.)
    pub(super) fn set_oil_dir(&mut self, buffer_id: crate::buffers::BufferId, dir: PathBuf) {
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(crate::modes::OilDir(dir));
    }

    /// Read the dir an oil buffer represents from its
    /// [`crate::modes::OilDir`] buffer-local. `None` if the
    /// buffer isn't registered or doesn't have the local
    /// seeded (shouldn't happen in practice -- every oil
    /// buffer's creation path calls [`Self::set_oil_dir`]).
    pub(super) fn oil_dir_for(&self, buffer_id: crate::buffers::BufferId) -> Option<PathBuf> {
        self.buffer_locals
            .get(&buffer_id)
            .and_then(|locals| locals.get::<crate::modes::OilDir>())
            .map(|d| d.0.clone())
    }

    /// Find a registered oil buffer whose `OilDir` matches
    /// `dir`. Used by `do_open_oil`'s dedup path. The
    /// registry can't answer this on its own because the
    /// dir lives in buffer-locals; we walk the registry's
    /// oil-id list and probe each buffer-local entry.
    pub(super) fn oil_with_dir(&self, dir: &Path) -> Option<crate::buffers::BufferId> {
        for id in self.buffers.oil_ids() {
            if self.oil_dir_for(id).as_deref() == Some(dir) {
                return Some(id);
            }
        }
        None
    }

    /// `:Oil [dir]` -- open an oil buffer rooted at `dir` (or the
    /// current document's parent / cwd if absent). De-dup: if a
    /// buffer at the same dir is already open, switch to it.
    pub(super) fn do_open_oil(&mut self, dir: Option<PathBuf>) {
        let dir = match dir {
            Some(p) => p,
            None => match self.document.path().and_then(|p| p.parent().map(Into::into)) {
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
        if let Some(existing_id) = self.oil_with_dir(&dir) {
            self.activate_oil(existing_id);
            self.set_message(EchoLevel::Info, format!("oil: {} (already open)", dir.display()));
            return;
        }
        let oil = match crate::oil::OilBuffer::open(&dir) {
            Ok(o) => o,
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("oil open error: {}: {e}", dir.display()));
                return;
            }
        };
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        let new_id = oil.id;
        // M.3.2.c.5: seed the OilDir buffer-local at creation
        // time through the chokepoint helper. From now on every
        // dir change for this buffer goes through `set_oil_dir`
        // as well.
        self.set_oil_dir(new_id, dir.clone());
        self.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::Oil(oil),
        });
        self.activate_major_for_buffer_kind(new_id, BufferKind::Oil);
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.active_buffer = BufferKind::Oil;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Oil;
        pane.buffer_id = new_id;
        pane.cursor = Position::ZERO;
        pane.scroll = 0;
        // Sync the App-side hot-path cursor / scroll to the
        // freshly-activated oil pane. Without this, `self.cursor`
        // carries over from the prior document buffer and oil
        // edits land at the wrong rope position.
        self.cursor = Position::ZERO;
        self.scroll = 0;
        self.set_message(EchoLevel::Info, format!("oil: {}", dir.display()));
    }

    pub(super) fn do_oil_follow(&mut self) {
        let active_id = self.active_pane_buffer_id();
        let Some(oil) = self.buffers.oil(active_id) else { return; };
        // Read the entry by the App's hot-path cursor line
        // (the cursor the user actually moves with `j` / `k`).
        let idx = self.cursor.line as usize;
        let Some(entry) = oil.snapshot_entries().get(idx).cloned() else {
            return;
        };
        // The dir lives in the OilDir buffer-local (canonical).
        let Some(dir) = self.oil_dir_for(active_id) else {
            return;
        };
        if entry.is_dir {
            let new_dir = dir.join(&entry.name);
            let reload_result = self
                .buffers
                .oil_mut(active_id)
                .map(|oil| oil.reload(&new_dir));
            match reload_result {
                Some(Err(e)) => {
                    self.set_message(EchoLevel::Error, format!("oil navigate: {e}"));
                }
                Some(Ok(_)) => {
                    // Single chokepoint write -- the buffer-
                    // local mirrors no struct field, it IS the
                    // state.
                    self.set_oil_dir(active_id, new_dir);
                    self.cursor = Position::ZERO;
                    self.scroll = 0;
                }
                None => {}
            }
        } else {
            let path = dir.join(&entry.name);
            self.do_edit(Some(path), false);
        }
    }

    /// `-` -- navigate to the parent of the current buffer's dir.
    /// In oil: compute the parent from `OilDir`, reload at parent,
    /// rewrite `OilDir`. In file-tree: open oil rooted at the
    /// parent of the entry under the cursor (or the entry itself
    /// when it's a directory). Anywhere else: open oil rooted at
    /// the parent of the active document's path.
    pub(super) fn do_oil_navigate_up(&mut self) {
        match self.active_buffer {
            BufferKind::Oil => {
                let id = self.active_pane_buffer_id();
                let Some(current_dir) = self.oil_dir_for(id) else {
                    return;
                };
                let Some(parent) = current_dir.parent().map(Path::to_path_buf) else {
                    // Already at the filesystem root; no-op.
                    return;
                };
                let reload_result = self
                    .buffers
                    .oil_mut(id)
                    .map(|oil| oil.reload(&parent));
                match reload_result {
                    Some(Err(e)) => {
                        self.set_message(EchoLevel::Error, format!("oil navigate up: {e}"));
                    }
                    Some(Ok(_)) => {
                        self.set_oil_dir(id, parent);
                        self.cursor = Position::ZERO;
                        self.scroll = 0;
                    }
                    None => {}
                }
            }
            BufferKind::FileTree => {
                let id = self.active_pane_buffer_id();
                let dir = self
                    .buffers
                    .file_tree(id)
                    .and_then(|t| t.entry_at_cursor())
                    .map(|e| {
                        if matches!(e.kind, crate::file_tree::FileTreeEntryKind::Directory { .. }) {
                            e.path.clone()
                        } else {
                            e.path.parent().unwrap_or(&e.path).to_path_buf()
                        }
                    });
                self.do_open_oil(dir);
            }
            _ => {
                let dir = self
                    .document
                    .path()
                    .and_then(|p| p.parent().map(Into::into));
                self.do_open_oil(dir);
            }
        }
    }
}
