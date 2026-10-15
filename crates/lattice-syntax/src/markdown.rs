//! Markdown link syntax — the textual half of markdown links that more
//! than one crate reads.
//!
//! `markdown-mode` follows links on `<CR>`; help buffers strip links down
//! to their labels when they are built. Both need the same answers to
//! "where does this `[label](url)` start and end" and "which line does
//! `#some-heading` name", and both live at or above this crate, so the
//! answers live here, beside the markdown grammar that owns the language.
//! See `docs/dev/architecture/markdown-links.md`.
//!
//! Scanning is over the line's *text*, not the parse tree, for the reason
//! conceal gives: the tree is absent during a reparse, and a link that
//! stops resolving while the user types is worse than one that never
//! consulted the tree.

/// Byte bounds of one inline link `[label](url)`, in the text it was
/// scanned from. `label` excludes the brackets, `url` the parentheses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineLinkSpan {
    /// Byte index of the opening `[`.
    pub open: usize,
    /// First byte of the label.
    pub label_start: usize,
    /// One past the label's last byte (the closing `]`).
    pub label_end: usize,
    /// First byte of the url.
    pub url_start: usize,
    /// One past the url's last byte (the closing `)`).
    pub url_end: usize,
}

impl InlineLinkSpan {
    /// One past the closing `)` — where the next scan resumes.
    pub fn end(&self) -> usize {
        self.url_end + 1
    }
}

/// Escape the link syntax's own punctuation — `\`, `[`, `]`, `(`, `)` — with a
/// backslash, so a label or URL may contain it. Chords are the reason: `]f`,
/// `di(`, `da)`, `ci]` are ordinary motions and text objects, and unescaped
/// each one ended the label or the URL early. [`unescape_link_text`] inverts it.
pub fn escape_link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '[' | ']' | '(' | ')') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Inverse of [`escape_link_text`]: a backslash makes the next char literal.
pub fn unescape_link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.extend(chars.next()),
            other => out.push(other),
        }
    }
    out
}

/// Byte index of the first `target` at or after `from` that is not escaped.
/// `target` is ASCII, so a match is always a char boundary.
fn find_unescaped(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b if b == target => return Some(j),
            _ => j += 1,
        }
    }
    None
}

/// `[label](url)` starting at the `[` at `open`, escape-aware. `None` when
/// it is not a well-formed link.
pub fn scan_inline_link(bytes: &[u8], open: usize) -> Option<InlineLinkSpan> {
    if bytes.get(open) != Some(&b'[') {
        return None;
    }
    let label_start = open + 1;
    let label_end = find_unescaped(bytes, label_start, b']')?;
    if bytes.get(label_end + 1) != Some(&b'(') {
        return None;
    }
    let url_start = label_end + 2;
    let url_end = find_unescaped(bytes, url_start, b')')?;
    Some(InlineLinkSpan {
        open,
        label_start,
        label_end,
        url_start,
        url_end,
    })
}

/// Every inline link in `text`, left to right, non-overlapping.
pub fn inline_links(text: &str) -> Vec<InlineLinkSpan> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'[' => match scan_inline_link(bytes, i) {
                Some(span) => {
                    i = span.end();
                    out.push(span);
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    out
}

/// What a link under the cursor points at, before anything resolves it.
/// The URL is unescaped and its optional `"title"` / `<…>` wrapping removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkDestination {
    /// `[t](#slug)` — a heading in the same document.
    Anchor(String),
    /// `[t](path)` or `[t](path#slug)` — another file, optionally at a
    /// heading. `path` is as written: a relative path is the caller's to
    /// resolve against the document's directory.
    File {
        path: String,
        anchor: Option<String>,
    },
    /// `[t](https://…)`, `mailto:`, or any `scheme://` — for the OS handler.
    External(String),
}

/// Classify a link destination as written in a markdown document.
/// `None` for an empty destination — there is nothing to follow.
pub fn classify_destination(raw: &str) -> Option<LinkDestination> {
    let url = strip_destination(raw);
    if url.is_empty() {
        return None;
    }
    if let Some(slug) = url.strip_prefix('#') {
        return (!slug.is_empty()).then(|| LinkDestination::Anchor(slug.to_string()));
    }
    if is_external_url(&url) {
        return Some(LinkDestination::External(url));
    }
    let url = url.strip_prefix("file://").unwrap_or(&url);
    let (path, anchor) = match url.split_once('#') {
        Some((p, a)) => (p, (!a.is_empty()).then(|| a.to_string())),
        None => (url, None),
    };
    if path.is_empty() {
        return anchor.map(LinkDestination::Anchor);
    }
    Some(LinkDestination::File {
        path: percent_decode(path),
        anchor,
    })
}

/// `<dest>` and `dest "title"` are both CommonMark; neither wrapping is
/// part of where the link goes.
fn strip_destination(raw: &str) -> String {
    let s = unescape_link_text(raw.trim());
    if let Some(inner) = s.strip_prefix('<')
        && let Some(end) = inner.find('>')
    {
        return inner[..end].to_string();
    }
    match s.split_once(char::is_whitespace) {
        Some((dest, _title)) => dest.to_string(),
        None => s,
    }
}

/// Decode `%XX` escapes — `my%20notes.md` is how a path with a space is
/// written in a link. A malformed escape is kept literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Whether `url` carries a real network / application scheme that the OS
/// handler should open (browser, mail client, registered app). True for
/// any `scheme://…` authority form (`http://`, `https://`, `ftp://`, and
/// app links like `slack://` / `vscode://`) and for `mailto:`. Bare
/// schemeless strings (`github.com/x`, a relative path) are NOT treated as
/// URLs, rather than risk shelling out on ambiguous input. `file://` is
/// excluded: a local file opens in the editor.
pub fn is_external_url(url: &str) -> bool {
    if url.starts_with("mailto:") {
        return true;
    }
    // `scheme://authority`: a non-empty scheme of URL-safe characters
    // followed by `://`. Guarding the scheme shape (rather than a bare
    // `contains("://")`) keeps this from matching stray text.
    if let Some((scheme, _)) = url.split_once("://") {
        return !scheme.is_empty()
            && !scheme.eq_ignore_ascii_case("file")
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    }
    false
}

/// The link whose markup contains byte `col` of `line`, with its
/// destination classified. A cursor anywhere from the `[` to the `)` is on
/// the link — the label is what shows when the markup is concealed, but the
/// raw form is what Insert mode shows, and both must give the same answer.
pub fn link_at(line: &str, col: usize) -> Option<LinkDestination> {
    inline_links(line)
        .into_iter()
        .find(|s| (s.open..s.end()).contains(&col))
        .and_then(|s| classify_destination(&line[s.url_start..s.url_end]))
        .or_else(|| autolink_at(line, col))
}

/// `<https://…>` containing `col` — CommonMark's autolink.
fn autolink_at(line: &str, col: usize) -> Option<LinkDestination> {
    let mut from = 0;
    while let Some(rel) = line[from..].find('<') {
        let open = from + rel;
        let close = open + line[open..].find('>')?;
        let inner = &line[open + 1..close];
        if (open..=close).contains(&col)
            && !inner.contains(char::is_whitespace)
            && is_external_url(inner)
        {
            return Some(LinkDestination::External(inner.to_string()));
        }
        from = close + 1;
    }
    None
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

/// `(slug, line)` for every ATX heading (`#`, `##`, …) in `lines`, in
/// document order. Heading-shaped lines inside fenced code blocks
/// (` ``` ` / ` ~~~ `) are skipped, so a `# foo` comment in a shell example
/// is not an anchor.
pub fn heading_slugs<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (i, line) in lines.into_iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !trimmed.starts_with('#') {
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
        if !slug.is_empty() {
            out.push((slug, i as u32));
        }
    }
    out
}

/// The line of the heading whose slug is `slug`, if any. Compared
/// case-insensitively — `#Install` written by hand still names `# Install`.
pub fn find_heading<'a>(lines: impl IntoIterator<Item = &'a str>, slug: &str) -> Option<u32> {
    let want = slug.to_ascii_lowercase();
    heading_slugs(lines)
        .into_iter()
        .find(|(s, _)| *s == want)
        .map(|(_, line)| line)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn inline_links_finds_every_link_and_skips_escapes() {
        let text = r"see [a](x.md) and \[not](y) then [b](#c)";
        let spans = inline_links(text);
        assert_eq!(spans.len(), 2);
        assert_eq!(&text[spans[0].label_start..spans[0].label_end], "a");
        assert_eq!(&text[spans[1].url_start..spans[1].url_end], "#c");
    }

    #[test]
    fn classify_covers_anchor_file_file_with_anchor_and_external() {
        assert_eq!(
            classify_destination("#install"),
            Some(LinkDestination::Anchor("install".into()))
        );
        assert_eq!(
            classify_destination("../docs/a.md"),
            Some(LinkDestination::File {
                path: "../docs/a.md".into(),
                anchor: None
            })
        );
        assert_eq!(
            classify_destination("guide.md#setup"),
            Some(LinkDestination::File {
                path: "guide.md".into(),
                anchor: Some("setup".into())
            })
        );
        assert_eq!(
            classify_destination("https://example.com/a#b"),
            Some(LinkDestination::External("https://example.com/a#b".into()))
        );
        assert_eq!(classify_destination(""), None);
        assert_eq!(classify_destination("#"), None);
    }

    #[test]
    fn classify_strips_title_angle_brackets_and_percent_escapes() {
        let file = |p: &str| {
            Some(LinkDestination::File {
                path: p.into(),
                anchor: None,
            })
        };
        assert_eq!(classify_destination(r#"a.md "The title""#), file("a.md"));
        assert_eq!(classify_destination("<my notes.md>"), file("my notes.md"));
        assert_eq!(classify_destination("my%20notes.md"), file("my notes.md"));
        assert_eq!(classify_destination("file:///tmp/a.md"), file("/tmp/a.md"));
    }

    #[test]
    fn link_at_covers_the_whole_markup_and_nothing_else() {
        let line = "x [label](a.md) y";
        let open = line.find('[').unwrap();
        let close = line.find(')').unwrap();
        assert!(link_at(line, open).is_some());
        assert!(link_at(line, close).is_some());
        assert!(link_at(line, open - 1).is_none());
        assert!(link_at(line, close + 1).is_none());
    }

    #[test]
    fn link_at_recognises_autolinks() {
        let line = "go to <https://example.com> now";
        let col = line.find("example").unwrap();
        assert_eq!(
            link_at(line, col),
            Some(LinkDestination::External("https://example.com".into()))
        );
        assert_eq!(link_at("a <b> c", 3), None);
    }

    #[test]
    fn find_heading_skips_fences_and_ignores_case() {
        let lines = [
            "# Title",
            "```sh",
            "# not a heading",
            "```",
            "## Not A Heading Either",
        ];
        assert_eq!(find_heading(lines, "title"), Some(0));
        assert_eq!(find_heading(lines, "not-a-heading"), None);
        assert_eq!(find_heading(lines, "Not-A-Heading-Either"), Some(4));
    }
}
