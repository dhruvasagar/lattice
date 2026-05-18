//! 4.4.l.2 / 5.8.AA.o / 5.8.AF.5 -- file-watcher service backing
//! `workspace/didChangeWatchedFiles` lives entirely on a tokio
//! task on the LSP runtime (see
//! `lattice_host::lsp_watcher::spawn_lsp_file_watcher_task`).
//! Editor sends fingerprint-gated `SyncSubscriptions` commands
//! via the `LspFileWatcherHandle::sync` cmd-tx; the watcher
//! itself + the per-event fan-out never run on the renderer's
//! per-tick loop (paramount goal #4).
//!
//! The `App::refresh_lsp_file_watcher` + `App::drain_lsp_fs_events`
//! delegates that lived here are gone: the only call site was
//! `run_tick_pending` (host-side), which now reaches
//! `Editor::refresh_lsp_file_watcher` directly. Module file kept
//! so the `mod lsp_watcher;` declaration in `app.rs` resolves
//! while we settle Slice 1 — drops in a follow-up cleanup.
