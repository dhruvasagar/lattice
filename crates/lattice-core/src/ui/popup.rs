//! Popup overlay primitives.
//!
//! Today the only popup surface is the help-buffer overlay
//! (DESIGN.md §5.11). Even so, "where the popup sits on screen"
//! is a renderer concern -- not a help-content concern -- so the
//! placement enum lives here and is reused by any future popup
//! kind (inline diagnostic box, completion docs, signature side-
//! panel) without dragging in help-buffer machinery.

/// Where the popup overlay anchors on screen.
///
/// Cursor-anchored popups (hover, signature help, diagnostic-at-
/// cursor) sit adjacent to the symbol that triggered them so the
/// user's eye doesn't have to leave the cursor to read them.
/// Centred popups are command-launched and unrelated to where the
/// cursor happens to be (`:lsp-status`, `:describe-*`, `:apropos`,
/// `:help`, `:keymap`, `:options`, `:ls`, `:lsp-log`, ...) so
/// anchoring next to the cursor would produce a visually arbitrary
/// placement. Centring puts them where the eye lands by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PopupPlacement {
    /// Anchor adjacent to the document cursor (above / below
    /// depending on screen room).
    CursorAnchored,
    /// Centre over the buffer area.
    #[default]
    Centered,
    /// WK.5: full width of the active pane, anchored to its bottom
    /// edge. Which-key's placement.
    ///
    /// Bottom-anchored full-width is what emacs `which-key` and
    /// `which-key.nvim` both do, and per the UX-convention rule that
    /// muscle memory is the default worth keeping. It is also the only
    /// placement where the column count is predictable, which is what
    /// lets the grid be laid out ahead of the renderer.
    ///
    /// Height is content + border, hard-capped at half the pane so the
    /// hint can never swallow the buffer it is describing.
    MinibufferBand,
}

/// Whether opening a popup moves focus into it. Names the distinction
/// the code already makes between the two overlay entry points, so any
/// popup kind (help, ACP permission menu, LSP code-action list) picks a
/// focus mode without dragging in help-buffer machinery.
///
/// See `docs/dev/architecture/popup-api.md` §4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PopupFocus {
    /// Focus moves into the popup buffer: it becomes the active buffer,
    /// its major mode receives keys, and modal resets to Normal (State
    /// B). `:help`, `:describe-*`, `:apropos`, the permission menu.
    #[default]
    Steal,
    /// The popup floats over the active buffer, which keeps focus and
    /// the caret; modal and active-buffer are untouched (State A).
    /// Hover, signature help.
    Passive,
}

/// Outer size (border-inclusive) of a help / hover popup overlay.
///
/// Centred popups are reading surfaces (`:help`, `:options`,
/// `:describe-*`, `:apropos`, `:customize`); they want enough
/// room to lay out paragraphs + tables comfortably without
/// covering the whole screen. The buffer below stays partly
/// visible so the user keeps spatial context. Caps:
///
/// - Width: `min(buffer_width - 4, 120)`, floor 30. The 120-cell
///   ceiling preserves a comfortable line length for reading
///   markdown -- wider lines are harder to scan, so even on a
///   200-cell terminal the popup stops at 120.
/// - Height: `buffer_height * 3 / 4`, floor 5, no upper ceiling.
///   Three-quarters leaves a strip of the underlying buffer
///   visible at top and bottom while giving a large help doc all
///   the vertical room the screen allows; anything longer than the
///   box scrolls within the popup.
///
/// Cursor-anchored popups are tooltips (hover, signature help);
/// they sit adjacent to the cursor and want to *not* dominate
/// the screen. Caps stay tight: width 30..=80, height 5..=20.
///
/// `line_count` is the popup's content row count
/// (excluding borders). The returned `(width, height)` is
/// border-inclusive (`height = inner + 2`); subtract 2 to get
/// the inner viewport for motion / scroll.
pub fn popup_outer_size(
    buffer_width: u16,
    buffer_height: u16,
    line_count: u16,
    placement: PopupPlacement,
) -> (u16, u16) {
    let line_count = line_count.max(1);
    let (max_h, max_w) = match placement {
        PopupPlacement::Centered => {
            // Height is a true three-quarters of the viewport (floor 5, no upper
            // ceiling): a large help doc should use all the vertical room the
            // screen allows, while ¾ still leaves a strip of buffer above and
            // below for context. The old 40-row cap made the popup feel cramped
            // on tall / high-resolution displays — a 70-row screen stopped at 40
            // and scrolled the rest. The `.max(5)` keeps `max_h >= 5` so the
            // `clamp(5, max_h)` below never inverts on a tiny terminal.
            let max_h = ((buffer_height as u32 * 3 / 4).max(5)) as u16;
            let max_w = (buffer_width.saturating_sub(4)).clamp(30, 120);
            (max_h, max_w)
        }
        PopupPlacement::CursorAnchored => {
            let max_h = (buffer_height / 2).clamp(5, 20);
            let max_w = (buffer_width.saturating_sub(4)).clamp(30, 80);
            (max_h, max_w)
        }
        // WK.5: full pane width, and never more than half its height.
        // The half-pane cap is the hard one: a hint that covers the code
        // it describes has defeated itself, and a prefix with a hundred
        // continuations would otherwise ask for exactly that.
        PopupPlacement::MinibufferBand => {
            let max_h = (buffer_height / 2).max(1);
            (max_h, buffer_width.max(1))
        }
    };
    // A pane-bottom popup sizes to its content and has no floor: a
    // two-row hint is two rows. The 5-row floor is for reading surfaces,
    // where a box smaller than that reads as broken rather than terse.
    let height = if matches!(placement, PopupPlacement::MinibufferBand) {
        (line_count.saturating_add(2)).min(max_h)
    } else {
        (line_count.saturating_add(2)).clamp(5, max_h)
    };
    (max_w, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_bottom_is_full_width_and_sizes_to_its_content() {
        // 8 content rows in a 100x40 pane → full width, 10 rows.
        let (w, h) = popup_outer_size(100, 40, 8, PopupPlacement::MinibufferBand);
        assert_eq!(w, 100, "full pane width — the grid was laid out to it");
        assert_eq!(h, 10, "content + border, no 5-row floor padding it out");
    }

    #[test]
    fn pane_bottom_never_takes_more_than_half_the_pane() {
        // 100 rows of continuations in a 40-row pane.
        let (_w, h) = popup_outer_size(100, 40, 100, PopupPlacement::MinibufferBand);
        assert_eq!(
            h, 20,
            "a hint that covers the code it describes has defeated itself"
        );
    }

    #[test]
    fn a_two_row_pane_bottom_popup_stays_two_rows() {
        let (_w, h) = popup_outer_size(100, 40, 1, PopupPlacement::MinibufferBand);
        assert_eq!(
            h, 3,
            "one row plus border — the 5-row floor is for reading surfaces, \
             where a smaller box reads as broken rather than terse"
        );
    }

    #[test]
    fn centered_popup_uses_three_quarters_height_and_120_width_cap() {
        // 200 wide, 60 tall, 50 lines of content. Centered.
        let (w, h) = popup_outer_size(200, 60, 50, PopupPlacement::Centered);
        // Width caps at 120 (not buffer_width - 4 = 196).
        assert_eq!(w, 120);
        // Height is 3/4 of 60 = 45 — content (50+2) exceeds it, so it fills the
        // three-quarters. No 40-row ceiling anymore.
        assert_eq!(h, 45);
    }

    #[test]
    fn centered_popup_has_no_40_row_ceiling_on_tall_screens() {
        // A tall / high-resolution viewport: 3/4 of 100 = 75, and a long help
        // doc uses all of it rather than stopping at the old 40-row cap.
        let (_w, h) = popup_outer_size(200, 100, 200, PopupPlacement::Centered);
        assert_eq!(h, 75, "large docs fill three-quarters of a tall screen");
    }

    #[test]
    fn centered_popup_fits_short_content_to_content_height() {
        let (_w, h) = popup_outer_size(200, 60, 8, PopupPlacement::Centered);
        // 8 lines + 2 borders = 10, well under both caps.
        assert_eq!(h, 10);
    }

    #[test]
    fn centered_popup_floor_at_five_rows() {
        // Empty content still gets 5-row floor (3 inner).
        let (_w, h) = popup_outer_size(200, 60, 0, PopupPlacement::Centered);
        assert_eq!(h, 5);
    }

    #[test]
    fn cursor_anchored_keeps_tooltip_caps_unchanged() {
        // Same buffer, same content as the centered case.
        let (w, h) = popup_outer_size(200, 60, 50, PopupPlacement::CursorAnchored);
        // Width caps at 80, height at 20 -- tooltip ergonomics.
        assert_eq!(w, 80);
        assert_eq!(h, 20);
    }

    #[test]
    fn small_buffer_shrinks_centered_height_proportionally() {
        // 30-row buffer: 3/4 = 22.5 → 22. Content exceeds it, so it fills the
        // three-quarters.
        let (_w, h) = popup_outer_size(100, 30, 50, PopupPlacement::Centered);
        assert_eq!(h, 22);
    }

    #[test]
    fn narrow_buffer_shrinks_centered_width() {
        // 50-cell buffer: width = min(50-4, 120) = 46.
        let (w, _h) = popup_outer_size(50, 60, 50, PopupPlacement::Centered);
        assert_eq!(w, 46);
    }
}
