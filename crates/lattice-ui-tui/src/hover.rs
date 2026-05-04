//! Hover popup (DESIGN.md §5.9.6, §5.11.4).
//!
//! Displays a transient floating panel anchored at a buffer
//! position -- typically used to surface LSP hover responses,
//! type signatures, or doc strings without flipping into a
//! full help view. v1 status (B.3): the data type + the renderer
//! land now; real LSP-driven hover wiring arrives with Phase 4.
//! `:hover [text]` triggers a manual demonstration (useful for
//! testing the popup positioning + dismissal).
//!
//! The popup carries markdown body content; we run it through the
//! shared markdown highlighter (same path as `HelpBuffer`) so a
//! ` ```rust ``` ` fenced code block in a hover renders with rust
//! highlights.

use std::sync::Arc;

use lattice_protocol::position::Position;
use lattice_syntax::{Lang, LangRegistry, StyledSpan, Syntax};

/// One open hover popup. Anchor is in buffer coordinates; the
/// renderer translates to screen coordinates each frame so terminal
/// resizes / scrolls reposition the popup naturally.
#[derive(Debug, Clone)]
pub struct HoverPopup {
    /// Buffer position the hover targets. The popup floats just
    /// below this row, clamped to fit on screen.
    pub anchor: Position,
    /// Markdown source. Pre-rendered into [`Self::lines`] +
    /// [`Self::highlights`] at construction time so the renderer
    /// reads cheap.
    pub markdown: String,
    pub lines: Vec<String>,
    pub highlights: Vec<Vec<StyledSpan>>,
    /// First visible line index inside the popup. Mutated by the
    /// hover-focused keymap (`j` / `k` / `<C-d>` / `<C-u>` / `gg`
    /// / `G`) so long hover bodies are scrollable. Stays at 0
    /// for transient (unfocused) display.
    pub scroll: usize,
}

impl HoverPopup {
    pub fn new(anchor: Position, markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let lines: Vec<String> = markdown.split('\n').map(|s| s.to_string()).collect();
        Self {
            anchor,
            markdown,
            lines,
            highlights: Vec::new(),
            scroll: 0,
        }
    }

    /// Scroll by `delta` lines (negative = up). Clamps so the
    /// popup never scrolls past the last line.
    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.lines.len().saturating_sub(1);
        let new = (self.scroll as i32 + delta).max(0) as usize;
        self.scroll = new.min(max);
    }

    /// Jump to the first line.
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    /// Jump so the last line is visible.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self.lines.len().saturating_sub(1);
    }

    /// Pre-compute markdown highlights for the popup body. Same
    /// path as `HelpBuffer::with_markdown_syntax`. Failure to build
    /// a markdown `Syntax` (registry doesn't have markdown
    /// configured) leaves `highlights` empty -- popup still renders,
    /// just without colour.
    pub fn with_markdown_syntax(mut self, registry: Arc<LangRegistry>) -> Self {
        if let Ok(Some(mut s)) = Syntax::for_language_with_registry(Lang::Markdown, registry) {
            s.parse(&self.markdown);
            let total_lines = self.lines.len() as u32;
            if let Ok(rows) = s.highlight_lines(0, total_lines) {
                self.highlights = rows;
            }
        }
        self
    }

    /// Width of the widest line, capped at `max`. Drives popup
    /// sizing: the renderer picks `min(content_width, max)`.
    pub fn content_width(&self, max: u16) -> u16 {
        self.lines
            .iter()
            .map(|l| l.chars().count() as u16)
            .max()
            .unwrap_or(0)
            .min(max)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn new_splits_markdown_into_lines() {
        let h = HoverPopup::new(Position::ZERO, "first\nsecond\nthird");
        assert_eq!(h.lines.len(), 3);
        assert_eq!(h.lines[1], "second");
    }

    #[test]
    fn content_width_caps_at_max() {
        let h = HoverPopup::new(Position::ZERO, "short\nthis_is_a_longer_line\nsh");
        assert_eq!(h.content_width(100), 21);
        assert_eq!(h.content_width(10), 10);
    }

    #[test]
    fn empty_body_produces_one_empty_line() {
        let h = HoverPopup::new(Position::ZERO, "");
        assert_eq!(h.lines.len(), 1);
        assert_eq!(h.lines[0], "");
    }

    #[test]
    fn with_markdown_syntax_populates_highlights_for_headings() {
        let registry = LangRegistry::standard().expect("registry");
        let h = HoverPopup::new(Position::ZERO, "# Title\nbody").with_markdown_syntax(registry);
        assert!(
            h.highlights
                .first()
                .map(|spans| spans
                    .iter()
                    .any(|sp| sp.style == lattice_syntax::Style::Heading1))
                .unwrap_or(false),
            "expected Heading1 span on heading line"
        );
    }
}
