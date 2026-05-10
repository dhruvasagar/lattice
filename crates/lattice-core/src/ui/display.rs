//! Buffer display preferences (DESIGN.md §5.9).
//!
//! When an ex-command produces a buffer (`:lsp-status`, `:help foo`,
//! `:diagnostics`, picker accept, hover, ...) it doesn't open the
//! buffer directly -- it asks the App to *display* it under a
//! [`BufferDisplayCategory`]. The App resolves the category to a
//! concrete [`BufferDisplay`] (built-in default today; user-
//! overridable via typed options in a follow-up) and dispatches to
//! the matching surface: popup overlay, active pane replacement,
//! or a fresh split.
//!
//! Decoupling the *what* (the buffer) from the *where* (the
//! display) means a single user preference -- "I want LSP logs in
//! a horizontal split, not a popup" -- is one toggle, not a patch
//! to every command that produces an LSP log buffer.
//!
//! The taxonomy is by *intent*, not by command: adding a new
//! `:describe-symbol` falls under the existing
//! [`BufferDisplayCategory::HelpDescribe`] knob without a new
//! category; adding a whole new feature (say `git.log`) is one
//! new variant.

use crate::ui::pane::SplitOrientation;
use crate::ui::popup::PopupPlacement;

/// Where to put a buffer the App is about to display.
///
/// Renderer-agnostic: a future GPUI / web renderer maps these
/// variants to its own surfaces. The TUI maps `Popup` to the
/// existing centred-or-anchored overlay, `FloatingPopup` to the
/// hover-style overlay (popup floats; the doc keeps focus),
/// `ActivePane` to the `:lsp-log`-style swap-active-pane path,
/// and `Split` to a horizontal / vertical split via the pane
/// tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferDisplay {
    /// Focused overlay -- the popup gains focus, the underlying
    /// document is paused. Used by `:lsp-status` /
    /// `:describe-*` / `:apropos` / etc. Carries its own
    /// placement (cursor-anchored vs centred).
    Popup(PopupPlacement),
    /// Floating overlay -- the popup paints on top, but the
    /// document keeps focus. Cursor motion in the doc
    /// auto-dismisses (via the `hover-mode` minor contract).
    /// Used by hover (`K`) and signature help.
    FloatingPopup(PopupPlacement),
    /// Replace the active pane's buffer with this one. The
    /// previous buffer stays in the registry; pane history /
    /// `<C-^>` (post v1) returns to it.
    ActivePane,
    /// Split the active pane (orientation-dependent) and open
    /// the buffer in the new pane. The new pane gains focus,
    /// matching vim's `:help` / `:vert help`.
    Split(SplitOrientation),
}

impl BufferDisplay {
    pub const POPUP_CENTERED: Self = Self::Popup(PopupPlacement::Centered);
    pub const POPUP_CURSOR: Self = Self::Popup(PopupPlacement::CursorAnchored);
    pub const FLOATING_CURSOR: Self = Self::FloatingPopup(PopupPlacement::CursorAnchored);
    pub const SPLIT_HORIZONTAL: Self = Self::Split(SplitOrientation::Horizontal);
    pub const SPLIT_VERTICAL: Self = Self::Split(SplitOrientation::Vertical);
}

/// Per-feature display preference. Each command that produces a
/// buffer carries its category; the App resolves the category to
/// a [`BufferDisplay`] via [`default_display`] (today) or a
/// user-supplied override (follow-up).
///
/// Granularity rationale: per-command is too narrow (six
/// `:describe-*` variants would need six knobs); per-`BufferKind`
/// is too coarse (every help-flavoured surface is `Help`, but
/// hover and `:lsp-log` have wildly different display intents).
/// Per-category groups commands that share *intent*, which is
/// what users actually configure.
///
/// Multiple-choice surfaces (`:diagnostics`, `:references`,
/// `:symbol`, `:Files`, `:buffers`) are *picker-shaped by
/// design* -- the user picks one of N candidates -- and don't
/// have a category here. Their picker UI is a fixed surface;
/// where the *selected* result lands is governed by
/// [`Self::PickerResult`]. The categories below all describe
/// dedicated single-buffer outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferDisplayCategory {
    // ---- LSP feature group ----
    /// `:lsp-status` -- one-shot read of the supervisor state.
    LspStatus,
    /// `:lsp-log` / `:lsp-trace-log` -- live-tailed log views
    /// the user wants to keep around. (`:lsp-server-log` is a
    /// picker; it routes through [`Self::PickerResult`] once the
    /// user picks a server, then the chosen log opens under
    /// `LspLog`.)
    LspLog,

    // ---- Help feature group ----
    /// `:help <topic>` -- free-form help docs.
    HelpTopic,
    /// `:describe-command/-buffer/-key/-option/-mode/-event` --
    /// introspection-driven help.
    HelpDescribe,
    /// `:apropos <pattern>` -- search command names + docs.
    HelpApropos,
    /// `:ls`, `:keymap`, `:marks`, `:registers`, `:options` --
    /// state-listing help views.
    HelpList,

    // ---- Cursor-adjacent overlays ----
    /// `K` -- inline hover / quick-info popup. Auto-dismisses on
    /// cursor motion.
    Hover,
    /// Signature help -- argument-list popup that follows the
    /// cursor as the user types args.
    Signature,

    // ---- Picker accept destination ----
    /// Where the *selected* buffer / location lands after the
    /// user accepts a picker entry. Used by `:diagnostics` /
    /// `:references` / `:symbol` / `:Files` / `:buffers` /
    /// `:lsp-server-log` -- their picker UI is fixed; this knob
    /// controls the post-accept buffer placement.
    PickerResult,
}

/// Built-in default for each category. Matches today's
/// hard-coded behaviour so the dispatch refactor lands without
/// observable behaviour changes; user overrides layer on top
/// in a follow-up slice.
pub const fn default_display(category: BufferDisplayCategory) -> BufferDisplay {
    use BufferDisplayCategory as C;
    match category {
        C::LspStatus => BufferDisplay::POPUP_CENTERED,
        C::LspLog => BufferDisplay::ActivePane,
        C::HelpTopic => BufferDisplay::POPUP_CENTERED,
        C::HelpDescribe => BufferDisplay::POPUP_CENTERED,
        C::HelpApropos => BufferDisplay::POPUP_CENTERED,
        C::HelpList => BufferDisplay::POPUP_CENTERED,
        C::Hover => BufferDisplay::FLOATING_CURSOR,
        C::Signature => BufferDisplay::FLOATING_CURSOR,
        C::PickerResult => BufferDisplay::ActivePane,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_legacy_hardcoded_behaviour() {
        // `:lsp-status` was `open_popup(centered)` -- preserved.
        assert_eq!(
            default_display(BufferDisplayCategory::LspStatus),
            BufferDisplay::POPUP_CENTERED
        );
        // `:lsp-log` family was `open_help_in_pane` -- preserved.
        assert_eq!(
            default_display(BufferDisplayCategory::LspLog),
            BufferDisplay::ActivePane
        );
        // Hover routes to the floating-popup variant (M.4
        // follow-up): popup floats, doc keeps focus; the
        // hover-mode minor's auto-dismiss-on-cursor-motion
        // contract makes State A semantics observable here.
        assert_eq!(
            default_display(BufferDisplayCategory::Hover),
            BufferDisplay::FLOATING_CURSOR
        );
    }

    #[test]
    fn buffer_display_constants_match_variants() {
        assert_eq!(
            BufferDisplay::POPUP_CENTERED,
            BufferDisplay::Popup(PopupPlacement::Centered)
        );
        assert_eq!(
            BufferDisplay::SPLIT_HORIZONTAL,
            BufferDisplay::Split(SplitOrientation::Horizontal)
        );
    }
}
