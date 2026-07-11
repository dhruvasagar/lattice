//! The content contract between a section and the dashboard compositor.
//!
//! A [`DashboardSection`](crate::DashboardSection) never touches cells,
//! theme ids, or the renderer. It emits a [`DashboardFragment`]: styled,
//! aligned, optionally-linked text. The compositor (DB.2+) turns fragments
//! into a branding virtual-row block plus document body. Keeping this the
//! single content type is what lets built-in sections (native Rust) and
//! future plugin sections share one pipeline.

/// One section's rendered contribution: an ordered list of visual lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashboardFragment {
    pub rows: Vec<DashboardRow>,
}

impl DashboardFragment {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the section rendered nothing (all sections should render
    /// at least one row; an empty fragment is a section bug the registry
    /// tests guard against).
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Push a whole row.
    pub fn push(&mut self, row: DashboardRow) -> &mut Self {
        self.rows.push(row);
        self
    }

    /// Convenience: a single left-aligned span of one role.
    pub fn line(&mut self, text: impl Into<String>, role: DashboardRole) -> &mut Self {
        self.rows.push(DashboardRow::line(text, role));
        self
    }

    /// Convenience: a blank spacer row.
    pub fn blank(&mut self) -> &mut Self {
        self.rows.push(DashboardRow::default());
        self
    }
}

/// One visual line: spans laid out left→right, with a line-level alignment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashboardRow {
    pub spans: Vec<DashboardSpan>,
    pub align: Align,
}

impl DashboardRow {
    /// A single-span line of one role, left-aligned.
    pub fn line(text: impl Into<String>, role: DashboardRole) -> Self {
        Self {
            spans: vec![DashboardSpan::new(text, role)],
            align: Align::Left,
        }
    }

    /// A single-span line of one role, centred.
    pub fn centered(text: impl Into<String>, role: DashboardRole) -> Self {
        Self {
            spans: vec![DashboardSpan::new(text, role)],
            align: Align::Center,
        }
    }

    pub fn with_align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub fn push(mut self, span: DashboardSpan) -> Self {
        self.spans.push(span);
        self
    }

    /// Total display text of the row (spans concatenated), ignoring style.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// A run of text with a semantic role and an optional link target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardSpan {
    pub text: String,
    pub role: DashboardRole,
    pub link: Option<LinkTarget>,
}

impl DashboardSpan {
    pub fn new(text: impl Into<String>, role: DashboardRole) -> Self {
        Self {
            text: text.into(),
            role,
            link: None,
        }
    }

    /// A link span: the visible label carries [`DashboardRole::Link`] and a
    /// follow target.
    pub fn link(label: impl Into<String>, target: LinkTarget) -> Self {
        Self {
            text: label.into(),
            role: DashboardRole::Link,
            link: Some(target),
        }
    }
}

/// Semantic style role. Never a colour — each role resolves to a
/// `dashboard.*` theme element at compose time (DB.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DashboardRole {
    /// The brand mark art.
    Logo,
    /// The blinking-cursor bar inside the mark (amber by default).
    Cursor,
    /// The "Lattice" wordmark.
    Title,
    /// The one-line tagline.
    Tagline,
    /// A section heading.
    SectionHeading,
    /// Body prose.
    Body,
    /// A key cap (e.g. `:`, `<leader>`).
    Key,
    /// Muted hint / secondary text.
    Hint,
    /// A followable link label.
    Link,
}

/// Line-level alignment. `Right` is reserved (no consumer yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
}

/// A link a `<CR>` follows. Mirrors the help-buffer `scheme:value` model so
/// DB.2 reuses the help follow mechanism rather than inventing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Run an ex-command, e.g. `cmd:tutor`.
    Command(String),
    /// Open a `:help` topic, e.g. `topic:getting-started`.
    Topic(String),
    /// Open a URL externally, e.g. `url:https://…`.
    Url(String),
}

impl LinkTarget {
    /// Parse a `scheme:value` string. Unknown or scheme-less input is
    /// treated as a URL only when it looks like one; otherwise `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        let (scheme, value) = raw.split_once(':')?;
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        match scheme {
            "cmd" => Some(LinkTarget::Command(value.to_string())),
            "topic" => Some(LinkTarget::Topic(value.to_string())),
            // A `url:` prefix, or a bare http(s) URL (split_once left the
            // scheme as "https"/"http").
            "url" => Some(LinkTarget::Url(value.to_string())),
            "http" | "https" => Some(LinkTarget::Url(raw.to_string())),
            _ => None,
        }
    }

    /// Serialise back to `scheme:value` form (the form the help follow
    /// mechanism consumes).
    pub fn to_scheme_string(&self) -> String {
        match self {
            LinkTarget::Command(v) => format!("cmd:{v}"),
            LinkTarget::Topic(v) => format!("topic:{v}"),
            LinkTarget::Url(v) => {
                if v.starts_with("http://") || v.starts_with("https://") {
                    v.clone()
                } else {
                    format!("url:{v}")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_target_parses_known_schemes() {
        assert_eq!(
            LinkTarget::parse("cmd:tutor"),
            Some(LinkTarget::Command("tutor".into()))
        );
        assert_eq!(
            LinkTarget::parse("topic:getting-started"),
            Some(LinkTarget::Topic("getting-started".into()))
        );
        assert_eq!(
            LinkTarget::parse("url:https://example.com"),
            Some(LinkTarget::Url("https://example.com".into()))
        );
    }

    #[test]
    fn link_target_parses_bare_url() {
        assert_eq!(
            LinkTarget::parse("https://github.com/dhruvasagar/lattice"),
            Some(LinkTarget::Url(
                "https://github.com/dhruvasagar/lattice".into()
            ))
        );
    }

    #[test]
    fn link_target_rejects_unknown_and_empty() {
        assert_eq!(LinkTarget::parse("bogus:thing"), None);
        assert_eq!(LinkTarget::parse("cmd:"), None);
        assert_eq!(LinkTarget::parse("no-colon"), None);
    }

    #[test]
    fn link_target_round_trips() {
        for raw in ["cmd:tutor", "topic:help", "url:mailto:x@y.z"] {
            let t = LinkTarget::parse(raw).unwrap();
            assert_eq!(t.to_scheme_string(), raw);
        }
        // Bare URL round-trips without the url: prefix.
        let t = LinkTarget::parse("https://x.y").unwrap();
        assert_eq!(t.to_scheme_string(), "https://x.y");
    }

    #[test]
    fn row_text_concatenates_spans() {
        let row = DashboardRow::line("Open ", DashboardRole::Body).push(DashboardSpan::link(
            ":tutor",
            LinkTarget::Command("tutor".into()),
        ));
        assert_eq!(row.text(), "Open :tutor");
    }
}
