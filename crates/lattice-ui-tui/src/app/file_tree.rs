//! File-tree buffer App surface.
//!
//! Methods that move here in R.1:
//! - `do_open_file_tree` (the `:Tree` entry point).
//! - `do_close_file_tree` (`:TreeClose`).
//! - `do_file_tree_follow` (`<CR>` on a row).
//! - `seed_file_tree_locals` (M.3.2.c.2 mirror at creation).
//! - File-tree-specific motion helpers (if any).
//!
//! What does NOT live here: `FileTreeBuffer` itself
//! (`crate::file_tree::FileTreeBuffer`), the directory
//! scanner, the per-row icon picker -- those live in
//! `crate::file_tree`.
