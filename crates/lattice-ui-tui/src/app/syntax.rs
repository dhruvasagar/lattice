//! Syntax / tree-sitter App surface -- (re)parse triggers,
//! version-tracking, the App-side glue that keeps
//! `DocumentEntry.syntax` in sync with the rope.
//!
//! Methods that move here in R.1:
//! - `ensure_syntax_for_active`,
//!   `reparse_active_if_dirty`, `sync_syntax_versions`.
//! - Major-mode-driven syntax selection
//!   (`syntax_handle_for_language`, `attach_syntax_to`).
//! - The drains / hooks that respond to syntax-load
//!   completion events.
//!
//! What does NOT live here: tree-sitter parser cache
//! (`crate::syntax`), grammar registration -- those are
//! content-shape concerns.
