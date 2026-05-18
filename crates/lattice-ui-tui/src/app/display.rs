//! Buffer-display dispatch (DESIGN.md §5.9).
//!
//! Every command that produces a help-style buffer
//! (`:lsp-status`, `:help foo`, `:diagnostics`, hover, picker
//! accept, ...) routes through [`App::display_buffer`]. The
//! caller passes a [`BufferDisplayCategory`] expressing *intent*
//! ("this is an LSP log", "this is a hover popup"); the App
//! resolves the category to a concrete [`BufferDisplay`] -- via
//! [`default_display`] today, with user-supplied overrides
//! layered on in a follow-up -- and dispatches to the matching
//! surface.
//!
//! Three surfaces in v1:
//! - [`BufferDisplay::Popup`] -> [`App::open_popup`] (overlay).
//! - [`BufferDisplay::ActivePane`] ->
//!   [`App::open_help_in_pane`] (registry-tracked, swap active
//!   pane).
//! - [`BufferDisplay::Split`] -> [`App::open_help_in_split`]
//!   (split active pane, focus the new sibling).
//!
//! A future GPUI / web renderer adds variants for tabs / OS
//! windows / inline panels without changing call sites: each
//! command keeps emitting its category, and the resolver +
//! dispatcher are the only places that learn new variants.

use lattice_core::ui::display::{BufferDisplay, BufferDisplayCategory};
use lattice_core::ui::pane::SplitOrientation;

use crate::buffers::BufferId;
use crate::help::HelpContent;

use super::App;

impl App {
    /// Display `content` under the given `category`. Resolves the
    /// category to a [`BufferDisplay`] (built-in default for now;
    /// user overrides land in a follow-up) and dispatches.
    ///
    /// Returns the registered [`BufferId`] for `ActivePane` /
    /// `Split` displays so callers can wire follow-up state
    /// (e.g. live-tail subscriptions key off this id). Returns
    /// `None` for `Popup` displays -- the popup buffer lives in
    /// the registry too, but most callers don't need the id back.
    pub(crate) fn display_buffer(
        &mut self,
        content: HelpContent,
        category: BufferDisplayCategory,
    ) -> Option<BufferId> {
        let display = self.resolve_display(category);
        match display {
            BufferDisplay::Popup(placement) => {
                self.open_popup(content, placement);
                self.editor.popup_buffer
            }
            BufferDisplay::FloatingPopup(placement) => {
                self.open_floating_popup(content, placement);
                self.editor.popup_buffer
            }
            BufferDisplay::ActivePane => Some(self.open_help_in_pane(content)),
            BufferDisplay::Split(orientation) => {
                Some(self.open_help_in_split(content, orientation))
            }
        }
    }

    /// Resolve a [`BufferDisplayCategory`] to a concrete
    /// [`BufferDisplay`]. Reads the per-category typed option
    /// (`:set <category>.display = ...`) and falls back to
    /// `default_display` when the option resolves to
    /// `BufferDisplayPreference::Default` (the implicit value
    /// when the user hasn't set it explicitly).
    ///
    /// Reads route through the config registry's typed-keyed
    /// `get_typed::<D>()` -- O(1) hash lookup + an `Arc::clone`.
    pub fn resolve_display(&self, category: BufferDisplayCategory) -> BufferDisplay {
        // Phase 5.8.AD.6: body migrated.
        self.editor.resolve_display(category)
    }

    /// Apply the [`BufferDisplayCategory::PickerResult`] preference
    /// to the active pane *before* a picker jump / buffer-switch
    /// runs. `ActivePane` (default) is a no-op; `Split` performs
    /// the split + focus shift so the subsequent file-edit /
    /// activation lands in a new sibling pane. `Popup` doesn't
    /// translate cleanly to a file buffer (popups hold help-style
    /// content); we fall through to `ActivePane` and surface no
    /// error -- the user-set override applies to *normal* buffer
    /// outputs only.
    pub(crate) fn prepare_pane_for_picker_result(&mut self) {
        // Phase 5.8.AD.6: body migrated.
        self.editor.prepare_pane_for_picker_result();
    }

    /// Open `content` in a fresh split alongside the active pane.
    /// Mirrors vim's `:help` (horizontal) and `:vert help`
    /// (vertical) -- the new pane gains focus, the original
    /// stays put with its content.
    ///
    /// The help buffer is registered in `app.editor.buffers` (same shape
    /// as [`Self::open_help_in_pane`]) and adopted by the new
    /// pane through the activation path so mode state, help
    /// metadata locals, and the popup hot-path slot all converge
    /// on the registered id.
    pub(crate) fn open_help_in_split(
        &mut self,
        content: HelpContent,
        orientation: SplitOrientation,
    ) -> BufferId {
        // Sync the active pane's hot-path cursor / scroll into
        // its stash so the new sibling clones a fresh snapshot.
        // (`split_active` copies the active pane's content +
        // cursor + scroll into the new sibling; without the
        // snapshot the snapshot would be stale.)
        self.snapshot_active_pane();
        let new_idx = self.editor.pane_tree.split_active(orientation);
        // Focus the new pane before adopting the help buffer so
        // `activate_help_in_pane` swaps the right pane's content.
        // `set_active` is a no-op if `new_idx` is already active,
        // which `split_active` doesn't promise (the active pane
        // stays the original by default, matching `do_split_pane`).
        self.editor.pane_tree.set_active(new_idx);
        // From here the in-pane path handles registry adoption,
        // mode activation, locals seeding, and the popup hot-
        // path slot mirror.
        self.open_help_in_pane(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_helpers::app_with;
    use crate::buffers::BufferKind;
    use crate::popup::PopupPlacement;

    #[test]
    fn lsp_status_category_routes_to_centered_popup() {
        let mut a = app_with("hi", 5);
        let content = HelpContent::from_lines("status", vec!["server: rust-analyzer".into()]);
        a.display_buffer(content, BufferDisplayCategory::LspStatus);
        assert_eq!(a.active_popup_placement(), Some(PopupPlacement::Centered));
        assert!(a.editor.popup_buffer.is_some());
    }

    #[test]
    fn lsp_log_category_routes_to_active_pane() {
        let mut a = app_with("hi", 5);
        let content = HelpContent::from_lines("lsp-log", vec!["...".into()]);
        let id = a
            .display_buffer(content, BufferDisplayCategory::LspLog)
            .expect("active-pane returns an id");
        // Active-pane adoption: pane swaps to the help buffer and
        // active_buffer flips to Help. (`popup_buffer` is set as a
        // hot-path mirror by `open_help_in_pane`; that's expected
        // -- the popup *slot* is reused; what matters here is the
        // pane state.)
        assert_eq!(a.editor.active_buffer, BufferKind::Help);
        assert_eq!(a.editor.pane_tree.active().buffer_id, id);
        assert_eq!(a.editor.pane_tree.active().buffer, BufferKind::Help);
    }

    #[test]
    fn hover_category_routes_to_cursor_anchored_popup() {
        let mut a = app_with("hi", 5);
        let content = HelpContent::from_lines("hover", vec!["doc string".into()]);
        a.display_buffer(content, BufferDisplayCategory::Hover);
        assert_eq!(
            a.active_popup_placement(),
            Some(PopupPlacement::CursorAnchored)
        );
    }

    #[test]
    fn set_category_display_overrides_resolved_value() {
        // M.4 follow-up: a typed-option override
        // (`:set hover.display = popup-cursor`) flips
        // `App::resolve_display(Hover)` from the built-in
        // floating-cursor default to the user-chosen
        // popup-cursor variant. Mechanism: the
        // `BufferDisplayPreference` enum-typed option resolves
        // to a non-`Default` variant; `pref.resolve(category)`
        // returns the override.
        let mut a = app_with("hi", 5);
        // Default (Hover) is FloatingPopup(CursorAnchored).
        assert_eq!(
            a.resolve_display(BufferDisplayCategory::Hover),
            BufferDisplay::FLOATING_CURSOR
        );
        // Override via the typed-options surface.
        a.do_set("hover.display=popup-cursor");
        assert_eq!(
            a.resolve_display(BufferDisplayCategory::Hover),
            BufferDisplay::POPUP_CURSOR
        );
        // Resetting to default round-trips back.
        a.do_set("hover.display=default");
        assert_eq!(
            a.resolve_display(BufferDisplayCategory::Hover),
            BufferDisplay::FLOATING_CURSOR
        );
    }

    #[test]
    fn picker_result_active_pane_default_is_noop() {
        // Default for PickerResult is ActivePane, so calling
        // `prepare_pane_for_picker_result` should not split.
        let mut a = app_with("hi", 5);
        let initial = a.editor.pane_tree.len();
        a.prepare_pane_for_picker_result();
        assert_eq!(a.editor.pane_tree.len(), initial);
    }

    #[test]
    fn split_horizontal_creates_second_pane_focused_on_help() {
        // Smoke test for the split path that wasn't reachable from
        // any command before this slice. Routes through the
        // resolver via a synthesized override (we don't expose a
        // category whose default is Split yet).
        let mut a = app_with("hi", 5);
        let initial_pane_count = a.editor.pane_tree.len();
        let content = HelpContent::from_lines("help-split", vec!["x".into()]);
        let id = a.open_help_in_split(content, SplitOrientation::Horizontal);
        assert_eq!(a.editor.pane_tree.len(), initial_pane_count + 1);
        // The active pane after the split holds the help buffer.
        assert_eq!(a.editor.pane_tree.active().buffer_id, id);
        assert_eq!(a.editor.active_buffer, BufferKind::Help);
    }
}
