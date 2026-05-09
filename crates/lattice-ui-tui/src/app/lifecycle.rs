//! Buffer-creation, activation transitions, and shutdown.
//!
//! Methods that move here in R.1:
//! - `App::new` (the big constructor).
//! - `activate_document`, `activate_file_tree`,
//!   `activate_help_in_pane`, `activate_buffer_state`.
//! - `snapshot_active_pane`, `snapshot_active_document`.
//! - `do_open_oil` / `do_open_file_tree` (buffer-creation
//!   entry points -- the destination feature module owns
//!   the per-buffer-kind body, but the lifecycle hook lives
//!   here).
//! - `set_viewport_height`, `pending_redraw` handling, and
//!   other "per-loop iteration" state hooks.
//!
//! What does NOT live here: the per-feature dispatchers
//! (`do_help_follow_link`, `do_lsp_*`) -- those live in their
//! respective feature modules.
