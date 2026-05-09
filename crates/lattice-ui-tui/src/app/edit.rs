//! Insert-mode edits, undo/redo, register paste, and the
//! low-level rope-mutation helpers App calls.
//!
//! Methods that move here in R.1:
//! - `do_insert_char`, `do_insert_str`,
//!   `do_insert_newline`, `do_insert_tab`,
//!   `do_backspace`, `do_delete_forward`,
//!   `do_delete_word_back`, `do_delete_line_back`.
//! - `do_undo`, `do_redo`, `do_undo_branch_older`,
//!   `do_undo_branch_newer`.
//! - `do_paste_after`, `do_paste_before`,
//!   `do_paste_register`, `do_yank_*` register writes.
//! - `do_replace_char`, `do_join_lines`,
//!   `do_indent`, `do_outdent`, `do_format_lines`,
//!   `do_repeat_last_change` (`.`).
//! - `apply_text_edit` (the LSP-edit applier reused by
//!   substitute, formatting, code actions).
//!
//! What does NOT live here: the rope itself (ropey,
//! wrapped by `Document`), the undo tree, the register
//! store -- those are owned by `crate::document` /
//! `crate::registers`.
