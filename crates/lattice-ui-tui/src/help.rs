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
    /// Every `[[…]]` link in the rendered text, with its byte range
    /// inside `content` and its resolved target.
    pub links: Vec<HelpLink>,
}

impl std::fmt::Debug for HelpBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelpBuffer")
            .field("title", &self.title)
            .field("scroll", &self.scroll)
            .field("link_count", &self.links.len())
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
            links,
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

    pub fn scroll_down(&mut self, delta: usize, viewport: usize) {
        let total = self.line_count() as usize;
        let max = total.saturating_sub(viewport);
        self.scroll = (self.scroll + delta).min(max);
    }

    pub fn scroll_up(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_sub(delta);
    }

    pub fn jump_top(&mut self) {
        self.scroll = 0;
    }

    pub fn jump_bottom(&mut self, viewport: usize) {
        let total = self.line_count() as usize;
        self.scroll = total.saturating_sub(viewport);
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
    fn scroll_clamps_within_content() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.scroll_down(5, 10);
        assert_eq!(h.scroll, 5);
        h.scroll_down(1000, 10);
        assert_eq!(h.scroll, 10); // 20 lines - 10 viewport = 10 max
        h.scroll_up(3);
        assert_eq!(h.scroll, 7);
    }

    #[test]
    fn jump_bottom_lands_at_max_scroll() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpBuffer::from_lines("t", lines);
        h.jump_bottom(10);
        assert_eq!(h.scroll, 20);
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
