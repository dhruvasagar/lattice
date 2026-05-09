//! Operator grammar -- delete, change, yank, indent, format,
//! case-toggle, etc. -- and text objects.
//!
//! Methods that move here in R.1:
//! - `do_op_delete`, `do_op_change`, `do_op_yank`,
//!   `do_op_indent`, `do_op_outdent`, `do_op_format`,
//!   `do_op_filter`, `do_op_join`, `do_op_uppercase`,
//!   `do_op_lowercase`, `do_op_swapcase`,
//!   `do_op_replace_with_register`.
//! - The driver: `apply_operator(op, motion_result,
//!   register, count)`.
//! - Text objects: `text_object_word`,
//!   `text_object_WORD`, `text_object_paragraph`,
//!   `text_object_quoted`, `text_object_paired`,
//!   `text_object_tag`, `text_object_indent`,
//!   `text_object_argument`, `text_object_function`
//!   (tree-sitter-driven), and the `inner` / `around`
//!   variants.
//!
//! What does NOT live here: the grammar parser
//! (`lattice-grammar`), the rope-edit primitives
//! (`crate::document`).
