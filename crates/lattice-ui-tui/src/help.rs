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
//! 2. **Links are first-class, in standard markdown form** -- the
//!    formatter emits `[label](scheme:value)` markdown links and we
//!    extract a `Vec<HelpLink>` listing every reference's byte range
//!    (the LABEL, what the user sees) plus its target ([command,
//!    chord, source-location]). Standard markdown link syntax means
//!    a help body renders correctly in any markdown viewer (GitHub,
//!    docs.rs, this editor's markdown highlighter); navigation
//!    inside the editor dispatches on the URL's scheme.
//!
//! 3. **Display target is a user preference** -- [`HelpDisplayMode`]
//!    enumerates the surfaces a help buffer can be shown in. v1
//!    implements `Popup` only; `Split` / `Tab` / `Window` arrive
//!    behind multi-buffer.
//!
//! Markup convention for links inside a help body
//! (`[label](url)` -- standard markdown):
//!
//! - `[ex:write](command:ex:write)` -> [`HelpLinkTarget::Command`]
//! - `[zo](key:zo)`                 -> [`HelpLinkTarget::Chord`]
//! - `[src/foo.rs:42](file:src/foo.rs:42)` -> [`HelpLinkTarget::Source`]
//!
//! Anything else (`scheme:value` with an unrecognized scheme) parses
//! as an unresolved link with the raw URL preserved -- forward-compat
//! for future targets (option, event, mode, ...).

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::Buffer;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_syntax::{Lang, LangRegistry, Syntax};

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
    /// Pre-computed per-line markdown highlight spans. Populated by
    /// [`Self::with_markdown_syntax`]; empty when constructed
    /// without a language registry (test paths). The renderer
    /// indexes by line into this Vec, then applies the `Style`
    /// mapping for terminal styling. Pre-computing avoids a
    /// `&mut Syntax` borrow during the render pass which would
    /// otherwise conflict with `&App`.
    pub highlights: Vec<Vec<lattice_syntax::StyledSpan>>,
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
    /// Build a help buffer from a list of pre-formatted lines.
    /// Lines may contain `[label](scheme:value)` markdown links --
    /// the parser indexes them into [`HelpBuffer::links`] at the
    /// label's byte range in the joined output. No syntax
    /// highlighting is attached -- use [`Self::with_markdown_syntax`]
    /// to add it (the App always does; tests usually don't need to).
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
        let raw = lines.join("\n");
        // Strip the markdown link wrapper down to its label so the
        // user reads `ex:write` instead of `[ex:write](command:ex:write)`.
        // Links are indexed against the CLEANED text so cursor / scroll
        // / navigation all line up with what's on screen.
        let (text, links) = extract_links_and_clean(&raw);
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
            highlights: Vec::new(),
        }
    }

    /// Pre-compute markdown highlight spans for the entire body
    /// using the shared language registry. The help-overlay
    /// renderer reads `self.highlights[line]` per visible row and
    /// applies the `Style` mapping. Headings, fenced code blocks
    /// (with per-language injection -- a ` ```rust``` ` block
    /// carries rust highlights), and other markup land here.
    ///
    /// Builder-style so callers can chain:
    /// `HelpBuffer::from_lines(title, lines).with_markdown_syntax(registry)`.
    /// Failure to construct the syntax (e.g. registry doesn't have
    /// markdown registered) leaves `highlights` empty -- the buffer
    /// renders without color, no error.
    pub fn with_markdown_syntax(mut self, registry: Arc<LangRegistry>) -> Self {
        if let Ok(Some(mut s)) = Syntax::for_language_with_registry(Lang::Markdown, registry) {
            let text = self.content.as_string();
            s.parse(&text);
            let total_lines = self.content.line_count();
            if let Ok(rows) = s.highlight_lines(0, total_lines) {
                self.highlights = rows;
            }
        }
        self
    }

    /// Find the link whose label range contains `pos`. Used by the
    /// help-mode link-following handler -- when the user presses
    /// `<CR>` the App calls this on the cursor's position and
    /// dispatches based on the target's variant.
    pub fn link_at(&self, pos: Position) -> Option<&HelpLink> {
        self.links.iter().find(|link| {
            let r = &link.range;
            // Same-line check first (the common case).
            if pos.line == r.start.line && pos.line == r.end.line {
                return pos.byte >= r.start.byte && pos.byte < r.end.byte;
            }
            // Multi-line label (rare; still cover it).
            if pos.line < r.start.line || pos.line > r.end.line {
                return false;
            }
            if pos.line == r.start.line {
                return pos.byte >= r.start.byte;
            }
            if pos.line == r.end.line {
                return pos.byte < r.end.byte;
            }
            true
        })
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

/// Helper for help-content formatters. Renders a chord link in
/// standard markdown form: `[chord](key:chord)`.
pub fn key_link(chord: &str) -> String {
    format!("[{chord}](key:{chord})")
}

/// Helper for help-content formatters. Renders a command link in
/// standard markdown form: `[name](command:name)`.
pub fn command_link(name: &str) -> String {
    format!("[{name}](command:{name})")
}

/// Helper for help-content formatters. Renders a source link in
/// standard markdown form: `[path:line](file:path:line)`.
pub fn source_link(file_line: &str) -> String {
    format!("[{file_line}](file:{file_line})")
}

/// Strip every `[label](url)` markdown link in `text` down to just
/// its label and return the cleaned-up text plus a [`HelpLink`] per
/// link with its byte range computed against the CLEANED text. This
/// is what the help-buffer constructor uses so the user reads
/// `ex:write` instead of `[ex:write](command:ex:write)`. The link's
/// URL still drives navigation -- it's stored on the returned
/// [`HelpLink::target`] but the URL bytes don't appear in the
/// rendered output.
pub fn extract_links_and_clean(text: &str) -> (String, Vec<HelpLink>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut links: Vec<HelpLink> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Try to match `[label](url)` starting at i. On any
            // failure (no `]`, no `(`, no `)`) fall through and copy
            // the `[` byte literally.
            let label_start = i + 1;
            if let Some(label_end_rel) = bytes[label_start..].iter().position(|&b| b == b']')
                && bytes.get(label_start + label_end_rel + 1) == Some(&b'(')
            {
                let label_end = label_start + label_end_rel;
                let url_start = label_end + 2;
                if let Some(url_end_rel) = bytes[url_start..].iter().position(|&b| b == b')') {
                    let url_end = url_start + url_end_rel;
                    let label = &text[label_start..label_end];
                    let url = &text[url_start..url_end];
                    let target = classify_link_url(url);
                    let label_byte_start = out.len();
                    out.push_str(label);
                    let label_byte_end = out.len();
                    let start_pos = byte_offset_to_position(&out, label_byte_start);
                    let end_pos = byte_offset_to_position(&out, label_byte_end);
                    links.push(HelpLink {
                        range: ProtoRange::new(start_pos, end_pos),
                        target,
                    });
                    i = url_end + 1;
                    continue;
                }
            }
        }
        // Copy one UTF-8 codepoint.
        let ch_end = next_char_boundary(text, i);
        out.push_str(&text[i..ch_end]);
        i = ch_end;
    }
    (out, links)
}

fn next_char_boundary(s: &str, byte: usize) -> usize {
    let mut j = byte + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// Walk `text`, locating every `[label](url)` markdown link and
/// resolving the URL's scheme into a typed [`HelpLinkTarget`]. Each
/// returned [`HelpLink`]'s `range` covers the LABEL bytes (what the
/// user sees as a clickable token) -- the surrounding `[`, `]`,
/// `(`, `)`, and URL bytes aren't part of the highlighted range.
///
/// Unlike [`extract_links_and_clean`] this preserves the input text
/// verbatim and returns ranges in the ORIGINAL text. Useful when the
/// caller wants to keep the markdown source visible (markdown editor
/// mode); the help-buffer constructor uses `extract_links_and_clean`
/// to render labels-only.
///
/// Forms recognized:
/// - `[label](command:NAME)` -> [`HelpLinkTarget::Command`]
/// - `[label](key:CHORD)`    -> [`HelpLinkTarget::Chord`]
/// - `[label](file:PATH:LINE)` -> [`HelpLinkTarget::Source`]
/// - any other URL -> [`HelpLinkTarget::Unresolved`]
///
/// The parser is intentionally simple (no nested-bracket support,
/// no escaping). Help-content authors compose links via the
/// helper functions [`command_link`] / [`key_link`] /
/// [`source_link`] which always emit well-formed input.
pub fn parse_help_links(text: &str) -> Vec<HelpLink> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        // Find `]` after the `[`.
        let label_start = i + 1;
        let Some(label_end_rel) = bytes[label_start..].iter().position(|&b| b == b']') else {
            i += 1;
            continue;
        };
        let label_end = label_start + label_end_rel;
        // Must be followed by `(`.
        if bytes.get(label_end + 1) != Some(&b'(') {
            i = label_start;
            continue;
        }
        let url_start = label_end + 2;
        let Some(url_end_rel) = bytes[url_start..].iter().position(|&b| b == b')') else {
            i = url_start;
            continue;
        };
        let url_end = url_start + url_end_rel;

        let url = &text[url_start..url_end];
        let target = classify_link_url(url);
        let start_pos = byte_offset_to_position(text, label_start);
        let end_pos = byte_offset_to_position(text, label_end);
        out.push(HelpLink {
            range: ProtoRange::new(start_pos, end_pos),
            target,
        });
        i = url_end + 1;
    }
    out
}

fn classify_link_url(url: &str) -> HelpLinkTarget {
    if let Some(rest) = url.strip_prefix("command:") {
        HelpLinkTarget::Command(rest.to_string())
    } else if let Some(rest) = url.strip_prefix("key:") {
        HelpLinkTarget::Chord(rest.to_string())
    } else if let Some(rest) = url.strip_prefix("file:") {
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
        HelpLinkTarget::Source {
            path: PathBuf::from(rest),
            line: 0,
        }
    } else {
        HelpLinkTarget::Unresolved(url.to_string())
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
        let links = parse_help_links("see [ex:write](command:ex:write) for details");
        assert_eq!(links.len(), 1);
        assert!(matches!(
            &links[0].target,
            HelpLinkTarget::Command(s) if s == "ex:write"
        ));
    }

    #[test]
    fn parse_help_links_extracts_chord_link() {
        let links = parse_help_links("press [<C-d>](key:<C-d>) to scroll");
        assert_eq!(links.len(), 1);
        assert!(matches!(
            &links[0].target,
            HelpLinkTarget::Chord(s) if s == "<C-d>"
        ));
    }

    #[test]
    fn parse_help_links_extracts_source_link() {
        let links = parse_help_links("source: [src/foo.rs:42](file:src/foo.rs:42)");
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
        let links = parse_help_links("see [editor.line-numbers](option:editor.line-numbers)");
        assert_eq!(links.len(), 1);
        assert!(matches!(
            &links[0].target,
            HelpLinkTarget::Unresolved(s) if s == "option:editor.line-numbers"
        ));
    }

    #[test]
    fn parse_help_links_handles_multiple_on_one_line() {
        let links = parse_help_links("[a](command:a) and [b](key:b)");
        assert_eq!(links.len(), 2);
        assert!(matches!(&links[0].target, HelpLinkTarget::Command(s) if s == "a"));
        assert!(matches!(&links[1].target, HelpLinkTarget::Chord(s) if s == "b"));
    }

    #[test]
    fn parse_help_links_unmatched_bracket_is_ignored() {
        let links = parse_help_links("see [command](command:no-close");
        // No closing `)` -- ignored.
        assert!(links.is_empty());
    }

    #[test]
    fn parse_help_links_label_only_is_ignored() {
        // Markdown link requires `(url)` after the label; a bare
        // `[label]` (reference-style markdown) is currently unused in
        // help bodies and gets ignored by the parser.
        let links = parse_help_links("see [foo] for details");
        assert!(links.is_empty());
    }

    #[test]
    fn parse_help_links_records_byte_positions_across_lines() {
        let text = "first\n[x](command:x)\nthird";
        let links = parse_help_links(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].range.start.line, 1);
        // The label `x` starts at byte 1 on line 1 (after the `[`).
        assert_eq!(links[0].range.start.byte, 1);
    }

    #[test]
    fn link_helpers_emit_standard_markdown() {
        assert_eq!(command_link("ex:write"), "[ex:write](command:ex:write)");
        assert_eq!(key_link("zo"), "[zo](key:zo)");
        assert_eq!(
            source_link("src/foo.rs:42"),
            "[src/foo.rs:42](file:src/foo.rs:42)"
        );
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
        assert_eq!(key_link("<C-d>"), "[<C-d>](key:<C-d>)");
    }

    #[test]
    fn command_link_helper_renders_markup() {
        assert_eq!(command_link("ex:write"), "[ex:write](command:ex:write)");
    }

    #[test]
    fn display_mode_default_is_popup() {
        assert_eq!(HelpDisplayMode::default(), HelpDisplayMode::Popup);
    }

    #[test]
    fn with_markdown_syntax_populates_highlights_for_headings() {
        let registry = LangRegistry::standard().expect("registry");
        let h = HelpBuffer::from_lines("t", vec!["# Configuration".into(), "body line".into()])
            .with_markdown_syntax(registry);
        // Line 0 (the heading) should carry a Heading1 span.
        assert!(
            h.highlights
                .first()
                .map(|spans| spans
                    .iter()
                    .any(|sp| sp.style == lattice_syntax::Style::Heading1))
                .unwrap_or(false),
            "expected Heading1 span on heading line, got {:?}",
            h.highlights.first()
        );
    }

    #[test]
    fn with_markdown_syntax_is_optional() {
        // The fallback path -- no registry means no highlights, but
        // the buffer still works.
        let h = HelpBuffer::from_lines("t", vec!["# title".into()]);
        assert!(h.highlights.is_empty());
        assert_eq!(h.content.line_count(), 1);
    }
}
