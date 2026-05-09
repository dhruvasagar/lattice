//! Help-buffer App surface -- opening / dedup-by-title /
//! activation in a pane, link-follow, anchor-scroll, the
//! `:describe-*` / `:apropos` / `:help` writers that compose
//! help bodies.
//!
//! Methods that move here in R.1:
//! - `open_help`, `open_help_in_pane`, `seed_help_locals`.
//! - `activate_help_in_pane` (or shared with lifecycle).
//! - `do_help_follow_link` (link-target dispatch).
//! - `do_describe_command`, `do_describe_buffer`,
//!   `do_describe_key`, `do_describe_option`,
//!   `do_describe_event`, `do_apropos`, `do_help`,
//!   `do_keymap`.
//! - `do_diagnostics` (the diagnostics buffer view).
//! - `do_buffers_listing` (the `:ls` / `:buffers` echo).
//! - `do_options` (the `:options` listing).
//!
//! What does NOT live here: `HelpBuffer` itself
//! (`crate::help::HelpBuffer`), the markdown parser, link
//! extraction, anchor generation -- those are content-shape
//! concerns owned by `crate::help`. This module is App's
//! *workflow* layer above that.
