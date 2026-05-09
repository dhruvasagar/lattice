//! LSP feature surface -- App methods for hover, definition,
//! references, diagnostics, completion-from-LSP, formatting,
//! rename, code actions, document/workspace symbols, log /
//! trace / server-log buffers, signature help, semantic
//! tokens (post-1.0), inlay hints (post-1.0), and the
//! event-bus drains for inbound LSP traffic
//! (`drain_inbound_apply_edits`,
//! `drain_inbound_configuration_requests`).
//!
//! Methods that move here in R.1:
//! - `do_lsp_hover_request`, `do_lsp_nav_request`,
//!   `do_lsp_references_request`,
//!   `do_lsp_signature_help_request`,
//!   `do_lsp_completion_request`,
//!   `do_lsp_insert_completion_request`,
//!   `do_lsp_document_symbol_request`,
//!   `do_lsp_workspace_symbol_request`,
//!   `do_lsp_format`, `do_lsp_format_range`,
//!   `do_lsp_rename_request`, `do_lsp_code_action_request`.
//! - LSP buffer openers: `do_lsp_log_listing`,
//!   `do_lsp_status`, `do_lsp_server_log_listing`,
//!   `do_lsp_log_clear`, `do_lsp_restart`,
//!   `do_set_lsp_log_level`, `lsp_close_buffer`,
//!   `lsp_flush`, `apply_persistent_lsp_editor_options`.
//! - Event-bus drains for LSP-initiated traffic.
//! - LSP completion meta + completion-result helpers.
//!
//! What does NOT live here: the LSP wire layer / actor /
//! supervisor (those live in `lattice-lsp`). This module is
//! about App's *consumption* of that layer.
