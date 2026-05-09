//! In-buffer search (`/`, `?`, `n`, `N`, `*`, `#`, `g*`,
//! `g#`) and the substitute (`:s`) preview pipeline.
//!
//! Methods that move here in R.1:
//! - `do_search_forward`, `do_search_backward`,
//!   `do_search_next`, `do_search_prev`,
//!   `do_search_word_under_cursor`,
//!   `do_search_word_under_cursor_partial`.
//! - `do_substitute`, `update_substitute_preview`,
//!   `clear_substitute_preview`.
//! - `do_find_char`, `do_till_char`, `do_repeat_find`,
//!   `do_repeat_find_reverse`.
//! - `update_search_highlights`,
//!   `clear_search_highlights`, `do_nohlsearch`.
//!
//! What does NOT live here: the regex engine, the
//! incremental highlight planner -- those live in
//! `crate::search`.
