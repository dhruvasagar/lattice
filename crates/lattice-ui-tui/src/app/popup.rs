//! Popup overlay lifecycle.
//!
//! A popup is a rectangular UI surface drawn over the buffer
//! area. Inside the popup a buffer renders the same way it would
//! inside any other window / split / tab; the popup itself is
//! content-agnostic and provides no key bindings of its own. Mode-
//! specific behaviour (help-mode binding `q` / `<Esc>` to close,
//! diagnostic-detail-mode binding `<CR>` to follow the link, etc.)
//! comes from the buffer's major mode, exactly as it would in a
//! split. The popup is purely a renderer + lifecycle concern.
//!
//! ## Surface
//!
//! - [`App::open_popup`] — display a popup with `buffer` as its
//!   content; the caller passes the [`PopupPlacement`] explicitly
//!   so cursor-anchored vs centred is a decision *at the call
//!   site*, not a hidden default. Hover / signature help pass
//!   [`PopupPlacement::CursorAnchored`]; everything else
//!   (`:lsp-status`, `:describe-*`, `:apropos`, `:help`,
//!   `:keymap`, `:options`, `:ls`, `:lsp-log`, ...) passes
//!   [`PopupPlacement::Centered`].
//! - [`App::set_popup_placement`] — update placement while the
//!   popup is still open (e.g. promoting an anchored hover into a
//!   centred reference view on focus).
//! - [`App::dismiss_popup`] — close the popup; restores any focus
//!   state captured when the user focused into it. Idempotent.
//! - [`App::popup_placement`] — read-side accessor for the
//!   renderer; returns `None` when no popup is open.
//!
//! ## Why placement on `App`, not on `HelpBuffer`
//!
//! The popup is a generic surface; the buffer inside is incidental.
//! Storing placement on the buffer would conflate "what to show"
//! with "where to put it" -- two pieces of state that change
//! independently. A future file-preview popup wouldn't suddenly
//! gain a `placement` field on `Buffer`; the popup gains the
//! field, exactly as it does today.

use crate::buffers::BufferKind;
use crate::help::{HelpContent, HelpMetadata};
use crate::popup::PopupPlacement;

use super::{App, PositionSource, PrevPaneState};

impl App {
    /// Open a popup with `content` as its body at the requested
    /// `placement`. The popup focuses in: subsequent vim-grammar
    /// motions and ex-commands operate on the popup's content
    /// (mode-specific bindings come from the buffer's major
    /// mode). Captures pre-popup focus state so [`Self::dismiss_popup`]
    /// restores the user cleanly to the prior buffer / cursor /
    /// scroll.
    ///
    /// `content` is a [`HelpContent`] = (slim `HelpBuffer`, parsed
    /// `HelpMetadata`). The buffer becomes `App.help_buffer` (the
    /// popup hot-path slot); the metadata is seeded into
    /// `App.buffer_locals[buffer.id]` via [`Self::seed_help_metadata_locals`]
    /// so the renderer + link-follow / anchor-jump readers route
    /// uniformly through buffer_locals (M.3.2.c.5).
    pub(crate) fn open_popup(&mut self, content: HelpContent, placement: PopupPlacement) {
        let HelpContent { buffer, metadata } = content;
        let buffer_id = buffer.id;
        // Record the *document* cursor (we're still active=Document
        // here, since the popup-open precedes the active_buffer
        // flip). Skip the push if we're already in Help (a help->
        // help re-open from a link follow); the inter-help
        // transition is recorded by `do_help_follow_link` itself.
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Sync the active pane's cursor / scroll stash *before*
        // swapping `active_buffer` to Help. Once active is Help,
        // the active pane's buffer (Document) no longer matches
        // `app.active_buffer`, so the renderer paints it as
        // visually inactive -- reading from `pane.cursor` rather
        // than `app.cursor`. Without this snapshot the pane stash
        // is whatever it was last set to (often (0,0)) and the
        // doc visibly jumps to the top of file when the popup
        // opens.
        self.snapshot_active_pane();
        // Capture pre-popup state so dismiss restores cleanly.
        // Mirrors `activate_help_in_pane` / `focus_help_popup`.
        if !matches!(self.active_buffer, BufferKind::Help) {
            let active = self.pane_tree.active();
            self.prev_pane_for_help = Some(PrevPaneState {
                buffer: active.buffer,
                buffer_id: active.buffer_id,
                cursor: self.cursor,
                scroll: self.scroll,
            });
        }
        // Load the buffer's cursor / scroll into the App's hot
        // path. Motion / scroll / search read / write them
        // uniformly across buffer kinds.
        let stash_cursor = buffer.cursor;
        let stash_scroll = buffer.scroll as u32;
        self.help_buffer = Some(buffer);
        self.popup_placement = placement;
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
        // M.3.2.c.5: seed parsed metadata into buffer_locals.
        // Reader sites (renderer's link / anchor / highlights
        // lookups, do_help_follow_link, scroll-to-anchor) consume
        // the locals exclusively now that the struct fields are
        // gone.
        self.seed_help_metadata_locals(buffer_id, metadata);
    }

    /// M.3.2.c.5: mirror parsed help metadata into the buffer-locals
    /// map for `buffer_id`. Idempotent (replace-on-collision). The
    /// active pane's buffer_id and `buffer.id` may differ (in-pane
    /// help registry uses the registry id; the popup uses the
    /// buffer's construction id) -- callers seed under both as
    /// needed.
    pub(crate) fn seed_help_metadata_locals(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        metadata: HelpMetadata,
    ) {
        let HelpMetadata { links, anchors, highlights } = metadata;
        let locals = self.buffer_locals.entry(buffer_id).or_default();
        locals.insert(crate::modes::HelpLinks(links));
        locals.insert(crate::modes::HelpAnchors(anchors));
        locals.insert(crate::modes::HelpHighlights(highlights));
    }

    /// Update the popup's placement in place. No-op when no popup
    /// is currently open. Used by callers that want to flip
    /// between cursor-anchored and centred mid-popup (e.g. hover
    /// promoted to a focused reference view).
    pub(crate) fn set_popup_placement(&mut self, placement: PopupPlacement) {
        if self.help_buffer.is_some() {
            self.popup_placement = placement;
        }
    }

    /// Read-side accessor for the renderer: the active popup's
    /// placement, or `None` when no popup is open.
    pub fn popup_placement(&self) -> Option<PopupPlacement> {
        self.help_buffer.as_ref().map(|_| self.popup_placement)
    }

    /// Close the popup. Drops the popup's content slot, resets
    /// placement to default, and restores any focus state that
    /// was captured at open. Idempotent: closing when no popup
    /// is open is a no-op.
    pub(crate) fn dismiss_popup(&mut self) {
        self.help_buffer = None;
        self.popup_placement = PopupPlacement::default();
        // Restore pre-popup state if focus had moved into it
        // (State B for hover; in-pane mode for `:lsp-log` etc.).
        // State A (popup shown but never focused) leaves
        // `prev_pane_for_help` as `None` -- nothing to restore;
        // active was never flipped to Help.
        if let Some(prev) = self.prev_pane_for_help.take() {
            self.cursor = prev.cursor;
            self.scroll = prev.scroll;
            let pane = self.pane_tree.active_mut();
            pane.buffer = prev.buffer;
            pane.buffer_id = prev.buffer_id;
            self.active_buffer = prev.buffer;
        } else {
            self.active_buffer = BufferKind::Document;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_helpers::app_with;
    use crate::help::HelpContent;

    #[test]
    fn lsp_status_popup_is_centered() {
        let mut a = app_with("hello", 10);
        a.do_lsp_status();
        assert_eq!(a.popup_placement(), Some(PopupPlacement::Centered));
    }

    #[test]
    fn hover_popup_is_cursor_anchored() {
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        assert_eq!(a.popup_placement(), Some(PopupPlacement::CursorAnchored));
    }

    #[test]
    fn open_popup_with_explicit_placement_overrides_default() {
        let mut a = app_with("hello", 10);
        let buf = HelpContent::from_lines("test", vec!["body".into()]);
        a.open_popup(buf, PopupPlacement::CursorAnchored);
        assert_eq!(a.popup_placement(), Some(PopupPlacement::CursorAnchored));
    }

    #[test]
    fn set_popup_placement_updates_open_popup() {
        let mut a = app_with("hello", 10);
        a.do_lsp_status();
        a.set_popup_placement(PopupPlacement::CursorAnchored);
        assert_eq!(a.popup_placement(), Some(PopupPlacement::CursorAnchored));
    }

    #[test]
    fn set_popup_placement_is_noop_when_closed() {
        let mut a = app_with("hello", 10);
        a.set_popup_placement(PopupPlacement::CursorAnchored);
        // No popup open: placement read returns None regardless.
        assert_eq!(a.popup_placement(), None);
    }

    #[test]
    fn dismiss_popup_clears_placement() {
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        assert_eq!(a.popup_placement(), Some(PopupPlacement::CursorAnchored));
        a.dismiss_popup();
        assert_eq!(a.popup_placement(), None);
        assert_eq!(a.popup_placement, PopupPlacement::default());
    }

    #[test]
    fn opening_centered_popup_after_hover_resets_placement() {
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        // Subsequent command-launched popup must override the
        // sticky CursorAnchored placement from the prior hover.
        a.dismiss_popup();
        a.do_lsp_status();
        assert_eq!(a.popup_placement(), Some(PopupPlacement::Centered));
    }
}
