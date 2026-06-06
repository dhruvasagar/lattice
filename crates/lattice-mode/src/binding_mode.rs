//! Re-export shim — `BindingMode` now lives in `lattice-keymap`.
//!
//! K.3 (2026-06-07): canonical home moved to `lattice-keymap::BindingMode`.
//! All existing imports through `lattice_mode::BindingMode` continue to work.
pub use lattice_keymap::BindingMode;
