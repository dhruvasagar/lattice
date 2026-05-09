//! Macro recording / playback -- App surface for `q`,
//! `q{reg}`, `@{reg}`, `@@`.
//!
//! Methods that move here in R.1:
//! - `start_macro_recording`, `stop_macro_recording`,
//!   `play_macro`, `play_last_macro`.
//! - `record_invocation` -- the hot-path append called from
//!   the dispatch layer when recording is active.
//! - `register_get_macro` / `register_set_macro` (the
//!   register-bridge accessors that materialise a macro as
//!   register text).
//!
//! What does NOT live here: the dispatch layer itself, the
//! register-bank (lives in `crate::registers`), or the
//! `MacroRecording` struct (lives in `app::state`).
