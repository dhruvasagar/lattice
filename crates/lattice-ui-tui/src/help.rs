//! Buffer-backed help model (DESIGN.md §5.11).
//!
//! Help is a *buffer* with introspection-collected content -- the
//! same underlying type that holds source code. The popup overlay we
//! render today is just one display strategy for this buffer; when
//! multi-buffer support lands the same content can be shown in a
//! split, tab, or window per a user preference (see
//! [`HelpDisplayMode`]). This is the emacs model: `*Help*` is a
//! buffer; its content is queryable, navigable with normal motions,
//! and its links are followable.
//!
//! Three architectural commitments are baked in here even though the
//! v1 surface only renders the popup:
//!
//! 1. **Content is a `lattice_core::Buffer`** -- rope-backed, the
//!    same shape as a code buffer. When the help-major-mode + tree-
//!    sitter grammar lands (Phase 6+8), motions and the highlighter
//!    work over this content with no special-casing.
//!
//! 2. **Links are first-class** -- the formatter emits `[[…]]` markup
//!    and we extract a `Vec<HelpLink>` listing every reference's byte
//!    range within the rendered text plus its target ([command,
//!    chord, source-location]). Today the renderer is dumb and the
//!    links are inert; tomorrow's link-following motion uses this
//!    same vec.
//!
//! 3. **Display target is a user preference** -- [`HelpDisplayMode`]
//!    enumerates the surfaces a help buffer can be shown in. v1
//!    implements `Popup` only; `Split` / `Tab` / `Window` arrive
//!    behind multi-buffer.
//!
//! Markup convention for links inside a help body:
//!
//! - `[[command:NAME]]` -> [`HelpLinkTarget::Command`]
//! - `[[key:CHORD]]`    -> [`HelpLinkTarget::Chord`]
//! - `[[file:PATH:LINE]]` -> [`HelpLinkTarget::Source`]
//!
//! Anything else inside `[[ ]]` parses as an unresolved link with the
//! raw payload preserved -- forward-compat for future targets
//! (option, event, mode, ...).

use std::path::PathBuf;

use lattice_core::Buffer;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};

/// Where a help buffer is displayed. Configured per-user; v1 only
/// implements [`HelpDisplayMode::Popup`]. The other variants exist now
/// to lock in the API shape -- when multi-buffer support arrives the
/// renderer dispatches on this enum without touching the help-content
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelpDisplayMode {
    /// Centred bordered overlay over the buffer area. Available today.
    #[default]
    Popup,
    /// Horizontal split below the active buffer. Post-multi-buffer.
    Split,
    /// Separate tab in the tab bar. Post-multi-buffer.
    Tab,
    /// Separate OS-level window. Post-multi-buffer.
    Window,
}

/// One open help buffer. The content is a real [`Buffer`] (rope-
/// backed), so it composes with everything else that consumes
/// `Buffer` -- search, motions, syntax highlighting (once a help
/// major mode + tree-sitter grammar lands).
pub struct HelpBuffer {
    pub title: String,
    pub content: Buffer,
    /// First visible line index (the popup renderer uses this; a
    /// future split/tab/window renderer would use the buffer's own
    /// scroll state instead).
    pub scroll: usize,
    /// Cursor position inside the help content. The help overlay
    /// behaves like any other buffer -- motions move this cursor
    /// and `scroll` auto-adjusts to keep it in view. The terminal
    /// cursor is rendered at the screen translation of this
    /// position.
    pub cursor: Position,
    /// Every `[[…]]` link in the rendered text, with its byte range
    /// inside `content` and its resolved target.
    pub links: Vec<HelpLink>,
    /// Named anchors recorded by the introspection renderer
    /// (DESIGN.md §5.11). Convention: `kind:name`
    /// (`arg:path`, `args`, `section:examples`). Used by
    /// `scroll_to_anchor` and (post-Phase 6) by motion
    /// commands that walk anchor-by-anchor.
    pub anchors: Vec<HelpAnchor>,
}

/// Named scroll target inside a help buffer's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpAnchor {
    pub name: String,
    /// Line index within `HelpBuffer::content`.
    pub line: u32,
}

impl std::fmt::Debug for HelpBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelpBuffer")
            .field("title", &self.title)
            .field("scroll", &self.scroll)
            .field("cursor", &self.cursor)
            .field("link_count", &self.links.len())
            .field("anchor_count", &self.anchors.len())
            .field("line_count", &self.content.line_count())
            .finish()
    }
}

impl HelpBuffer {
    /// Build a help buffer from a list of pre-formatted lines. Each
    /// line may contain `[[…]]` link markup, which is preserved
    /// verbatim in the buffer text and indexed into [`HelpBuffer::links`]
    /// at the byte range it occupies in the joined output.
    pub fn from_lines(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self::from_lines_and_anchors(title, lines, Vec::new())
    }

    /// Build a help buffer with anchors. Used by the introspection
    /// renderer to feed `RenderedIntrospection.anchors` through.
    pub fn from_lines_and_anchors(
        title: impl Into<String>,
        lines: Vec<String>,
        anchors: Vec<HelpAnchor>,
    ) -> Self {
        let text = lines.join("\n");
        let links = parse_help_links(&text);
        let mut buffer = Buffer::empty();
        if !text.is_empty() {
            // One-shot fill of the rope. We ignore the AppliedEdit
            // since this is a fresh empty buffer; insertion at the
            // origin always succeeds.
            let _ = buffer.apply_edit(&Edit::insert(Position::ZERO, text));
        }
        Self {
            title: title.into(),
            content: buffer,
            scroll: 0,
            cursor: Position::ZERO,
            links,
            anchors,
        }
    }

    /// Scroll the help view so the named anchor's heading row is at
    /// the top of the visible region. Returns `true` if the anchor
    /// was found, `false` otherwise (caller can fall back to top).
    pub fn scroll_to_anchor(&mut self, name: &str) -> bool {
        if let Some(a) = self.anchors.iter().find(|a| a.name == name) {
            self.scroll = a.line as usize;
            true
        } else {
            false
        }
    }

    /// Number of visible content lines (the popup renderer uses this
    /// to clamp scroll). Equivalent to `content.line_count()`.
    pub fn line_count(&self) -> u32 {
        self.content.line_count()
    }

    /// Iterate the rendered lines top-down. Allocates -- `Buffer`
    /// doesn't expose per-line slicing yet. Acceptable for v1; the
    /// popup renderer only calls this on a small visible window.
    pub fn lines(&self) -> Vec<String> {
        self.content
            .as_string()
            .split('\n')
            .map(|s| s.to_string())
            .collect()
    }

    /// Move cursor by `dy` lines and `dx` bytes, then auto-scroll
    /// so the cursor stays visible in the given `viewport` rows.
    /// Negative deltas move up / left. Bytes clamp to the new
    /// line's length; lines clamp to `[0, line_count - 1]`.
    pub fn move_cursor(&mut self, dx: i32, dy: i32, viewport: usize) {
        let last_line = self.line_count().saturating_sub(1) as i32;
        let new_line = (self.cursor.line as i32 + dy).clamp(0, last_line) as u32;
        let line_len = self.line_byte_len(new_line);
        // h/l are line-only -- they don't wrap across newlines.
        let new_byte = (self.cursor.byte as i32 + dx).clamp(0, line_len as i32) as u32;
        self.cursor = Position::new(new_line, new_byte);
        self.adjust_scroll_to_cursor(viewport);
    }

    /// Jump cursor to a specific line. Bytes preserved if the
    /// new line is long enough; clamped to line end otherwise.
    pub fn jump_cursor_to(&mut self, line: u32, viewport: usize) {
        let last_line = self.line_count().saturating_sub(1);
        let target = line.min(last_line);
        let line_len = self.line_byte_len(target);
        self.cursor = Position::new(target, self.cursor.byte.min(line_len));
        self.adjust_scroll_to_cursor(viewport);
    }

    /// Jump cursor to the start of the current line (`0`).
    pub fn cursor_line_start(&mut self) {
        self.cursor.byte = 0;
    }

    /// Jump cursor to the end of the current line (`$`).
    pub fn cursor_line_end(&mut self) {
        let line_len = self.line_byte_len(self.cursor.line);
        // Match vim's `$`: cursor sits on the last char (byte
        // line_len-1), or at byte 0 on an empty line.
        self.cursor.byte = line_len.saturating_sub(1);
    }

    /// Auto-scroll so `cursor.line` is in view. If the cursor is
    /// already visible, scroll doesn't change.
    pub fn adjust_scroll_to_cursor(&mut self, viewport: usize) {
        if viewport == 0 {
            return;
        }
        let line = self.cursor.line as usize;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + viewport {
            self.scroll = line + 1 - viewport;
        }
    }

    /// Jump cursor to the top of the buffer (`gg`).
    pub fn jump_top(&mut self) {
        self.scroll = 0;
        self.cursor = Position::ZERO;
    }

    /// Jump cursor to the bottom of the buffer (`G`).
    pub fn jump_bottom(&mut self, viewport: usize) {
        let last_line = self.line_count().saturating_sub(1);
        self.cursor = Position::new(last_line, 0);
        self.adjust_scroll_to_cursor(viewport);
    }

    /// Half-page down (`Ctrl-D` in vim). Moves cursor by `viewport / 2`
    /// lines and adjusts scroll.
    pub fn half_page_down(&mut self, viewport: usize) {
        let delta = (viewport / 2).max(1) as i32;
        self.move_cursor(0, delta, viewport);
    }

    /// Half-page up (`Ctrl-U`).
    pub fn half_page_up(&mut self, viewport: usize) {
        let delta = (viewport / 2).max(1) as i32;
        self.move_cursor(0, -delta, viewport);
    }

    /// Byte length of a given line (excluding the trailing newline).
    /// Returns 0 for out-of-range lines.
    fn line_byte_len(&self, line: u32) -> u32 {
        let s = self.content.as_string();
        s.split('\n')
            .nth(line as usize)
            .map(|l| l.len() as u32)
            .unwrap_or(0)
    }
}

/// One `[[…]]` link inside a help buffer's content. `range` is the
/// byte interval within the rendered text (NOT including the `[[`
/// `]]` delimiters -- the renderer can highlight just the inner text
/// or the full match depending on style).
#[derive(Debug, Clone)]
pub struct HelpLink {
    pub range: ProtoRange,
    pub target: HelpLinkTarget,
}

/// What a `[[…]]` link points at. Renderers / link-following motions
/// dispatch on this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLinkTarget {
    /// `[[command:NAME]]` -- re-dispatches `:describe-command NAME`.
    Command(String),
    /// `[[key:CHORD]]` -- re-dispatches `:describe-key CHORD`.
    Chord(String),
    /// `[[file:PATH:LINE]]` -- opens PATH at LINE.
    Source { path: PathBuf, line: u32 },
    /// `[[…]]` whose payload didn't match a known scheme. Preserved
    /// verbatim for forward-compat -- a plugin / future scheme can
    /// inspect the raw payload.
    Unresolved(String),
}

/// Helper for help-content formatters. Renders a chord-link.
pub fn key_link(chord: &str) -> String {
    format!("[[key:{chord}]]")
}

/// Helper for help-content formatters. Renders a command-link.
pub fn command_link(name: &str) -> String {
    format!("[[command:{name}]]")
}

/// Helper for help-content formatters. Renders a source-link.
pub fn source_link(file_line: &str) -> String {
    // file_line is conventionally "path:line"; we just reflect what
    // the caller passed and let parse_help_links validate.
    format!("[[file:{file_line}]]")
}

/// Walk `text`, locating every `[[…]]` link and resolving its target.
/// Byte offsets in the returned [`HelpLink`]s are 0-indexed within
/// `text`, treating `text` as a flat byte stream and computing the
/// `(line, byte_in_line)` pair lazily.
pub fn parse_help_links(text: &str) -> Vec<HelpLink> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // Find the closing `]]`.
            let inner_start = i + 2;
            let mut j = inner_start;
            while j + 1 < bytes.len() && !(bytes[j] == b']' && bytes[j + 1] == b']') {
                j += 1;
            }
            if j + 1 < bytes.len() && bytes[j] == b']' && bytes[j + 1] == b']' {
                let payload = &text[inner_start..j];
                let target = classify_link_payload(payload);
                let start_pos = byte_offset_to_position(text, inner_start);
                let end_pos = byte_offset_to_position(text, j);
                out.push(HelpLink {
                    range: ProtoRange::new(start_pos, end_pos),
                    target,
                });
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn classify_link_payload(payload: &str) -> HelpLinkTarget {
    if let Some(rest) = payload.strip_prefix("command:") {
        HelpLinkTarget::Command(rest.to_string())
    } else if let Some(rest) = payload.strip_prefix("key:") {
        HelpLinkTarget::Chord(rest.to_string())
    } else if let Some(rest) = payload.strip_prefix("file:") {
        // `path:line` -- split at the LAST `:` so paths with colons
        // (Windows drives, URLs) survive.
        if let Some((path, line)) = rest.rsplit_once(':')
            && let Ok(line) = line.parse::<u32>()
        {
            return HelpLinkTarget::Source {
                path: PathBuf::from(path),
                line,
            };
        }
        HelpLinkTarget::Unresolved(payload.to_string())
    } else {
        HelpLinkTarget::Unresolved(payload.to_string())
    }
}

/// Convert a flat byte offset in `text` into a `(line, byte_in_line)`
/// [`Position`]. Lines are split at `\n`; the byte index past EOL
/// projects onto the start of the next line.
fn byte_offset_to_position(text: &str, byte_offset: usize) -> Position {
    let mut line = 0u32;
    let mut last_nl = 0usize;
    let bytes = text.as_bytes();
    let stop = byte_offset.min(bytes.len());
    for (i, b) in bytes.iter().enumerate().take(stop) {
        if *b == b'\n' {
            line += 1;
            last_nl = i + 1;
        }
    }
    Position::new(line, (stop - last_nl) as u32)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn from_lines_and_anchors_stores_provided_anchors() {
        let h = HelpBuffer::from_lines_and_anchors(
            "t",
            vec!["heading".into(), "body".into()],
            vec![HelpAnchor {
                name: "section:foo".into(),
                line: 0,
            }],
        );
        assert_eq!(h.anchors.len(), 1);
        assert_eq!(h.anchors[0].name, "section:foo");
    }

    #[test]
    fn scroll_to_anchor_moves_to_recorded_line() {
        let mut h = HelpBuffer::from_lines_and_anchors(
            "t",
            (0..30).map(|i| format!("line {i}")).collect(),
            vec![HelpAnchor {
                name: "mid".into(),
                line: 15,
            }],
        );
        assert!(h.scroll_to_anchor("mid"));
        assert_eq!(h.scroll, 15);
    }

    #[test]
    fn scroll_to_unknown_anchor_returns_false_and_leaves_scroll_alone() {
        let mut h = HelpBuffer::from_lines_and_anchors("t", vec!["a".into(), "b".into()], vec![]);
        h.scroll = 1;
        assert!(!h.scroll_to_anchor("nope"));
        assert_eq!(h.scroll, 1);
    }

    #[test]
    fn from_lines_creates_buffer_without_anchors() {
        let h = HelpBuffer::from_lines("t", vec!["x".into()]);
        assert!(h.anchors.is_empty());
    }

    #[test]
    fn from_lines_round_trips_through_buffer() {
        let h = HelpBuffer::from_lines("t", vec!["one".into(), "two".into(), "three".into()]);
        assert_eq!(h.title, "t");
        assert_eq!(h.line_count(), 3);
        let lines = h.lines();
        assert_eq!(lines, vec!["one", "two", "three"]);
    }

    #[test]
    fn empty_lines_yield_empty_buffer() {
        let h = HelpBuffer::from_lines("t", vec![]);
        assert_eq!(h.line_count(), 1); // empty buffer reports one empty line
        assert!(h.links.is_empty());
    }

    #[test]
    fn parse_help_links_extracts_command_link() {
        let links = parse_help_links("see [[command:ex:write]] for details");
        assert_eq!(links.len(), 1);
        assert!(matches!(
            &links[0].target,
            HelpLinkTarget::Command(s) if s == "ex:write"
        ));
    }

    #[test]
    fn parse_help_links_extracts_chord_link() {
        let links = parse_help_links("press [[key:<C-d>]] to scroll");
        assert_eq!(links.len(), 1);
        assert!(matches!(
            &links[0].target,
            HelpLinkTarget::Chord(s) if s == "<C-d>"
        ));
    }

    #[test]
    fn parse_help_links_extracts_source_link() {
        let links = parse_help_links("source: [[file:src/foo.rs:42]]");
        assert_eq!(links.len(), 1);
        match &links[0].target {
            HelpLinkTarget::Source { path, line } => {
                assert_eq!(path, &PathBuf::from("src/foo.rs"));
                assert_eq!(*line, 42);
            }
            other => panic!("unexpected target: {other:?}"),
        }
    }

    #[test]
    fn parse_help_links_unknown_scheme_is_unresolved() {
        let links = parse_help_links("see [[option:editor.line-numbers]]");
        assert_eq!(links.len(), 1);
        assert!(matches!(
            &links[0].target,
            HelpLinkTarget::Unresolved(s) if s == "option:editor.line-numbers"
        ));
    }

    #[test]
    fn parse_help_links_handles_multiple_on_one_line() {
        let links = parse_help_links("[[command:a]] and [[key:b]]");
        assert_eq!(links.len(), 2);
        assert!(matches!(&links[0].target, HelpLinkTarget::Command(s) if s == "a"));
        assert!(matches!(&links[1].target, HelpLinkTarget::Chord(s) if s == "b"));
    }

    #[test]
    fn parse_help_links_unmatched_bracket_is_ignored() {
        let links = parse_help_links("see [[command:no-close");
        assert!(links.is_empty());
    }

    #[test]
    fn parse_help_links_records_byte_positions_across_lines() {
        let text = "first\n[[command:x]]\nthird";
        let links = parse_help_links(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].range.start.line, 1);
        // After "[[" on line 1 the inner payload starts at byte 2.
        assert_eq!(links[0].range.start.byte, 2);
    }

    #[test]
    fn move_cursor_down_within_viewport_does_not_scroll() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.move_cursor(0, 5, 10);
        assert_eq!(h.cursor.line, 5);
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn move_cursor_past_viewport_advances_scroll() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.move_cursor(0, 12, 10);
        assert_eq!(h.cursor.line, 12);
        // cursor at line 12 with viewport=10 -> scroll = 12 + 1 - 10 = 3.
        assert_eq!(h.scroll, 3);
    }

    #[test]
    fn move_cursor_clamps_to_last_line() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.move_cursor(0, 1000, 10);
        assert_eq!(h.cursor.line, 19);
        // 20 lines - viewport 10 -> max scroll = 10.
        assert_eq!(h.scroll, 10);
    }

    #[test]
    fn move_cursor_up_pulls_scroll_back() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.move_cursor(0, 19, 10);
        assert_eq!(h.scroll, 10);
        // Move cursor up to line 5; scroll should clamp to follow.
        h.move_cursor(0, -14, 10);
        assert_eq!(h.cursor.line, 5);
        assert_eq!(h.scroll, 5);
    }

    #[test]
    fn move_cursor_horizontal_clamps_to_line_length() {
        let h_start = HelpBuffer::from_lines("t", vec!["abc".into(), "xy".into()]);
        let mut h = h_start;
        h.move_cursor(2, 0, 10);
        assert_eq!(h.cursor.byte, 2);
        h.move_cursor(100, 0, 10); // clamp to line length (3)
        assert_eq!(h.cursor.byte, 3);
        h.move_cursor(-1000, 0, 10); // clamp to 0
        assert_eq!(h.cursor.byte, 0);
    }

    #[test]
    fn cursor_line_start_and_end_jump_within_line() {
        let mut h = HelpBuffer::from_lines("t", vec!["hello world".into()]);
        h.cursor_line_end();
        // vim `$` lands on the last char index.
        assert_eq!(h.cursor.byte, 10);
        h.cursor_line_start();
        assert_eq!(h.cursor.byte, 0);
    }

    #[test]
    fn jump_top_resets_cursor_and_scroll() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.move_cursor(0, 25, 10);
        assert_ne!(h.scroll, 0);
        h.jump_top();
        assert_eq!(h.cursor, Position::ZERO);
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn jump_bottom_lands_cursor_and_scroll() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.jump_bottom(10);
        assert_eq!(h.cursor.line, 29);
        // 30 lines, viewport 10 -> scroll = 30 - 10 = 20.
        assert_eq!(h.scroll, 20);
    }

    #[test]
    fn half_page_motions_move_by_viewport_half() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.half_page_down(10);
        assert_eq!(h.cursor.line, 5);
        h.half_page_down(10);
        assert_eq!(h.cursor.line, 10);
        h.half_page_up(10);
        assert_eq!(h.cursor.line, 5);
    }

    #[test]
    fn key_link_helper_renders_markup() {
        assert_eq!(key_link("<C-d>"), "[[key:<C-d>]]");
    }

    #[test]
    fn command_link_helper_renders_markup() {
        assert_eq!(command_link("ex:write"), "[[command:ex:write]]");
    }

    #[test]
    fn display_mode_default_is_popup() {
        assert_eq!(HelpDisplayMode::default(), HelpDisplayMode::Popup);
    }
}
