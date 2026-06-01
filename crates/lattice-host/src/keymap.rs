//! Re-export shim for the static keymap catalog + `keymap_entry!` macro.
//!
//! K.2.4.A.0.1 (2026-06-02) relocated the canonical definitions of
//! `KeymapEntry` / `default_keymap` / `lookup` / `entries` and the
//! `keymap_entry!` macro into `lattice-mode::keymap_entry`, so mode
//! crates contributing keymaps via `Mode::keymap()` can declare
//! bindings without depending on `lattice-host`. The matcher engine
//! (`KeymapTrie` / `KeymapLayer` / `BoundCommand`) still lives in
//! host; only the static catalog + entry-row metadata moved.
//!
//! This shim preserves every `lattice_host::keymap::{...}` import
//! path used by:
//!
//! - `dispatch.rs` — `:describe-key` static-catalog hits via
//!   `crate::keymap::lookup(chord)`; `:keymap` listing via
//!   `crate::keymap::{BindingMode, entries}`.
//! - `lattice-ui-tui::keymap` — re-export chain feeding the TUI
//!   drift-test catalog walker in `input.rs` and the app-glue
//!   reference in `app.rs`.
//! - The various `lattice-host::keymap_normal` / `keymap_visual` /
//!   `keymap_insert` / `keymap_replace` modules that import
//!   `BindingMode` via `crate::keymap::BindingMode`.
//!
//! Plan to retire the shim once downstream imports flip to
//! `lattice_mode::{KeymapEntry, keymap_entry, ...}` directly.

// `BindingMode` already moved to `lattice-mode` in K.2.2; the
// re-export at this path has lived here since.
pub use lattice_mode::BindingMode;

// K.2.4.A.0.1: static catalog + entry-row metadata.
pub use lattice_mode::keymap_entry::{
    KeymapEntry, __builtin_source, default_keymap, entries, lookup,
};

// `#[macro_export]`'d macros land at the defining crate's root. The
// host had `pub use lattice_host::keymap_entry;` consumers (notably
// `lattice-ui-tui::lib.rs:51`); re-export through the host's root so
// those paths still resolve. The re-export lives at the lib root
// (see `lattice-host/src/lib.rs`), not here, since macros can't be
// re-exported from a non-root module.
