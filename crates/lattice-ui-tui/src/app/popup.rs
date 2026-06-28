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

use crate::help::HelpContent;
use lattice_host::popup::{HelpMetadata, PopupPlacement};

use super::App;

/// One frame of in-popup navigation history. Captured by
/// [`App::snapshot_current_popup`] before [`App::swap_popup_content`]
/// overwrites the buffer; popped by [`App::pop_popup_back`] when the
/// user presses `<C-o>` from inside the popup. Carries everything
/// needed to fully restore the prior view: title, rope, cursor,
/// scroll, placement, and the link / anchor / highlight metadata
/// that backs the renderer + follow-link reader.
// Moved to lattice_host::popup::PopupSnapshot; keep local alias if needed.
pub use lattice_host::popup::PopupSnapshot;

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
    /// `HelpMetadata`). The buffer becomes `App.editor.popup_buffer` (the
    /// popup hot-path slot); the metadata is seeded into
    /// `App.editor.buffer_locals[buffer.id]` via [`Self::seed_help_metadata_locals`]
    /// so the renderer + link-follow / anchor-jump readers route
    /// uniformly through buffer_locals (M.3.2.c.5).
    pub(crate) fn open_popup(&mut self, content: HelpContent, placement: PopupPlacement) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        // The closure-tail publish from `mutate_editor_with` replaces
        // the prior 3c.atomic.E manual `publish_render_state()`.
        let signals = self.mutate_editor_with(move |e| e.open_popup(content, placement));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Open `content` as a *floating* popup over the active
    /// document (M.4 follow-up). Distinct from
    /// [`Self::open_popup`]: focus stays on the doc -- cursor
    /// motion in the doc auto-dismisses (the State A semantics
    /// `do_open_hover` codified). Activates `markdown-mode` as
    /// the major and `hover-mode` as the minor; the latter is
    /// what the dispatch's auto-dismiss check
    /// (`popup_has_hover_mode`) keys on.
    ///
    /// Used by hover (`K`), signature help, and any future
    /// cursor-anchored quick-info popup that wants the
    /// "popup floats; doc keeps focus" shape.
    pub(crate) fn open_floating_popup(&mut self, content: HelpContent, placement: PopupPlacement) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals =
            self.mutate_editor_with(move |e| e.open_floating_popup(content, placement));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// M.4 (b): clear out a popup buffer's registry / mode /
    /// option-cache state. Called by [`Self::open_popup`] before
    /// adopting a new popup (so back-to-back popups don't
    /// accumulate stale entries) and by [`Self::dismiss_popup`]
    /// when the popup closes. No-op when no popup is set.
    pub(super) fn dismiss_stale_popup_registry(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.dismiss_stale_popup_registry());
    }

    /// M.4 (b): resolve the popup's `HelpBuffer` through the
    /// unified registry. The field stores only the `BufferId`; the
    /// actual buffer lives in `app.editor.buffers` with
    /// `BufferFlags { listed: false, hidden: true }`. Returns a
    /// cloned snapshot (the rope is cheap-to-clone); `None` when no
    /// popup is open or the registry entry has been torn down.
    pub fn popup_help(&self) -> Option<crate::help::HelpBuffer> {
        // Slice 3c.final.B (group 3) note: the published
        // `rs.popup.help` substate IS populated and the GPUI peer
        // reads it directly. This TUI-side wrapper keeps the
        // legacy `editor.popup_help()` path because the test
        // suite + several out-of-dispatch help-popup setup paths
        // mutate the popup buffer without republishing
        // `RenderState`. Migrating the wrapper to read from
        // `rs.popup.help` requires adding `publish_render_state()`
        // calls at every popup-mutation site (open, scroll,
        // dismiss, follow-link, …) — deferred to a follow-up
        // (3c.final.B.3b) so the slice stays bounded.
        self.read_editor(move |e| e.popup_help())
    }

    // Slice 3c.final.E.5i: `with_popup_help_mut` moved to
    // `#[cfg(test)] impl App` below — the `impl FnOnce(&mut
    // HelpBuffer) -> R` arg is fundamentally incompatible with
    // the `Send + 'static` closure bound of `mutate_editor` (the
    // user-supplied `f` carries no Send bound, and adding one
    // would propagate `Send` requirements onto every test
    // fixture's local-borrow closure). Only callers are in
    // app.rs's `mod tests` block (popup_help_mut_*); production
    // code mutates popup buffers through the host directly.

    /// M.3.2.c.5: mirror parsed help metadata into the buffer-locals
    /// map for `buffer_id`. Idempotent (replace-on-collision). The
    /// active pane's buffer_id and `buffer.id` may differ (in-pane
    /// help registry uses the registry id; the popup uses the
    /// buffer's construction id) -- callers seed under both as
    /// needed.
    /// 5.5.G.7: body migrated to
    /// [`lattice_host::dispatch::Editor::seed_help_metadata_locals`].
    pub(crate) fn seed_help_metadata_locals(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        metadata: HelpMetadata,
    ) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.seed_help_metadata_locals(buffer_id, metadata));
    }

    // Slice 3c.final.E.swap: `popup_help_links`,
    // `popup_help_anchors`, `popup_help_highlights` return
    // borrowed slices off `editor.buffer_locals`; their only
    // callers are in `#[cfg(test)] mod tests` blocks. Moved to
    // the `#[cfg(test)] impl App` block at the bottom of this
    // file. `active_popup_placement` same — only `display.rs` +
    // `popup.rs` tests use it.

    /// Update the popup's placement in place. No-op when no popup
    /// is currently open. Used by callers that want to flip
    /// between cursor-anchored and centred mid-popup (e.g. hover
    /// promoted to a focused reference view).
    ///
    /// `#[allow(dead_code)]`: the method is a designed-in API
    /// surface (referenced from the module-level docs above) but
    /// no production call site lives at HEAD; current call sites
    /// pass the placement at popup open. Removing it now would
    /// re-add the test surface when the first promote-popup flow
    /// lands. Tests below exercise it.
    #[allow(dead_code)]
    pub(crate) fn set_popup_placement(&mut self, placement: PopupPlacement) {
        if self.popup().buffer_id.is_some() {
            self.mutate_editor(move |e| e.popup_placement = placement);
        }
    }

    /// Snapshot the current popup's content + cursor + metadata so
    /// it can be restored later by `<C-o>`. Returns `None` if no
    /// popup is open or the registry entry has been torn down.
    pub(super) fn snapshot_current_popup(&self) -> Option<PopupSnapshot> {
        // Phase 5.8.AE: body migrated.
        self.read_editor(move |e| e.snapshot_current_popup())
    }

    /// Swap `content` into the existing popup buffer in place.
    /// Phase 5.8.AE: body migrated.
    pub(super) fn swap_popup_content(&mut self, content: HelpContent, placement: PopupPlacement) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.swap_popup_content(content, placement));
    }

    // 5.5.H: `pop_popup_back` App-side delegate retired (zero
    // callers; host copy at
    // [`lattice_host::dispatch::Editor::pop_popup_back`]).

    /// Read-side accessor for the renderer: the active popup's
    /// placement, or `None` when no popup is open.
    ///
    /// Renamed from `popup_placement()` in Phase 5.B.10
    /// because the migrated `Editor::popup_placement` field
    /// shadowed the method via auto-deref; the rename keeps
    /// the method's intent (Option-returning gated accessor)
    /// distinct from the raw field.
    // Slice 3c.final.E.swap: `active_popup_placement` moved to
    // `#[cfg(test)] impl App` below — only test callers.

    /// Close the popup. Drops the popup's content slot, resets
    /// placement to default, and restores any focus state that
    /// was captured at open. Idempotent: closing when no popup
    /// is open is a no-op.
    pub(crate) fn dismiss_popup(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.dismiss_popup());
    }
}

// Slice 3c.final.E.5i — test-fixture surface for popup-help reads.
// PU.1a: `with_popup_help_mut` retired — help content is an
// actor-backed Document, so tests mutate `editor.cursor` /
// `editor.popup_cursor` (view state) directly, not the storage.
#[cfg(test)]
impl App {
    pub fn popup_help_links(&self) -> Option<&[crate::help::HelpLink]> {
        let id = self.popup().buffer_id?;
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::HelpLinks>())
            .map(|h| h.0.as_slice())
    }

    pub fn popup_help_anchors(&self) -> Option<&[crate::help::HelpAnchor]> {
        let id = self.popup().buffer_id?;
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::HelpAnchors>())
            .map(|h| h.0.as_slice())
    }

    pub fn popup_help_highlights(&self) -> Option<&[Vec<lattice_syntax::StyledSpan>]> {
        let id = self.popup().buffer_id?;
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::HelpHighlights>())
            .map(|h| h.0.as_slice())
    }

    pub fn active_popup_placement(&self) -> Option<PopupPlacement> {
        self.editor
            .popup_buffer
            .map(|_| self.popup().placement)
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
        assert_eq!(a.active_popup_placement(), Some(PopupPlacement::Centered));
    }

    #[test]
    fn hover_popup_is_cursor_anchored() {
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        assert_eq!(
            a.active_popup_placement(),
            Some(PopupPlacement::CursorAnchored)
        );
    }

    #[test]
    fn hover_popup_activates_hover_mode_minor() {
        // M.4: do_open_hover activates `hover-mode` as a minor on
        // the popup buffer. Future hover-only behaviour gates on
        // this mode being active rather than the popup's state
        // shape (`prev_pane_for_help.is_none()`).
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        let buffer_id = a.editor.popup_buffer.expect("popup open");
        let modes = a
            .editor
            .active_modes
            .get(&buffer_id)
            .expect("popup has modes");
        assert!(
            modes.minors().contains(&crate::modes::HoverMode::mode_id()),
            "hover popup should activate hover-mode minor; got {:?}",
            modes.minors()
        );
    }

    #[test]
    fn open_popup_with_explicit_placement_overrides_default() {
        let mut a = app_with("hello", 10);
        let buf = HelpContent::from_lines("test", vec!["body".into()]);
        a.open_popup(buf, PopupPlacement::CursorAnchored);
        assert_eq!(
            a.active_popup_placement(),
            Some(PopupPlacement::CursorAnchored)
        );
    }

    #[test]
    fn set_popup_placement_updates_open_popup() {
        let mut a = app_with("hello", 10);
        a.do_lsp_status();
        a.set_popup_placement(PopupPlacement::CursorAnchored);
        assert_eq!(
            a.active_popup_placement(),
            Some(PopupPlacement::CursorAnchored)
        );
    }

    #[test]
    fn set_popup_placement_is_noop_when_closed() {
        let mut a = app_with("hello", 10);
        a.set_popup_placement(PopupPlacement::CursorAnchored);
        // No popup open: placement read returns None regardless.
        assert_eq!(a.active_popup_placement(), None);
    }

    #[test]
    fn popup_registers_buffer_with_unlisted_hidden_flags() {
        // M.4 (b): popup buffers participate in `app.editor.buffers` like
        // every other buffer, with `listed: false` (skipped by `:bn`
        // / `:bp` / `:ls`) and `hidden: true` (informational; popups
        // don't have windows of their own).
        let mut a = app_with("hello", 10);
        a.do_lsp_status();
        let id = a.editor.popup_buffer.expect("popup open");
        let (flags, is_help) = a
            .editor
            .buffers
            .with_entry(id, |entry| {
                (entry.flags, entry.kind() == crate::buffers::BufferKind::Help)
            })
            .expect("popup registered");
        assert!(!flags.listed);
        assert!(flags.hidden);
        assert!(is_help);
        // `:ls` / `:bn` cycling skips it.
        assert!(!a.editor.buffers.listed_ids_sorted().contains(&id));
    }

    #[test]
    fn dismiss_popup_removes_buffer_from_registry() {
        // M.4 (b): closing a popup tears down its registry / mode /
        // option-cache state. Otherwise back-to-back popups
        // accumulate stale entries indefinitely.
        let mut a = app_with("hello", 10);
        a.do_lsp_status();
        let id = a.editor.popup_buffer.expect("popup open");
        assert!(a.editor.buffers.contains(id));
        a.dismiss_popup();
        assert!(!a.editor.buffers.contains(id));
        assert!(a.editor.active_modes.get(&id).is_none());
        assert!(a.editor.buffer_locals.get(&id).is_none());
    }

    #[test]
    fn back_to_back_popups_reuse_the_same_buffer() {
        // Opening a second popup while one is already open swaps
        // the content in place rather than allocating a fresh
        // buffer. Jump-list / marks / search state keyed by the
        // popup id stay coherent across in-popup navigation; the
        // registry never holds more than one popup at a time.
        let mut a = app_with("hello", 10);
        a.do_lsp_status();
        let first_id = a.editor.popup_buffer.expect("first popup open");
        a.do_lsp_status();
        let second_id = a.editor.popup_buffer.expect("second popup open");
        assert_eq!(
            first_id, second_id,
            "popup id should be reused on in-Help reopen"
        );
        assert!(
            a.editor.buffers.contains(first_id),
            "popup buffer survives the swap"
        );
        // The prior frame is recorded on the back-stack so `<C-o>`
        // can restore it.
        assert_eq!(a.editor.popup_back_stack.len(), 1);
    }

    #[test]
    fn dismiss_popup_clears_placement() {
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        assert_eq!(
            a.active_popup_placement(),
            Some(PopupPlacement::CursorAnchored)
        );
        a.dismiss_popup();
        assert_eq!(a.active_popup_placement(), None);
        assert_eq!(a.editor.popup_placement, PopupPlacement::default());
    }

    #[test]
    fn opening_centered_popup_after_hover_resets_placement() {
        let mut a = app_with("hello", 10);
        a.do_open_hover("hover body");
        // Subsequent command-launched popup must override the
        // sticky CursorAnchored placement from the prior hover.
        a.dismiss_popup();
        a.do_lsp_status();
        assert_eq!(a.active_popup_placement(), Some(PopupPlacement::Centered));
    }
}
