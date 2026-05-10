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
