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
use crate::help::HelpContent;
use lattice_host::popup::{HelpMetadata, PopupPlacement};

use super::{App, PositionSource, PrevPaneState};

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
        // Two paths into a popup, distinguished by `active_buffer`:
        //
        // - From outside Help (`Document` / `Oil` / `FileTree`):
        //   fresh top-level popup. Drop any prior back-stack so
        //   stale frames from a closed help session don't
        //   accumulate.
        // - From within Help: the user just followed a help link
        //   (`[foo](mode:foo)` / `command:` / `help:` etc.). Reuse
        //   the *same* popup buffer by swapping its content in
        //   place; snapshot the prior state onto `popup_back_stack`
        //   so `<C-o>` walks back to it without leaving the
        //   popup.
        if matches!(self.editor.active_buffer, BufferKind::Help)
            && self.editor.popup_buffer.is_some()
        {
            if let Some(snap) = self.snapshot_current_popup() {
                self.editor.popup_back_stack.push(snap);
            }
            self.swap_popup_content(content, placement);
            return;
        }
        self.editor.popup_back_stack.clear();
        let HelpContent { buffer, metadata } = content;
        let buffer_id = buffer.id;
        // Drop any previous popup buffer cleanly before adopting
        // the new one. Avoids stale registry entries / mode state
        // accumulating across back-to-back `:lsp-status` / `:help`
        // / `:apropos` invocations.
        self.dismiss_stale_popup_registry();
        // Record the *document* cursor (we're still active=Document
        // here, since the popup-open precedes the active_buffer
        // flip). Skip the push if we're already in Help (a help->
        // help re-open from a link follow); the inter-help
        // transition is recorded by `do_help_follow_link` itself.
        if matches!(self.editor.active_buffer, BufferKind::Document) {
            let cur = self.editor.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Sync the active pane's cursor / scroll stash *before*
        // swapping `active_buffer` to Help. Once active is Help,
        // the active pane's buffer (Document) no longer matches
        // `app.editor.active_buffer`, so the renderer paints it as
        // visually inactive -- reading from `pane.cursor` rather
        // than `app.editor.cursor`. Without this snapshot the pane stash
        // is whatever it was last set to (often (0,0)) and the
        // doc visibly jumps to the top of file when the popup
        // opens.
        self.snapshot_active_pane();
        // Capture pre-popup state so dismiss restores cleanly.
        // Mirrors `activate_help_in_pane` / `focus_help_popup`.
        if !matches!(self.editor.active_buffer, BufferKind::Help) {
            let active = self.editor.pane_tree.active();
            self.editor.prev_pane_for_help = Some(PrevPaneState {
                buffer: active.buffer,
                buffer_id: active.buffer_id,
                cursor: self.editor.cursor,
                scroll: self.editor.scroll,
            });
        }
        // Load the buffer's cursor / scroll into the App's hot
        // path. Motion / scroll / search read / write them
        // uniformly across buffer kinds.
        let stash_cursor = buffer.cursor;
        let stash_scroll = buffer.scroll as u32;
        // M.4 (b): popup buffers participate in `app.editor.buffers` like
        // any other buffer. The registry entry is the durable
        // record; the `popup_buffer` slot just holds the id.
        // `listed: false` keeps `:ls` / `:bn` / `:bp` from cycling
        // through transient popups; `hidden: true` is
        // informational (popups don't have windows of their own).
        // Same shape as [`Self::open_help_in_pane`] uses for
        // `:lsp-log` etc.
        self.editor
            .buffers
            .insert(crate::buffer_registry::BufferEntry {
                id: buffer_id,
                flags: crate::buffers::BufferFlags {
                    listed: false,
                    hidden: true,
                },
                data: crate::buffer_registry::BufferData::Help(buffer),
                name: None,
            });
        self.editor.popup_buffer = Some(buffer_id);
        self.editor.popup_placement = placement;
        self.editor.cursor = stash_cursor;
        self.editor.scroll = stash_scroll;
        self.editor.active_buffer = BufferKind::Help;
        // M.3.2.c.5: seed parsed metadata into buffer_locals.
        // Reader sites (renderer's link / anchor / highlights
        // lookups, do_help_follow_link, scroll-to-anchor) consume
        // the locals exclusively now that the struct fields are
        // gone.
        self.seed_help_metadata_locals(buffer_id, metadata);
        // M.4 (Option B): popup help buffers run `markdown-mode`
        // major + `help-mode` minor -- same activation
        // `open_help_in_pane` performs for the registry-tracked
        // copy. Drives the `pane_render_provider` lookup uniformly
        // across the popup and in-pane display strategies.
        self.activate_major_for_buffer_kind(buffer_id, BufferKind::Help);
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
        let HelpContent { buffer, metadata } = content;
        let buffer_id = buffer.id;
        // Drop any previous popup buffer cleanly before adopting
        // the new one (back-to-back hovers shouldn't pile up).
        self.dismiss_stale_popup_registry();
        // Popup buffers participate in `app.editor.buffers` like every
        // other buffer (same shape `open_popup` uses).
        self.editor
            .buffers
            .insert(crate::buffer_registry::BufferEntry {
                id: buffer_id,
                flags: crate::buffers::BufferFlags {
                    listed: false,
                    hidden: true,
                },
                data: crate::buffer_registry::BufferData::Help(buffer),
                name: None,
            });
        self.editor.popup_buffer = Some(buffer_id);
        self.editor.popup_placement = placement;
        self.seed_help_metadata_locals(buffer_id, metadata);
        // markdown-mode major + hover-mode minor. Markdown
        // carries the syntax pipeline; hover-mode adds the
        // auto-dismiss-on-doc-cursor-motion contract (and any
        // future hover-only behaviour). Help-mode is
        // intentionally NOT activated -- hover content is
        // markdown that may include fenced code, but its links
        // are typically external URLs we don't follow internally.
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self
            .editor
            .active_modes
            .remove(&buffer_id)
            .unwrap_or_default();
        let _ = self.editor.mode_registry.activate_major(
            &mut active,
            &self.editor.mode_guards,
            &self.editor.config,
            &self.editor.event_bus,
            &self.editor.services,
            proto_id,
            lattice_syntax::MarkdownMode::mode_id(),
            lattice_mode::CapabilitySet::empty(),
        );
        let _ = self.editor.mode_registry.activate_minor(
            &mut active,
            &self.editor.mode_guards,
            &self.editor.config,
            &self.editor.event_bus,
            &self.editor.services,
            proto_id,
            crate::modes::HoverMode::mode_id(),
            lattice_mode::CapabilitySet::empty(),
        );
        self.editor.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        // Crucially, NO active_buffer flip, NO prev_pane_for_help
        // capture, NO position-history push, NO cursor/scroll
        // load -- the document keeps focus. State A.
    }

    /// M.4 (b): clear out a popup buffer's registry / mode /
    /// option-cache state. Called by [`Self::open_popup`] before
    /// adopting a new popup (so back-to-back popups don't
    /// accumulate stale entries) and by [`Self::dismiss_popup`]
    /// when the popup closes. No-op when no popup is set.
    pub(super) fn dismiss_stale_popup_registry(&mut self) {
        self.editor.dismiss_stale_popup_registry();
    }

    /// M.4 (b): resolve the popup's `HelpBuffer` through the
    /// unified registry. The field stores only the `BufferId`; the
    /// actual buffer lives in `app.editor.buffers` with
    /// `BufferFlags { listed: false, hidden: true }`. Returns a
    /// cloned snapshot (the rope is cheap-to-clone); `None` when no
    /// popup is open or the registry entry has been torn down.
    pub fn popup_help(&self) -> Option<crate::help::HelpBuffer> {
        self.editor.popup_help()
    }

    /// Mutable counterpart to [`Self::popup_help`]. The closure
    /// runs under the registry lock; callers can mutate cursor /
    /// scroll on the active popup buffer in place.
    pub fn with_popup_help_mut<R>(
        &mut self,
        f: impl FnOnce(&mut crate::help::HelpBuffer) -> R,
    ) -> Option<R> {
        let id = self.editor.popup_buffer?;
        self.editor.buffers.with_help_mut(id, f)
    }

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
        self.editor.seed_help_metadata_locals(buffer_id, metadata);
    }

    /// M.3.2.c.5: read the parsed `[label](url)` links seeded into
    /// the active popup's buffer-locals. Returns `None` when no
    /// popup is open or the locals slot was not seeded.
    pub fn popup_help_links(&self) -> Option<&[crate::help::HelpLink]> {
        let id = self.editor.popup_buffer?;
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::HelpLinks>())
            .map(|h| h.0.as_slice())
    }

    /// M.3.2.c.5: read the named anchors (heading slugs +
    /// introspection-recorded anchors) seeded into the active
    /// popup's buffer-locals. Returns `None` when no popup is
    /// open or the locals slot was not seeded.
    pub fn popup_help_anchors(&self) -> Option<&[crate::help::HelpAnchor]> {
        let id = self.editor.popup_buffer?;
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::HelpAnchors>())
            .map(|h| h.0.as_slice())
    }

    /// M.3.2.c.5: read the pre-computed per-line markdown highlight
    /// spans seeded into the active popup's buffer-locals. Returns
    /// `None` when no popup is open or the locals slot was not
    /// seeded.
    pub fn popup_help_highlights(&self) -> Option<&[Vec<lattice_syntax::StyledSpan>]> {
        let id = self.editor.popup_buffer?;
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::HelpHighlights>())
            .map(|h| h.0.as_slice())
    }

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
        if self.editor.popup_buffer.is_some() {
            self.editor.popup_placement = placement;
        }
    }

    /// Snapshot the current popup's content + cursor + metadata so
    /// it can be restored later by `<C-o>`. Returns `None` if no
    /// popup is open or the registry entry has been torn down.
    pub(super) fn snapshot_current_popup(&self) -> Option<PopupSnapshot> {
        let id = self.editor.popup_buffer?;
        let (title, content) = self
            .editor
            .buffers
            .with_help(id, |buf| (buf.title.clone(), buf.content.clone()))?;
        let locals = self.editor.buffer_locals.get(&id)?;
        let metadata = HelpMetadata {
            links: locals
                .get::<crate::modes::HelpLinks>()
                .map(|h| h.0.clone())
                .unwrap_or_default(),
            anchors: locals
                .get::<crate::modes::HelpAnchors>()
                .map(|h| h.0.clone())
                .unwrap_or_default(),
            highlights: locals
                .get::<crate::modes::HelpHighlights>()
                .map(|h| h.0.clone())
                .unwrap_or_default(),
        };
        Some(lattice_host::popup::PopupSnapshot {
            title,
            content,
            cursor: self.editor.cursor,
            scroll: self.editor.scroll,
            metadata,
            placement: self.editor.popup_placement,
        })
    }

    /// Swap `content` into the existing popup buffer in place. Reuses
    /// the current `popup_buffer` id so position-history entries,
    /// marks, and cross-buffer features keep working coherently
    /// across in-popup navigation. Updates the buffer's rope,
    /// title, cursor, scroll, placement, and the
    /// `links`/`anchors`/`highlights` buffer-locals.
    pub(super) fn swap_popup_content(&mut self, content: HelpContent, placement: PopupPlacement) {
        let Some(id) = self.editor.popup_buffer else {
            return;
        };
        let HelpContent {
            buffer: new_buf,
            metadata,
        } = content;
        // Update the registered HelpBuffer in place. We retain `id`
        // (the existing popup's id) -- not `new_buf.id` -- so every
        // outer-state slot keyed on the popup id stays coherent.
        self.editor.buffers.with_help_mut(id, |existing| {
            existing.title = new_buf.title;
            existing.content = new_buf.content;
            existing.scroll = 0;
            existing.cursor = lattice_protocol::position::Position::ZERO;
        });
        self.editor.cursor = lattice_protocol::position::Position::ZERO;
        self.editor.scroll = 0;
        self.editor.popup_placement = placement;
        self.seed_help_metadata_locals(id, metadata);
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
    pub fn active_popup_placement(&self) -> Option<PopupPlacement> {
        self.editor
            .popup_buffer
            .map(|_| self.editor.popup_placement)
    }

    /// Close the popup. Drops the popup's content slot, resets
    /// placement to default, and restores any focus state that
    /// was captured at open. Idempotent: closing when no popup
    /// is open is a no-op.
    pub(crate) fn dismiss_popup(&mut self) {
        self.editor.dismiss_popup();
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
            .with_entry(id, |entry| (entry.flags, entry.help().is_some()))
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
