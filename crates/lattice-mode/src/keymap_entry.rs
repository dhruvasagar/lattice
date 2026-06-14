//! Re-export shim — `KeymapEntry`, `default_keymap`, and friends now live
//! in `lattice-keymap`.
//!
//! K.3 (2026-06-07): canonical home moved to `lattice-keymap::keymap_entry`.
//! The `keymap_entry!` macro stays here so `$crate = lattice_mode` paths
//! work for callers in `lattice-multibuffer`, `lattice-host`, and
//! `lattice-ui-tui` without those crates needing `lattice-keymap` as a
//! direct dep. `pub use lattice_keymap::keymap_entry` conflicts with
//! `pub mod keymap_entry` in the module namespace, so the macro body is
//! duplicated here (using `$crate`-relative paths through the re-exports).

pub use lattice_keymap::{KeymapEntry, default_keymap, entries, lookup};

/// Re-export `__builtin_source` from `lattice-keymap` so the `keymap_entry!`
/// macro below can reference it as `$crate::keymap_entry::__builtin_source`.
#[doc(hidden)]
pub use lattice_keymap::keymap_entry::__builtin_source;

/// Construct a [`KeymapEntry`] with the row's source location captured
/// at the macro invocation site. Three forms:
///
/// - `keymap_entry! { mode: Normal, chord: "j", doc: "...", cmd: "motion:line-down" }`
/// - `keymap_entry! { mode: Help, chord: "j", doc: "..." }` (no cmd)
/// - `keymap_entry! { mode: Normal, chord: "x", doc: "...", cmd: Some("plugin:foo") }` (explicit Option)
///
/// This is a forwarding shim: the macro body lives in `lattice-mode` so
/// `$crate = lattice_mode` and callers don't need `lattice-keymap` as a direct
/// dep. Paths resolve through `lattice_mode::keymap_entry::` re-exports above.
///
/// `file!()` and `line!()` expand at the **call site** — each entry records
/// its own source location, not the macro definition's location.
#[macro_export]
macro_rules! keymap_entry {
    // No-cmd form: defaults command to None.
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr $(,)? } => {
        $crate::keymap_entry! { mode: $mode, chord: $chord, doc: $doc, cmd: None }
    };
    // String-literal sugar + fall_through (SN.3c.2b).
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr, cmd: $cmd:literal, fall_through: $ft:expr $(,)? } => {
        $crate::keymap_entry! { mode: $mode, chord: $chord, doc: $doc, cmd: Some($cmd), fall_through: $ft }
    };
    // String-literal sugar: `cmd: "name"` -> `cmd: Some("name")`.
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr, cmd: $cmd:literal $(,)? } => {
        $crate::keymap_entry! { mode: $mode, chord: $chord, doc: $doc, cmd: Some($cmd) }
    };
    // Explicit form + fall_through (SN.3c.2b).
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr, cmd: $cmd:expr, fall_through: $ft:expr $(,)? } => {
        $crate::keymap_entry::KeymapEntry::__new(
            $chord,
            $crate::BindingMode::$mode,
            $doc,
            $cmd,
            $ft,
            $crate::keymap_entry::__builtin_source(file!(), line!()),
        )
    };
    // Explicit form: `cmd: None` or `cmd: Some(...)`. fall_through defaults false.
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr, cmd: $cmd:expr $(,)? } => {
        $crate::keymap_entry::KeymapEntry::__new(
            $chord,
            $crate::BindingMode::$mode,
            $doc,
            $cmd,
            false,
            $crate::keymap_entry::__builtin_source(file!(), line!()),
        )
    };
}
