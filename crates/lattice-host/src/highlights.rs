//! Syntax highlight cache keys and helpers.
//! Renderer-agnostic; used by host-side Editor and renderers.

/// Cache key for visible-highlights. The renderer paints spans every
/// frame; the expensive `highlight_lines` walk only runs when this key
/// changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleHighlightsKey {
    pub snapshot_ptr: usize,
    pub syntax_text_version: u64,
    pub scroll: u32,
    pub viewport_height: u32,
    pub fold_hash: u64,
}
