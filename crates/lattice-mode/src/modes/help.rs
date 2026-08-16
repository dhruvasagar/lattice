//! `help-mode` -- minor mode that turns a markdown buffer into a
//! help buffer (DESIGN.md §5.11). Composes with `markdown-mode`
//! (the major), which carries the syntax pipeline + motion
//! semantics. Help-mode adds:
//!
//! - `ReadOnly = true` (option contribution).
//! - Link / anchor metadata parsing (today carried on the
//!   `HelpContent` bundle; future: contributed by help-mode's
//!   `on_activate`).
//! - `<CR>` follow-link dispatch (gated on this minor being
//!   active).
//! - The `:help` / `:describe-*` / `:apropos` / `:keymap` / etc.
//!   workflow commands (gated on this minor being active).
//!
//! Decoupling stance (M.4): the popup UI component is buffer-
//! agnostic. It can render any buffer; help-mode-tagged buffers
//! just happen to be the only popup content kind today. The
//! user's display preference (popup / split / tab / minibuffer)
//! is orthogonal to which mode the buffer carries.

use lattice_config::OptionOverrideSet;

use crate::{CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

pub struct HelpMode;

impl HelpMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("help-mode")
    }
}

impl Mode for HelpMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // `Wrap = false` (HP.3). It was `true` — "Bug 4: long help
        // bodies should wrap at the pane width rather than overflow
        // horizontally" — and that reasoning held while a help page was
        // prose. It stopped holding once pages carried structure.
        //
        // Wrapping is a per-LINE transform with no idea what the line
        // is part of, so a table row wider than the pane breaks in the
        // middle of a cell and the column below it no longer lines up
        // with anything. HP.1 aligns those columns by display width;
        // wrapping then takes the alignment apart again on exactly the
        // tables that most needed it — the wide ones. The same applies
        // to the box-drawing menu mock-ups in the magit pages and to
        // indented code samples, where a wrapped continuation reads as
        // a new line at the wrong depth.
        //
        // The trade is real and worth stating: a long prose paragraph
        // now runs off the right edge and needs horizontal scrolling
        // (`zl` / `zh`, or `:set wrap` for that buffer). That is the
        // lesser harm — prose that runs off the edge is still readable
        // once scrolled, where a broken table is misinformation about
        // which value belongs to which column. It is also the
        // convention: `man`, `info` and Emacs `*Help*` all lay out to a
        // fixed measure rather than reflow.
        //

        // `NoFile = true`: help buffers carry generated content
        // (apropos lists, describe-* renders), not on-disk
        // files; `:q` must not warn about unsaved changes.
        //
        // PU.1b-1a: help renders gutterless — `Number = false`
        // (no line-number gutter) + `signcolumn = no` (no
        // diagnostics / diff sign columns). These are plain option
        // values: the renderer derives the gutter geometry from them
        // and never knows it is painting help (a regular buffer with
        // `:set nonu signcolumn=no` renders identically).
        // IG.6: `IndentGuides = false`. A help page's leading whitespace is
        // layout — table cells, box-drawing mock-ups, list continuation —
        // not the indent structure of something being edited, so a rule
        // down it claims a nesting that is not there. It is an option
        // rather than a renderer check for the usual reason: a regular
        // buffer with `:setlocal noindent-guides` renders identically, and
        // neither peer learns what help is.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::Wrap = false,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
            lattice_config::SignColumnOption = lattice_config::SignColumn::No,
            lattice_config::core_options::IndentGuides = false,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// 2026-05-26: claim invocation dispatch for help panes via
    /// `Editor::run_help_invocation`. Help is a *minor* mode
    /// (`MarkdownMode` is the major), so the runner lookup walks
    /// active minors first and finds this id before falling
    /// through to the major.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(Self::mode_id())
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(HelpMode.id(), HelpMode::mode_id());
        assert_eq!(HelpMode::mode_id().as_str(), "help-mode");
        assert_eq!(HelpMode.kind(), ModeKind::Minor);
    }

    /// Read one override's value back out of the set.
    ///
    /// The set is type-erased (`TypeId` + `Arc<dyn Any>`), so a test
    /// that wants to assert a VALUE has to downcast. Worth the few
    /// lines: the previous test only counted the overrides, which meant
    /// it passed identically whether `Wrap` was `true` or `false` —
    /// it could not have caught HP.3 flipping it in either direction.
    fn value_of<D: lattice_config::OptionDecl>(opts: &OptionOverrideSet) -> Option<D::Value>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        opts.iter()
            .find(|o| o.option_type_id == std::any::TypeId::of::<D>())
            .and_then(|o| o.value.clone().downcast::<D::Value>().ok())
            .map(|v| (*v).clone())
    }

    #[test]
    fn contributes_read_only_wrap_and_no_file() {
        // NoFile keeps `:q` quiet when a help pane is the last buffer
        // open. PU.1b-1a adds Number = false + signcolumn = no so help
        // renders gutterless via the option-driven path.
        let opts = HelpMode.options();
        assert_eq!(
            opts.iter().count(),
            5,
            "expected ReadOnly + Wrap + NoFile + Number + SignColumn",
        );
        assert_eq!(value_of::<lattice_config::ReadOnly>(&opts), Some(true));
        assert_eq!(value_of::<lattice_config::NoFile>(&opts), Some(true));
        assert_eq!(value_of::<lattice_config::Number>(&opts), Some(false));
    }

    /// HP.3: **help does not wrap**, and the value is asserted rather
    /// than counted.
    ///
    /// Wrapping is a per-line transform that knows nothing about what
    /// the line belongs to, so a table row wider than the pane breaks
    /// mid-cell and the columns below stop lining up — undoing HP.1 on
    /// exactly the wide tables that needed aligning, and mangling the
    /// box-drawing menu mock-ups in the magit pages the same way.
    #[test]
    fn help_does_not_wrap() {
        assert_eq!(
            value_of::<lattice_config::Wrap>(&HelpMode.options()),
            Some(false),
            "help-mode must contribute `Wrap = false`: a wrapped table \
             row misreports which value belongs to which column, which \
             is worse than prose needing horizontal scroll",
        );
    }

    #[test]
    fn contributes_read_only() {
        let opts = HelpMode.options();
        assert!(!opts.is_empty());
    }
}
