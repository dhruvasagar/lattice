//! Motion grammar -- the `Motion`-evaluating side of the
//! vim grammar. Each `do_motion_*` method evaluates a
//! motion to a `MotionResult` (target position, exclusive /
//! inclusive, linewise / charwise / blockwise).
//!
//! Methods that move here in R.1:
//! - Character-level: `do_motion_char_left`,
//!   `do_motion_char_right`, `do_motion_line_up`,
//!   `do_motion_line_down`.
//! - Word-level: `do_motion_word_forward`,
//!   `do_motion_word_backward`, `do_motion_word_end`,
//!   `do_motion_word_end_back`, `do_motion_WORD_*`.
//! - Line-level: `do_motion_line_start`,
//!   `do_motion_line_end`, `do_motion_first_nonblank`,
//!   `do_motion_column`.
//! - File-level: `do_motion_first_line`,
//!   `do_motion_last_line`, `do_motion_goto_line`,
//!   `do_motion_percent`.
//! - Paragraph / sentence / section / brace / paren / fold
//!   motions.
//! - Search-result-as-motion (`n`, `N`, `*`, `#` when used
//!   as motion arguments to operators).
//!
//! Text objects live in `operators.rs` / a dedicated
//! `text_objects.rs` if it grows; for R.1 they stay
//! adjacent to operators.
//!
//! What does NOT live here: the motion *grammar* (rules
//! and parser) -- that lives in `lattice-grammar`. This
//! module evaluates already-parsed `Motion` values.
