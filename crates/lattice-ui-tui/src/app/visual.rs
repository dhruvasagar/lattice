//! Visual mode (`v`, `V`, `<C-v>`) -- selection state,
//! reselect (`gv`), swap-anchor (`o`, `O`), the
//! `Range::Selection` wiring that lets visual selection
//! act as the default range arg for ex commands.
//!
//! Methods that move here in R.1:
//! - `enter_visual_char`, `enter_visual_line`,
//!   `enter_visual_block`, `exit_visual`.
//! - `visual_swap_anchor`, `visual_swap_anchor_corner`
//!   (block-mode corner toggling).
//! - `visual_extend_to`, `visual_reselect`
//!   (`gv`).
//! - `selection_range`, `selection_lines_range`,
//!   `selection_block_rect` -- the resolvers that turn the
//!   current visual span into a `Range` for execution.
//! - `LastVisual` updates on visual exit.
//!
//! What does NOT live here: the rendering of the selection
//! highlight (lives in `crate::render`).
