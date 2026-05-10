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
/// - Height: `min(buffer_height * 3 / 4, 40)`, floor 5. Three-
///   quarters leaves a strip of the underlying buffer visible at
///   top and bottom; the 40-row ceiling keeps the popup
///   navigable (longer help docs scroll within the popup).
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
            let max_h = ((buffer_height as u32 * 3 / 4).max(5).min(40)) as u16;
            let max_w = (buffer_width.saturating_sub(4)).min(120).max(30);
            (max_h, max_w)
        }
        PopupPlacement::CursorAnchored => {
            let max_h = (buffer_height / 2).max(5).min(20);
            let max_w = (buffer_width.saturating_sub(4)).clamp(30, 80);
            (max_h, max_w)
        }
    };
    let height = (line_count.saturating_add(2)).min(max_h).max(5);
    (max_w, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_popup_uses_three_quarters_height_and_120_width_cap() {
        // 200 wide, 60 tall, 50 lines of content. Centered.
        let (w, h) = popup_outer_size(200, 60, 50, PopupPlacement::Centered);
        // Width caps at 120 (not buffer_width - 4 = 196).
        assert_eq!(w, 120);
        // Height caps at min(45, 40) = 40 (3/4 of 60 = 45 > 40).
        assert_eq!(h, 40);
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
        // 30-row buffer: 3/4 = 22.5 → 22; capped at 40 (not hit).
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
