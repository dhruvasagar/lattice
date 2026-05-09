//! Oil-buffer App surface -- opening, navigation, and the
//! per-creation buffer-locals seed.
//!
//! Methods that live here:
//! - `seed_oil_locals` (M.3.2.c.3 mirror at creation).
//! - `do_open_oil` (`:Oil` / `:e <dir>` entry).
//! - `do_oil_follow` (`<CR>` on a row -- navigate into
//!   directory or `:e` the file; re-mirrors the new dir
//!   into buffer-locals).
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

use lattice_protocol::position::Position;

use super::{App, BufferData, BufferEntry, BufferFlags, BufferKind, EchoLevel, PositionSource};

impl App {
    /// Mirror oil-mode-owned data from an `OilBuffer` into the
    /// buffer-locals map for `buffer_id` (M.3.2.c.3). Currently
    /// mirrors `dir` only; `snapshot` is private to `OilBuffer`
    /// and stays internal until the M.3.2.c.5 `BufferStorage`
    /// retirement decision.
    pub(super) fn seed_oil_locals(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        buffer: &crate::oil::OilBuffer,
    ) {
        let locals = self
            .buffer_locals
            .entry(buffer_id)
            .or_default();
        locals.insert(crate::modes::OilDir(buffer.dir.clone()));
    }

    /// `:Oil [dir]` -- open an oil buffer rooted at `dir` (or the
    /// current document's parent / cwd if absent). De-dup: if a
    /// buffer at the same dir is already open, switch to it.
    pub(super) fn do_open_oil(&mut self, dir: Option<std::path::PathBuf>) {
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
        if let Some(existing_id) = self.buffers.oil_with_dir(&dir) {
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
        // M.3.2.c.3: mirror oil-mode-owned data (`dir`) into
        // buffer-locals BEFORE the move.
        self.seed_oil_locals(new_id, &oil);
        self.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::Oil(oil),
        });
        // M.3.1: activate oil-mode (writable, no ReadOnly
        // override; activation is mostly a no-op for now but
        // populates active_modes so M.5+ minor-mode toggles
        // can find a target).
        self.activate_major_for_buffer_kind(new_id, BufferKind::Oil);
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.active_buffer = BufferKind::Oil;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Oil;
        pane.buffer_id = new_id;
        pane.cursor = Position::ZERO;
        pane.scroll = 0;
        self.set_message(EchoLevel::Info, format!("oil: {}", dir.display()));
    }

    pub(super) fn do_oil_follow(&mut self) {
        let active_id = self.active_pane_buffer_id();
        let Some(oil) = self.buffers.oil(active_id) else { return; };
        let Some(entry) = oil.entry_at_cursor().cloned() else { return; };
        // M.3.2.c.3: read dir from buffer-locals.
        let dir = self
            .buffer_locals
            .get(&active_id)
            .and_then(|locals| locals.get::<crate::modes::OilDir>())
            .map(|d| d.0.clone())
            .unwrap_or_else(|| oil.dir.clone());
        if entry.is_dir {
            let navigate_result = self
                .buffers
                .oil_mut(active_id)
                .map(|oil| oil.navigate_into(dir.join(&entry.name)));
            match navigate_result {
                Some(Err(e)) => {
                    self.set_message(EchoLevel::Error, format!("oil navigate: {e}"));
                }
                Some(Ok(_)) => {
                    // Re-mirror dir into buffer-locals so the
                    // canonical reader sees the new sub-directory.
                    if let Some(o) = self.buffers.oil(active_id) {
                        let new_dir = o.dir.clone();
                        if let Some(locals) = self.buffer_locals.get_mut(&active_id) {
                            locals.insert(crate::modes::OilDir(new_dir));
                        }
                    }
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
    /// In oil: defer to OilBuffer::navigate_up. In file-tree: open
    /// oil rooted at the parent of the entry under the cursor (or
    /// the entry itself when it's a directory). Anywhere else: open
    /// oil rooted at the parent of the active document's path.
    pub(super) fn do_oil_navigate_up(&mut self) {
        match self.active_buffer {
            BufferKind::Oil => {
                let id = self.active_pane_buffer_id();
                if let Some(oil) = self.buffers.oil_mut(id) {
                    if let Err(e) = oil.navigate_up() {
                        self.set_message(EchoLevel::Error, format!("oil navigate up: {e}"));
                        return;
                    }
                    self.cursor = Position::ZERO;
                    self.scroll = 0;
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
