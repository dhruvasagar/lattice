//! Display minor modes -- thin user-toggleable wrappers around
//! the typed display options that already shipped (DESIGN.md
//! §5.12 + mode-architecture.md §4.2.2 / M.7).
//!
//! Each mode contributes its corresponding typed option's
//! "on" value via `Mode::options()`. Activating the mode flips
//! the option in the buffer's resolved state via the
//! mode-contribution layer; deactivating removes the
//! contribution and the resolver falls back to the typed-option
//! layer (TOML / `:set` / default).
//!
//! ## Two surfaces, one underlying state
//!
//! `:set number=true` and `:line-numbers-mode` both flip the
//! resolved value of `Number` for the active buffer. They sit at
//! different layers of the resolver:
//!
//! - **Typed-option layer** (`:set`): the persistent global /
//!   per-project value; user-typed config.
//! - **Mode-contribution layer** (`:line-numbers-mode`): the
//!   per-buffer mode-driven override.
//!
//! Layer priority is `mode-contribution > typed-option`, so when
//! the mode is active it wins regardless of `:set` state. The
//! M.7.1 follow-up adds a cascade so `:set number=false` also
//! deactivates `line-numbers-mode`, giving full bidirectional
//! convergence; for v1 the mode is a one-way ratchet --
//! activating it forces the on-value, deactivating it removes
//! the contribution and the typed-option layer takes over.
//!
//! ## What's NOT here
//!
//! - `whitespace-show-mode` and `current-line-highlight-mode`:
//!   their backing typed options don't exist yet (the renderer
//!   has no `:set list` / `:set cursorline` plumbing). These
//!   land alongside the option in M.7.2.

use lattice_config::OptionOverrideSet;

use crate::{CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind};

/// Macro: declare a display minor mode that contributes a
/// single typed-option override when active.
///
/// `$option_path` is the option's type path (e.g.
/// `lattice_config::Number`); `$on_value` is the literal value
/// the mode contributes (typically `true` for booleans).
macro_rules! display_minor_mode {
    (
        $struct_name:ident,
        $mode_name:literal,
        $(mirrors $mirrors_name:literal,)?
        contributes $option_path:path = $on_value:expr,
        $($extra_overrides:tt)*
    ) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn options(&self) -> OptionOverrideSet {
                lattice_config::overrides! {
                    $option_path = $on_value,
                    $($extra_overrides)*
                }
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            $(
                fn mirrors_option(&self) -> Option<&'static str> {
                    Some($mirrors_name)
                }
            )?
            fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
        }
    };
}

display_minor_mode!(
    LineNumbersMode,
    "line-numbers-mode",
    mirrors "number",
    contributes lattice_config::Number = true,
);

// `relative-line-numbers-mode` mirrors vim's `:set rnu` cascade:
// rnu implies nu (so the gutter renders at all). We contribute
// both overrides directly. The `mirrors "relativenumber"` hint
// keeps the mode's active state and the option's value in sync
// via the host's option-mirror cascade -- the App's hardcoded
// per-mode special case in `apply_option_cascade` is gone,
// replaced with one declarative loop driven by this hint.
display_minor_mode!(
    RelativeLineNumbersMode,
    "relative-line-numbers-mode",
    mirrors "relativenumber",
    contributes lattice_config::RelativeNumber = true,
    lattice_config::Number = true,
);

display_minor_mode!(
    WrapMode,
    "wrap-mode",
    mirrors "wrap",
    contributes lattice_config::Wrap = true,
);

// `read-only-mode` -- user-toggleable surface for the same
// `ReadOnly` option that `help-mode` / `file-tree-mode` /
// LSP-log-mode contribute via M.3.1. Those majors are
// kind-driven (every help buffer gets read-only); this minor
// is the user gesture for "make THIS buffer read-only" on
// arbitrary buffer kinds. `ReadOnly` is `customizable = false`
// (mode-only); this is the only user-typed pathway. No
// `mirrors` hint because `ReadOnly` has no `:set` surface.
display_minor_mode!(
    ReadOnlyMode,
    "read-only-mode",
    contributes lattice_config::ReadOnly = true,
);

// M.7.2: `whitespace-show-mode` -- backing for `:set list`
// (vim convention). Renderer's whitespace-glyph plumbing
// lands in M.7.3; today the mode + option exist as the
// declarative + cascade surface, ready for the renderer hook.
display_minor_mode!(
    WhitespaceShowMode,
    "whitespace-show-mode",
    mirrors "whitespace",
    contributes lattice_config::Whitespace = true,
);

// M.7.2: `current-line-highlight-mode` -- backing for
// `:set cursorline`. Same M.7.3 deferral note as
// whitespace-show-mode: option + mode declared today,
// renderer's current-line-highlight pipeline lands later.
display_minor_mode!(
    CurrentLineHighlightMode,
    "current-line-highlight-mode",
    mirrors "current-line-highlight",
    contributes lattice_config::CursorLine = true,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_display_mode_has_distinct_id() {
        let ids = [
            LineNumbersMode::mode_id(),
            RelativeLineNumbersMode::mode_id(),
            WrapMode::mode_id(),
            ReadOnlyMode::mode_id(),
            WhitespaceShowMode::mode_id(),
            CurrentLineHighlightMode::mode_id(),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn each_display_mode_is_minor_with_no_caps() {
        let modes: Vec<&dyn Mode> = vec![
            &LineNumbersMode,
            &RelativeLineNumbersMode,
            &WrapMode,
            &ReadOnlyMode,
            &WhitespaceShowMode,
            &CurrentLineHighlightMode,
        ];
        for m in modes {
            assert_eq!(m.kind(), ModeKind::Minor, "{} not minor", m.id());
            assert_eq!(
                m.required_capabilities(),
                CapabilitySet::empty(),
                "{} declared caps",
                m.id(),
            );
        }
    }

    #[test]
    fn line_numbers_mode_contributes_number_true() {
        let opts = LineNumbersMode.options();
        // Single contribution.
        assert_eq!(opts.iter().count(), 1);
    }

    #[test]
    fn relative_line_numbers_mode_contributes_both_options() {
        // Mirrors vim's `:set rnu` ⇒ `:set nu` cascade by
        // contributing both directly. Lets a user have
        // `relative-line-numbers-mode` active without needing
        // `line-numbers-mode` separately.
        let opts = RelativeLineNumbersMode.options();
        assert_eq!(
            opts.iter().count(),
            2,
            "expected RelativeNumber + Number",
        );
    }

    #[test]
    fn wrap_mode_contributes_wrap_true() {
        let opts = WrapMode.options();
        assert_eq!(opts.iter().count(), 1);
    }

    #[test]
    fn read_only_mode_contributes_read_only_true() {
        let opts = ReadOnlyMode.options();
        assert_eq!(opts.iter().count(), 1);
    }

    #[test]
    fn whitespace_show_mode_contributes_whitespace_true() {
        let opts = WhitespaceShowMode.options();
        assert_eq!(opts.iter().count(), 1);
    }

    #[test]
    fn current_line_highlight_mode_contributes_cursorline_true() {
        let opts = CurrentLineHighlightMode.options();
        assert_eq!(opts.iter().count(), 1);
    }
}
