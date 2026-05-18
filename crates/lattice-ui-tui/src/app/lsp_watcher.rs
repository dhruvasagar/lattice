//! 4.4.l.2 / 5.8.AA.o -- file-watcher service backing
//! `workspace/didChangeWatchedFiles` lives in the host crate
//! (`lattice_host::lsp_watcher`) so both renderer peers reach it
//! uniformly via `Editor::run_tick_pending`. This module is now
//! just thin App-side delegates kept so the rest of the TUI's
//! call sites don't need to change.

use crate::app::App;

impl App {
    /// 4.4.l.2: ensure the file-watcher service is alive and its
    /// per-server subscription cache reflects the current
    /// dynamic registry. Delegates to the host (5.8.AA.o).
    pub fn refresh_lsp_file_watcher(&mut self) {
        self.editor.refresh_lsp_file_watcher();
    }

    /// 4.4.l.2: drain queued fs events and fan out per-server
    /// `workspace/didChangeWatchedFiles` notifications.
    /// Delegates to the host (5.8.AA.o).
    pub fn drain_lsp_fs_events(&mut self) {
        self.editor.drain_lsp_fs_events();
    }
}
