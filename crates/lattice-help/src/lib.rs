//! Buffer-backed help model (DESIGN.md §5.11).
//!
//! Help is a *buffer* with introspection-collected content -- the
//! same underlying type that holds source code. The popup overlay we
//! render today is just one display strategy for this buffer; when
//! multi-buffer support lands the same content can be shown in a
//! split, tab, or window per a user preference (see
//! `lattice_core::ui::display::BufferDisplay`). This is the emacs model: `*Help*` is a
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
//! 3. **Display target is a user preference** -- `BufferDisplay`
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

use lattice_core::BufferId;

pub mod topics;

// Display strategy for help-flavoured buffers (popup / split /
// active-pane / ...) lives in `lattice_core::ui::display` as
// [`BufferDisplay`] / [`BufferDisplayCategory`] -- one enum
// covers every dedicated-buffer producer (LSP status, hover,
// signature, the various help / describe / apropos surfaces),
// not just help. Callers in the App route through
// [`App::display_buffer`].

// `PopupPlacement` lives in `crate::popup`. The popup is a
// generic rendering surface (a rect drawn over the buffer area
// inside which any buffer can render); placement / anchoring is
// a property of the popup itself, not of whatever buffer happens
// to be inside it.

/// One open help buffer. The content is a real [`Buffer`] (rope-
/// backed), so it composes with everything else that consumes
/// `Buffer` -- search, motions, syntax highlighting (once a help
/// major mode + tree-sitter grammar lands).
///
/// Clone is cheap-ish: the rope clones in O(1), but the markdown
/// highlight `Vec<Vec<StyledSpan>>` allocates per-line. Used by
/// the registry-mirrored hot-path (`App::open_help_in_pane`) so
/// the durable record and the active hot-path slot stay in sync
/// at pane-switch boundaries.
/// M.3.2.c.5 (partial): the data fields `links` / `anchors` /
/// `highlights` remain present for now. Production code paths
/// flip to read them through `buffer_locals` (mirrored at
/// construction sites via `App::seed_help_metadata_locals`); the
/// vestigial struct fields keep ~70 unit tests that construct
/// `HelpBuffer` directly compiling without rewrites. The
/// fields' visible-to-the-renderer fallback paths are removed
/// in this slice -- production reads exclusively via locals --
/// so the in-struct state is now pure test-fixture leftover.
/// Final retirement of the fields waits until the test sites
/// migrate to seeding `BufferLocals` directly.
#[derive(Clone)]
pub struct HelpBuffer {
    /// Stable id assigned at creation. Position-history entries
    /// (§5.1.1) carry this so `<C-o>` / `<C-i>` can route back to
    /// the originating buffer when multiple Help buffers coexist
    /// (Phase B.1.c). v1 only ever holds one Help buffer at a
    /// time -- the id still matters because the position history
    /// outlives any one Help session and a stale entry must not
    /// land on a freshly-opened, unrelated Help.
    pub id: BufferId,
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
    /// Vestigial: `[label](url)` markdown links. Production reads
    /// route through `buffer_locals`; this field remains for
    /// tests that construct via `HelpBuffer::from_lines` and
    /// inspect parser output without going through the App seam.
    pub links: Vec<HelpLink>,
    /// Vestigial: named anchors. See `links` for the production
    /// vs. test split.
    pub anchors: Vec<HelpAnchor>,
    /// Vestigial: pre-computed per-line markdown highlight spans.
    pub highlights: Vec<Vec<lattice_syntax::StyledSpan>>,
}

/// Named scroll target inside a help buffer's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpAnchor {
    pub name: String,
    /// Line index within `HelpBuffer::content`.
    pub line: u32,
}

/// M.3.2.c.5: parsed-out metadata that travels alongside a
/// freshly-constructed [`HelpBuffer`]. Bundles the data the
/// help-mode owns per-buffer so the App can seed it into
/// `buffer_locals[id]` at popup-open time. Replaces the
/// `links` / `anchors` / `highlights` fields that used to live
/// directly on `HelpBuffer`.
#[derive(Debug, Clone, Default)]
pub struct HelpMetadata {
    /// `[label](url)` links extracted by the from_lines parser,
    /// indexed against the cleaned (post-link-strip) text.
    pub links: Vec<HelpLink>,
    /// Named anchors -- heading slugs auto-generated by
    /// [`generate_heading_anchors`], plus any explicit anchors
    /// supplied by the introspection renderer.
    pub anchors: Vec<HelpAnchor>,
    /// Per-line markdown highlight spans. Empty when the
    /// content was not enriched with [`compute_markdown_highlights`].
    pub highlights: Vec<Vec<lattice_syntax::StyledSpan>>,
}

/// M.3.2.c.5: pair of (slim help buffer, parsed metadata) returned
/// from every help factory. The App splits this into:
/// - `buffer` -> `App.popup_buffer` (the popup hot-path slot)
/// - `metadata` -> `App.buffer_locals[buffer.id]` via
///   `seed_help_metadata_locals` at popup-open time.
#[derive(Debug, Clone)]
pub struct HelpContent {
    pub buffer: HelpBuffer,
    pub metadata: HelpMetadata,
}

impl HelpContent {
    /// Builder-style override: enrich the metadata's `highlights`
    /// via the markdown grammar. Cheap when the registry doesn't
    /// have markdown registered (silent no-op). Also mirrors the
    /// computed spans onto the vestigial buffer field so any
    /// HelpBuffer-direct test path observes the same value.
    pub fn with_markdown_syntax(mut self, registry: Arc<LangRegistry>) -> Self {
        let highlights = compute_markdown_highlights(&self.buffer, registry);
        self.metadata.highlights = highlights.clone();
        self.buffer.highlights = highlights;
        self
    }
}

// Deref pattern: `HelpContent` is a transient construction value
// composed of (slim buffer, parsed metadata). Tests + a handful
// of legacy call sites still treat it like a `HelpBuffer`
// (`content.cursor`, `content.line_count()`, `content.scroll_to_anchor`,
// ...). Forwarding through `Deref` keeps those call sites clean
// without copying every method onto `HelpContent`. Production
// callers reach the metadata via `content.metadata` directly;
// the App's `open_popup` consumes the whole struct by value.
impl std::ops::Deref for HelpContent {
    type Target = HelpBuffer;
    fn deref(&self) -> &HelpBuffer {
        &self.buffer
    }
}

impl std::ops::DerefMut for HelpContent {
    fn deref_mut(&mut self) -> &mut HelpBuffer {
        &mut self.buffer
    }
}

/// Parse `lines` into a help buffer + metadata. Walks the joined
/// text once, stripping `[label](url)` markdown links down to
/// their visible labels and indexing each link's range against
/// the cleaned text. The buffer's content is the cleaned text;
/// links land on the metadata.
pub fn parse_help_lines(
    title: impl Into<String>,
    lines: Vec<String>,
) -> HelpContent {
    parse_help_lines_and_anchors(title, lines, Vec::new())
}

/// Parse `lines` + explicit `anchors` into a help buffer + metadata.
pub fn parse_help_lines_and_anchors(
    title: impl Into<String>,
    lines: Vec<String>,
    anchors: Vec<HelpAnchor>,
) -> HelpContent {
    let raw = lines.join("\n");
    let (text, links) = extract_links_and_clean(&raw);
    let mut buffer = Buffer::empty();
    if !text.is_empty() {
        let _ = buffer.apply_edit(&Edit::insert(Position::ZERO, text));
    }
    let metadata = HelpMetadata {
        links: links.clone(),
        anchors: anchors.clone(),
        highlights: Vec::new(),
    };
    HelpContent {
        buffer: HelpBuffer {
            id: BufferId::next(),
            title: title.into(),
            content: buffer,
            scroll: 0,
            cursor: Position::ZERO,
            // Vestigial mirror -- populated for tests that read
            // these fields directly. Production reads route through
            // `HelpContent::metadata` -> `buffer_locals`.
            links,
            anchors,
            highlights: Vec::new(),
        },
        metadata,
    }
}

/// Compute per-line markdown highlight spans for `buffer` via the
/// shared language registry. Failure to construct the markdown
/// syntax (registry doesn't have it, parse error) returns an
/// empty Vec -- the renderer treats that as "no syntax colours,
/// no error".
pub fn compute_markdown_highlights(
    buffer: &HelpBuffer,
    registry: Arc<LangRegistry>,
) -> Vec<Vec<lattice_syntax::StyledSpan>> {
    if let Ok(Some(mut s)) = Syntax::for_language_with_registry(Lang::Markdown, registry) {
        let text = buffer.content.as_string();
        s.parse(&text);
        let total_lines = buffer.content.line_count();
        if let Ok(rows) = s.highlight_lines(0, total_lines) {
            return rows;
        }
    }
    Vec::new()
}

/// Find the metadata link whose label range contains `pos`.
pub fn link_at(links: &[HelpLink], pos: Position) -> Option<&HelpLink> {
    links.iter().find(|link| {
        let r = &link.range;
        if pos.line == r.start.line && pos.line == r.end.line {
            return pos.byte >= r.start.byte && pos.byte < r.end.byte;
        }
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

/// Look up an anchor by name and return the line it points at.
pub fn anchor_line(anchors: &[HelpAnchor], name: &str) -> Option<u32> {
    anchors.iter().find(|a| a.name == name).map(|a| a.line)
}

impl std::fmt::Debug for HelpBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelpBuffer")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("scroll", &self.scroll)
            .field("cursor", &self.cursor)
            .field("line_count", &self.content.line_count())
            .finish()
    }
}

impl HelpContent {
    /// Build a help buffer from a list of pre-formatted lines.
    /// Lines may contain `[label](scheme:value)` markdown links --
    /// the parser indexes them into the returned metadata's `links`
    /// vec at the label's byte range in the joined output. No syntax
    /// highlighting is attached -- chain
    /// [`Self::with_markdown_syntax`] (the App always does; tests
    /// usually don't need to).
    pub fn from_lines(title: impl Into<String>, lines: Vec<String>) -> Self {
        parse_help_lines(title, lines)
    }

    /// Build with explicit anchors. Used by the introspection
    /// renderer to feed `RenderedIntrospection.anchors` through.
    pub fn from_lines_and_anchors(
        title: impl Into<String>,
        lines: Vec<String>,
        anchors: Vec<HelpAnchor>,
    ) -> Self {
        parse_help_lines_and_anchors(title, lines, anchors)
    }

}

impl HelpBuffer {
    /// Find the link whose label range contains `pos`. Reads
    /// the vestigial `links` field; production callers go
    /// through `buffer_locals[id].get::<HelpLinks>()` instead.
    pub fn link_at(&self, pos: Position) -> Option<&HelpLink> {
        link_at(&self.links, pos)
    }

    /// Scroll to a named anchor. Reads the vestigial `anchors`
    /// field; production callers go through
    /// `buffer_locals[id].get::<HelpAnchors>()` instead.
    pub fn scroll_to_anchor(&mut self, name: &str) -> bool {
        if let Some(line) = anchor_line(&self.anchors, name) {
            self.scroll = line as usize;
            true
        } else {
            false
        }
    }

    /// Test-only convenience: enrich vestigial `highlights` via
    /// the markdown grammar. Production callers should chain
    /// [`HelpContent::with_markdown_syntax`] instead so the
    /// highlights also land on the parsed metadata that gets
    /// seeded into `buffer_locals`.
    pub fn with_markdown_syntax(mut self, registry: Arc<LangRegistry>) -> Self {
        self.highlights = compute_markdown_highlights(&self, registry);
        self
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
    /// `[[help:TOPIC]]` -- re-dispatches `:help TOPIC`. Used by
    /// `:describe-*` cross-references and by topic body content
    /// itself so a topic can link to a sibling topic.
    Topic(String),
    /// `[label](#slug)` -- intra-document jump. Auto-generated from
    /// markdown headings (GitHub-style slug: lowercase, non-alnum
    /// runs collapsed to `-`, leading/trailing `-` trimmed). The
    /// follow-link handler scrolls the *current* help buffer to
    /// the matching anchor's line; no buffer swap.
    Anchor(String),
    /// `[label](exec:CMDLINE)` -- *executes* the cmdline as if the
    /// user had typed `:CMDLINE<CR>`. Distinct from
    /// [`Self::Command`] which describes the command instead of
    /// running it. Used by picker-style help buffers (e.g.
    /// `:lsp-server-log`) where each row's link should fire the
    /// real command on Enter, not surface its docs.
    ///
    /// The payload is the *full* cmdline (command + args, no
    /// leading colon). Multi-arg commands like `lsp-log rust`
    /// pass through verbatim.
    Execute(String),
    /// `[label](customize:NAME)` -- re-dispatches `:customize NAME`
    /// (M.9.1). Used by the customize picker so each group / mode
    /// row in the no-args view follows to its own focused buffer
    /// on `<CR>`.
    Customize(String),
    /// `[label](customize-edit:NAME)` -- prefills the cmdline
    /// with `:set NAME=<current-value>` and enters Command
    /// mode (M.9.2). Used by the customize buffer's per-row
    /// links so `<CR>` on an option row opens an inline edit.
    /// The actual write goes through the existing `:set`
    /// machinery, so validation, cascade, and event-bus
    /// publishing all run unchanged.
    CustomizeEdit(String),
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

/// Helper for help-content formatters. Renders a topic link in
/// standard markdown form: `[name](help:name)`. Used by
/// `:describe-*` cross-references.
pub fn topic_link(name: &str) -> String {
    format!("[{name}](help:{name})")
}

/// Strip every `[label](url)` markdown link in `text` down to just
/// its label and return the cleaned-up text plus a [`HelpLink`] per
/// link with its byte range computed against the CLEANED text. This
/// is what the help-buffer constructor uses so the user reads
/// `ex:write` instead of `[ex:write](command:ex:write)`. The link's
/// URL still drives navigation -- it's stored on the returned
/// [`HelpLink::target`] but the URL bytes don't appear in the
/// rendered output.
/// Collapse a multi-line diagnostic message to a single line.
/// LSP messages can contain newlines (e.g. rust-analyzer's
/// "expected `Foo`, found `Bar`\n  -- in fn::method"). The
/// help-buffer's row layout assumes one row per entry; squash
/// to keep visual alignment.
pub fn one_line(s: &str) -> String {
    s.lines().collect::<Vec<_>>().join(" / ")
}


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
    } else if let Some(rest) = url.strip_prefix("exec:") {
        // `[label](exec:CMDLINE)` -- runs `:CMDLINE` on Enter.
        // Distinct from `command:` which describes the command.
        HelpLinkTarget::Execute(rest.to_string())
    } else if let Some(rest) = url.strip_prefix("key:") {
        HelpLinkTarget::Chord(rest.to_string())
    } else if let Some(rest) = url.strip_prefix("help:") {
        HelpLinkTarget::Topic(rest.to_string())
    } else if let Some(rest) = url.strip_prefix("customize-edit:") {
        // Order: `customize-edit:` must precede `customize:`
        // because both share the leading prefix.
        HelpLinkTarget::CustomizeEdit(rest.to_string())
    } else if let Some(rest) = url.strip_prefix("customize:") {
        HelpLinkTarget::Customize(rest.to_string())
    } else if let Some(rest) = url.strip_prefix('#') {
        // Markdown intra-document anchor (`[label](#slug)`). Matches
        // the GitHub-style slug auto-generated from headings by
        // [`generate_heading_anchors`].
        HelpLinkTarget::Anchor(rest.to_string())
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

/// Convert a markdown heading line ("## 1. Tree-sitter, core") into
/// the GitHub-style slug ("1-tree-sitter-core") used for intra-doc
/// anchor links. Algorithm:
///
/// 1. Strip the leading `#`s + any whitespace.
/// 2. Lowercase.
/// 3. Drop any non-alphanumeric / non-hyphen / non-space character
///    (punctuation, fences, parens, periods, etc.).
/// 4. Collapse whitespace runs to a single hyphen; collapse hyphen
///    runs to a single hyphen.
/// 5. Trim leading / trailing hyphens.
///
/// Matches the slugs GitHub renders for `# Heading` blocks so links
/// authored against rendered docs work in-editor too.
pub fn slugify_heading(text: &str) -> String {
    let mut s = text.trim().to_lowercase();
    // Strip leading `#`s + whitespace.
    s = s.trim_start_matches('#').trim_start().to_string();
    let mut out = String::with_capacity(s.len());
    let mut prev_hyphen = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_hyphen = false;
        } else if (ch == '-' || ch.is_whitespace()) && !prev_hyphen && !out.is_empty() {
            out.push('-');
            prev_hyphen = true;
        }
        // Anything else (punctuation, backticks, parens, slashes...)
        // is dropped, mirroring GitHub.
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Walk `lines` for ATX-style markdown headings (`#`, `##`, ...) and
/// emit a [`HelpAnchor`] per heading whose name is the GitHub-style
/// slug. Used by the help-topic loader so authors can write
/// `[label](#slug)` in markdown bodies and have the link route in-
/// editor without manually-managed anchor lists.
///
/// Skips heading-shaped lines inside fenced code blocks
/// (` ``` ` / ` ~~~ `) so a `# foo` line in a Rust example doesn't
/// register as an anchor.
pub fn generate_heading_anchors(lines: &[String]) -> Vec<HelpAnchor> {
    let mut anchors = Vec::new();
    let mut in_fence = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !trimmed.starts_with('#') {
            continue;
        }
        // Count leading `#`s; ATX cap is 6.
        let depth = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&depth) {
            continue;
        }
        // Require at least one whitespace between hashes and content
        // (CommonMark §4.2). Bare `###foo` is not a heading.
        let after = &trimmed[depth..];
        if !after.is_empty() && !after.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let slug = slugify_heading(trimmed);
        if slug.is_empty() {
            continue;
        }
        anchors.push(HelpAnchor {
            name: slug,
            line: i as u32,
        });
    }
    anchors
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
        let h = HelpContent::from_lines_and_anchors(
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
        let mut h = HelpContent::from_lines_and_anchors(
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
        let mut h = HelpContent::from_lines_and_anchors("t", vec!["a".into(), "b".into()], vec![]);
        h.scroll = 1;
        assert!(!h.scroll_to_anchor("nope"));
        assert_eq!(h.scroll, 1);
    }

    #[test]
    fn from_lines_creates_buffer_without_anchors() {
        let h = HelpContent::from_lines("t", vec!["x".into()]);
        assert!(h.anchors.is_empty());
    }

    #[test]
    fn from_lines_round_trips_through_buffer() {
        let h = HelpContent::from_lines("t", vec!["one".into(), "two".into(), "three".into()]);
        assert_eq!(h.title, "t");
        assert_eq!(h.line_count(), 3);
        let lines = h.lines();
        assert_eq!(lines, vec!["one", "two", "three"]);
    }

    #[test]
    fn empty_lines_yield_empty_buffer() {
        let h = HelpContent::from_lines("t", vec![]);
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
        let mut h = HelpContent::from_lines("t", lines);
        h.move_cursor(0, 5, 10);
        assert_eq!(h.cursor.line, 5);
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn move_cursor_past_viewport_advances_scroll() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpContent::from_lines("t", lines);
        h.move_cursor(0, 12, 10);
        assert_eq!(h.cursor.line, 12);
        // cursor at line 12 with viewport=10 -> scroll = 12 + 1 - 10 = 3.
        assert_eq!(h.scroll, 3);
    }

    #[test]
    fn move_cursor_clamps_to_last_line() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpContent::from_lines("t", lines);
        h.move_cursor(0, 1000, 10);
        assert_eq!(h.cursor.line, 19);
        // 20 lines - viewport 10 -> max scroll = 10.
        assert_eq!(h.scroll, 10);
    }

    #[test]
    fn move_cursor_up_pulls_scroll_back() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut h = HelpContent::from_lines("t", lines);
        h.move_cursor(0, 19, 10);
        assert_eq!(h.scroll, 10);
        // Move cursor up to line 5; scroll should clamp to follow.
        h.move_cursor(0, -14, 10);
        assert_eq!(h.cursor.line, 5);
        assert_eq!(h.scroll, 5);
    }

    #[test]
    fn move_cursor_horizontal_clamps_to_line_length() {
        let h_start = HelpContent::from_lines("t", vec!["abc".into(), "xy".into()]);
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
        let mut h = HelpContent::from_lines("t", vec!["hello world".into()]);
        h.cursor_line_end();
        // vim `$` lands on the last char index.
        assert_eq!(h.cursor.byte, 10);
        h.cursor_line_start();
        assert_eq!(h.cursor.byte, 0);
    }

    #[test]
    fn jump_top_resets_cursor_and_scroll() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpContent::from_lines("t", lines);
        h.move_cursor(0, 25, 10);
        assert_ne!(h.scroll, 0);
        h.jump_top();
        assert_eq!(h.cursor, Position::ZERO);
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn jump_bottom_lands_cursor_and_scroll() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpContent::from_lines("t", lines);
        h.jump_bottom(10);
        assert_eq!(h.cursor.line, 29);
        // 30 lines, viewport 10 -> scroll = 30 - 10 = 20.
        assert_eq!(h.scroll, 20);
    }

    #[test]
    fn half_page_motions_move_by_viewport_half() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut h = HelpContent::from_lines("t", lines);
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
    fn with_markdown_syntax_populates_highlights_for_headings() {
        let registry = LangRegistry::standard().expect("registry");
        let h = HelpContent::from_lines("t", vec!["# Configuration".into(), "body line".into()])
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
    fn hover_markdown_with_fenced_rust_block_has_highlights() {
        // Representative LSP hover body: fenced ```rust block
        // followed by prose with inline backtick code. Verifies
        // the markdown highlighter populates highlights for at
        // least the fenced code lines (rust injection) and the
        // inline-code spans on prose lines.
        let registry = LangRegistry::standard().expect("registry");
        let lines = vec![
            "```rust".to_string(),
            "fn write(&mut self, buf: &[u8]) -> Result<usize>".to_string(),
            "```".to_string(),
            String::new(),
            "Writes the contents of `buf` to the stream.".to_string(),
        ];
        let h = HelpContent::from_lines("hover", lines).with_markdown_syntax(registry);
        // Line 1: fenced rust code -- should carry Keyword spans
        // (`fn`) once the rust injection runs.
        let line1_has_styled = h
            .highlights
            .get(1)
            .map(|spans| !spans.is_empty())
            .unwrap_or(false);
        assert!(
            line1_has_styled,
            "expected highlights on fenced rust line, got {:?}",
            h.highlights.get(1)
        );
        // Line 4: inline `buf` -- should carry MarkupRaw or
        // similar.
        let line4_has_styled = h
            .highlights
            .get(4)
            .map(|spans| !spans.is_empty())
            .unwrap_or(false);
        assert!(
            line4_has_styled,
            "expected highlights on inline-code line, got {:?}",
            h.highlights.get(4)
        );
    }

    #[test]
    fn with_markdown_syntax_populates_subheading_and_inline_code() {
        let registry = LangRegistry::standard().expect("registry");
        let lines = vec![
            "# :lsp-status".to_string(),
            String::new(),
            "## rust-analyzer".to_string(),
            "- workspace root: `/home/foo`".to_string(),
            "- supports hover: true".to_string(),
        ];
        let h = HelpContent::from_lines("lsp-status", lines).with_markdown_syntax(registry);
        let h1 = h.highlights[0]
            .iter()
            .any(|sp| sp.style == lattice_syntax::Style::Heading1);
        // tree-sitter-md tags every heading with the same `markup.heading`
        // capture; the capture-name table maps it to Heading1
        // uniformly, so `##` shows up the same as `#`. Acceptable for
        // v1 -- improving heading-level differentiation requires a
        // grammar query update or per-level capture extraction.
        let h2 = h.highlights[2]
            .iter()
            .any(|sp| sp.style == lattice_syntax::Style::Heading1);
        assert!(h1, "line 0 (# heading): {:?}", h.highlights[0]);
        assert!(h2, "line 2 (## heading): {:?}", h.highlights[2]);
        // inline code on line 3 -- look for any Markup-family span
        // (MarkupRaw) covering the backticked range.
        let has_raw = h.highlights[3].iter().any(|sp| {
            matches!(
                sp.style,
                lattice_syntax::Style::MarkupRaw
                    | lattice_syntax::Style::Markup
                    | lattice_syntax::Style::String
            )
        });
        assert!(
            has_raw,
            "expected inline-code style on `/home/foo`, got {:?}",
            h.highlights[3]
        );
    }

    #[test]
    fn with_markdown_syntax_is_optional() {
        // The fallback path -- no registry means no highlights, but
        // the buffer still works.
        let h = HelpContent::from_lines("t", vec!["# title".into()]);
        assert!(h.highlights.is_empty());
        assert_eq!(h.content.line_count(), 1);
    }

    // --- Anchor links + heading slugs ---------------------------

    #[test]
    fn slugify_heading_matches_github_style() {
        assert_eq!(slugify_heading("# Quick reference"), "quick-reference");
        assert_eq!(slugify_heading("## 1. Tree-sitter, core"), "1-tree-sitter-core");
        assert_eq!(
            slugify_heading("### Step 1 -- pin the grammar crate"),
            "step-1-pin-the-grammar-crate"
        );
        assert_eq!(slugify_heading("### What you lose"), "what-you-lose");
        assert_eq!(slugify_heading("##  Trailing   space  "), "trailing-space");
        assert_eq!(slugify_heading("# `code` ignored?"), "code-ignored");
    }

    #[test]
    fn classify_link_url_routes_anchor_form() {
        match classify_link_url("#1-tree-sitter-core") {
            HelpLinkTarget::Anchor(s) => assert_eq!(s, "1-tree-sitter-core"),
            other => panic!("expected Anchor, got {other:?}"),
        }
    }

    #[test]
    fn classify_link_url_routes_customize_scheme() {
        // M.9.1: `customize:NAME` follows to `:customize NAME`.
        match classify_link_url("customize:lsp-completion-mode") {
            HelpLinkTarget::Customize(s) => {
                assert_eq!(s, "lsp-completion-mode");
            }
            other => panic!("expected Customize, got {other:?}"),
        }
        match classify_link_url("customize:editor") {
            HelpLinkTarget::Customize(s) => assert_eq!(s, "editor"),
            other => panic!("expected Customize, got {other:?}"),
        }
    }

    #[test]
    fn classify_link_url_routes_customize_edit_scheme_separately() {
        // M.9.2: `customize-edit:NAME` is a distinct scheme
        // from `customize:NAME` (must come first in the parse
        // chain since they share a prefix).
        match classify_link_url("customize-edit:tabstop") {
            HelpLinkTarget::CustomizeEdit(s) => assert_eq!(s, "tabstop"),
            other => panic!("expected CustomizeEdit, got {other:?}"),
        }
        // The plain `customize:` scheme stays correct -- not
        // accidentally captured by the longer prefix's parse.
        match classify_link_url("customize:editor") {
            HelpLinkTarget::Customize(s) => assert_eq!(s, "editor"),
            other => panic!("expected Customize, got {other:?}"),
        }
    }

    #[test]
    fn generate_heading_anchors_emits_one_per_heading() {
        let lines = vec![
            "# Title".into(),
            "body".into(),
            "## 1. Tree-sitter, core".into(),
            "more".into(),
            "### Step 1 -- pin".into(),
            "## 2. Plugin".into(),
        ];
        let anchors = generate_heading_anchors(&lines);
        let names: Vec<&str> = anchors.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["title", "1-tree-sitter-core", "step-1-pin", "2-plugin"]
        );
        assert_eq!(anchors[1].line, 2);
        assert_eq!(anchors[3].line, 5);
    }

    #[test]
    fn generate_heading_anchors_skips_inside_fenced_code_blocks() {
        // A `# foo` line inside a code fence is example content, not
        // a real heading.
        let lines = vec![
            "# Real Title".into(),
            "```".into(),
            "# not a heading".into(),
            "```".into(),
            "## After".into(),
        ];
        let anchors = generate_heading_anchors(&lines);
        let names: Vec<&str> = anchors.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["real-title", "after"]);
    }

    #[test]
    fn from_lines_parses_anchor_link_target() {
        // A markdown link `[Section 1](#1-tree-sitter-core)` should
        // produce a HelpLink with the Anchor target so follow-link
        // routes to scroll_to_anchor instead of "no handler".
        let h = HelpContent::from_lines(
            "t",
            vec!["see [Section 1](#1-tree-sitter-core) for details".into()],
        );
        assert_eq!(h.links.len(), 1);
        match &h.links[0].target {
            HelpLinkTarget::Anchor(slug) => assert_eq!(slug, "1-tree-sitter-core"),
            other => panic!("expected Anchor target, got {other:?}"),
        }
    }
}
