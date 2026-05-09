//! Code folding -- the App-side surface above
//! `crate::folds`.
//!
//! Methods that move here in R.1:
//! - `do_fold_open`, `do_fold_close`, `do_fold_toggle`,
//!   `do_fold_open_all`, `do_fold_close_all`,
//!   `do_fold_open_recursive`, `do_fold_close_recursive`,
//!   `do_fold_define`, `do_fold_delete`,
//!   `do_fold_delete_all`.
//! - Fold-aware motion helpers used only by the App
//!   (`fold_at_cursor`, `fold_containing_line`, etc., if
//!   present).
//! - Re-mirror logic that keeps `App.folds` in sync with
//!   `DocumentEntry.folds` (until M.3.2.c.4.a removes the
//!   parallel field).
//!
//! What does NOT live here: the fold algorithm itself, the
//! `Fold` struct (lives in `app::state`), tree-sitter
//! fold-query plumbing -- those are content-shape concerns.
