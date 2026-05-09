//! Completion popup state machine -- the in-buffer
//! completion UI (omnifunc-style triggers, accept/cancel,
//! filtering, snippet expansion entry).
//!
//! Methods that move here in R.1:
//! - `start_completion`, `cancel_completion`,
//!   `accept_completion`, `accept_completion_with`.
//! - `complete_next`, `complete_prev`,
//!   `update_completion_filter`,
//!   `refresh_completion_results`.
//! - Snippet-tabstop traversal hooks
//!   (`snippet_jump_next`, `snippet_jump_prev`,
//!   `snippet_finalize`).
//! - The buffer-local triggers that auto-pop completion on
//!   matching characters (config-driven).
//!
//! What does NOT live here: the completion provider
//! registry, source plugins, snippet parser -- those live
//! in `crate::completion` / `crate::snippet`.
