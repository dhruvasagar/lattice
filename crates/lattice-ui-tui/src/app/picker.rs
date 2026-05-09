//! Picker / fuzzy-finder App surface -- file picker, buffer
//! picker, command picker, document-symbol picker,
//! workspace-symbol picker, references picker.
//!
//! Methods that move here in R.1:
//! - `open_file_picker`, `open_buffer_picker`,
//!   `open_command_picker`, `open_help_picker`,
//!   `open_document_symbols_picker`,
//!   `open_workspace_symbols_picker`,
//!   `open_references_picker`.
//! - `picker_next`, `picker_prev`, `picker_accept`,
//!   `picker_cancel`, `picker_filter_changed`.
//! - The async drain that delivers picker entries from the
//!   indexing actor.
//!
//! What does NOT live here: matcher implementation
//! (FzfV2-style), index actor, file-walk worker -- those
//! are owned by `crate::picker`.
