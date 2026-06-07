//! Re-export shim — `KeymapEntry`, `default_keymap`, and friends now live
//! in `lattice-keymap`.
//!
//! K.3 (2026-06-07): canonical home moved to `lattice-keymap::keymap_entry`.
//! The `keymap_entry!` macro is re-exported at the `lattice-mode` crate root
//! (`lib.rs`) so existing callers using `lattice_mode::keymap_entry! { … }`
//! keep working without changes.
pub use lattice_keymap::{KeymapEntry, default_keymap, entries, lookup};
// __builtin_source stays in lattice_keymap::keymap_entry; the macro's
// $crate::keymap_entry::__builtin_source(…) path resolves there directly.
