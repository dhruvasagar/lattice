//! Modal-state transitions -- the buffer-level state machine
//! (Normal / Insert / Visual / Op-pending / Command /
//! Search) and the major-mode activation hooks that fire
//! on transition.
//!
//! Methods that move here in R.1:
//! - `enter_normal`, `enter_insert`, `enter_replace`,
//!   `enter_op_pending`, `enter_command`, `enter_search`.
//! - Insert variants: `enter_insert_after`,
//!   `enter_insert_line_start`,
//!   `enter_insert_line_end`,
//!   `enter_insert_open_below`,
//!   `enter_insert_open_above`.
//! - The `ModeContext` builder used by major modes during
//!   `on_activate` -- including the `setup_buffer_locals`
//!   path that seeds `BufferLocals` from struct fields.
//! - Cursor-shape recompute on mode transitions (the
//!   App-side hook; renderer reads the resulting state).
//! - `set_active_mode_for_buffer` and the `Mode` trait
//!   dispatch that calls `on_activate` / `on_deactivate`.
//!
//! What does NOT live here: the `Mode` trait itself
//! (lives in `crate::modes`), the per-major-mode impls
//! (`TextMode`, `RustMode`, ...) -- those stay in
//! `crate::modes`.
