//! Ex command line (`:`) state machine -- the App-side
//! glue between keypress in CommandLine mode and the
//! parser/executor in `crate::cmdline`.
//!
//! Methods that move here in R.1:
//! - `enter_command_line`, `cancel_command_line`,
//!   `accept_command_line`.
//! - `cmdline_history_prev`, `cmdline_history_next`,
//!   `cmdline_history_filter`.
//! - `cmdline_complete`, `cmdline_complete_next`,
//!   `cmdline_complete_prev`.
//! - The `do_*` ex-command bodies that don't fit any other
//!   feature module:
//!     `do_quit`, `do_write`, `do_edit`, `do_split`,
//!     `do_vsplit`, `do_close_pane`, `do_only`,
//!     `do_buffer_next`, `do_buffer_prev`, `do_buffer_n`,
//!     `do_buffer_delete`, `do_buffer_listing`,
//!     `do_messages`, `do_redraw`, `do_set_option`,
//!     `do_global`, `do_vglobal`, `do_normal`, `do_redir`,
//!     `do_shell`, `do_make`, `do_g_alias`, `do_v_alias`.
//! - The history persistence hooks.
//!
//! What does NOT live here: the parser
//! (`crate::cmdline::parse`), the typed `CommandRegistry`,
//! or the `SearchLine` struct (lives in `app::state`).
