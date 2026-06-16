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
/// at the macro invocation site. Forms:
///
/// - `keymap_entry! { mode: Normal, chord: "j", doc: "...", cmd: "motion:line-down" }`
/// - `keymap_entry! { mode: Help, chord: "j", doc: "..." }` (no cmd)
/// - `keymap_entry! { mode: Normal, chord: "x", doc: "...", cmd: Some("plugin:foo") }` (explicit Option)
/// - `keymap_entry! { mode: [Normal, Visual], chord: "zn", doc: "...", cmd: "operator:narrow" }` (multi-mode)
///
/// The `mode:` slot accepts a single mode or a bracketed list; a single
/// mode sugars to a one-element slice so every entry carries
/// `modes: &[BindingMode]` (B-field). Existing single-mode call sites are
/// unchanged.
///
/// This is a forwarding shim: the macro body lives in `lattice-mode` so
/// `$crate = lattice_mode` and callers don't need `lattice-keymap` as a direct
/// dep. Paths resolve through `lattice_mode::keymap_entry::` re-exports above.
/// It MUST stay in lock-step with the canonical copy in
/// `lattice-keymap::keymap_entry`.
///
/// `file!()` and `line!()` expand at the **call site** — each entry records
/// its own source location, not the macro definition's location.
#[macro_export]
macro_rules! keymap_entry {
    // Entry arms: match `mode:` FRESH (single ident or bracketed list),
    // normalize to a parenthesized `&[BindingMode]` slice token, forward
    // the rest to `@build`. Matching mode here — never after a `:tt`
    // forward — sidesteps the macro_rules gotcha where an interpolated
    // `:tt` won't re-match as `:ident` / `[..]`.
    { mode: $m:ident, $($rest:tt)* } => {
        $crate::keymap_entry!(@build (&[$crate::BindingMode::$m]) $($rest)*)
    };
    { mode: [ $($m:ident),+ $(,)? ], $($rest:tt)* } => {
        $crate::keymap_entry!(@build (&[$($crate::BindingMode::$m),+]) $($rest)*)
    };

    // `@build`: `$modes` is the parenthesized slice token; only EMITTED
    // (re-matched as `:tt`, always safe), never destructured.
    // No-cmd form: defaults command to None.
    (@build $modes:tt chord: $chord:expr, doc: $doc:expr $(,)?) => {
        $crate::keymap_entry!(@build $modes chord: $chord, doc: $doc, cmd: None)
    };
    // String-literal sugar + fall_through (SN.3c.2b).
    (@build $modes:tt chord: $chord:expr, doc: $doc:expr, cmd: $cmd:literal, fall_through: $ft:expr $(,)?) => {
        $crate::keymap_entry!(@build $modes chord: $chord, doc: $doc, cmd: Some($cmd), fall_through: $ft)
    };
    // String-literal sugar: `cmd: "name"` -> `cmd: Some("name")`.
    (@build $modes:tt chord: $chord:expr, doc: $doc:expr, cmd: $cmd:literal $(,)?) => {
        $crate::keymap_entry!(@build $modes chord: $chord, doc: $doc, cmd: Some($cmd))
    };
    // Explicit form + fall_through (SN.3c.2b).
    (@build $modes:tt chord: $chord:expr, doc: $doc:expr, cmd: $cmd:expr, fall_through: $ft:expr $(,)?) => {
        $crate::keymap_entry::KeymapEntry::__new(
            $chord,
            $modes,
            $doc,
            $cmd,
            $ft,
            $crate::keymap_entry::__builtin_source(file!(), line!()),
        )
    };
    // Explicit form: `cmd: None` or `cmd: Some(...)`. fall_through defaults false.
    (@build $modes:tt chord: $chord:expr, doc: $doc:expr, cmd: $cmd:expr $(,)?) => {
        $crate::keymap_entry::KeymapEntry::__new(
            $chord,
            $modes,
            $doc,
            $cmd,
            false,
            $crate::keymap_entry::__builtin_source(file!(), line!()),
        )
    };
}
