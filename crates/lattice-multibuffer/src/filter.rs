//! MF.1: keep or reject a view's excerpts by pattern.
//!
//! `:multibuffer-keep /pat/` and `:multibuffer-reject /pat/` narrow the
//! excerpts a view shows; `:multibuffer-clear-filters` brings every one
//! back. Vim's `:Cfilter` / `:Cfilter!` over the quickfix list, and
//! emacs's `keep-lines` / `flush-lines` over an occur buffer, are the
//! convention: an excerpt matches when its text OR its file path does.
//!
//! **A filter is a property of the view, not a pass over its contents.**
//! The view keeps every excerpt its provider supplied (`FilterState::all`)
//! and shows the ones every filter admits. So:
//!
//! - results that stream in AFTER the filter was set are filtered too —
//!   a project search is usually still running when you narrow it;
//! - a provider refresh (`gr`) re-runs with the filters still applied,
//!   and the headerline says so, so a narrowed view never passes for a
//!   complete one;
//! - clearing restores the excerpts as they are NOW — expanded,
//!   slid past edits — because every mutation of the visible list is
//!   mirrored onto `all`.
//!
//! It never edits a source. That is the whole difference from `:g` /
//! `:v`, which in an editable multibuffer delete lines from the files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_runtime::Document;

use crate::{Excerpt, MultibufferDocumentHandle, MultibufferState};

/// The editor's working directory, as the host registers it.
///
/// The same type as `providers::search::CurrentDirHandle` (a type alias
/// shares its `TypeId`, so the service lookup finds the host's
/// registration); spelled out here because that alias sits behind the
/// `search` feature and the filter does not. Pinned equal by
/// `cwd_handle_is_the_search_providers_type`.
pub type CwdHandle = Arc<Mutex<Option<PathBuf>>>;

/// Whether a filter keeps what it matches or drops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// Show only excerpts that match.
    Keep,
    /// Hide excerpts that match.
    Reject,
}

/// One `:multibuffer-keep` / `:multibuffer-reject`.
#[derive(Debug, Clone)]
pub struct ExcerptFilter {
    kind: FilterKind,
    pattern: String,
    regex: fancy_regex::Regex,
    /// Paths are matched relative to this — the working directory when
    /// the filter was set. Fixed at creation so a later `:cd` does not
    /// change what an existing filter means halfway through a stream.
    base: Option<PathBuf>,
}

impl ExcerptFilter {
    /// Compile `pattern` (already stripped of its delimiters — see
    /// [`parse_filter_pattern`]). The dialect is `:g`'s: fancy-regex,
    /// case-sensitive, `(?i)` to fold case.
    pub fn new(kind: FilterKind, pattern: &str, base: Option<PathBuf>) -> Result<Self, String> {
        let regex = fancy_regex::Regex::new(pattern).map_err(|e| e.to_string())?;
        Ok(Self {
            kind,
            pattern: pattern.to_string(),
            regex,
            base,
        })
    }

    pub fn kind(&self) -> FilterKind {
        self.kind
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    fn is_match(&self, text: &str) -> bool {
        // A pattern that exhausts fancy-regex's backtrack limit on some
        // line is treated as not matching THAT line — the filter still
        // applies everywhere else, and a pathological pattern costs a
        // bounded amount per line rather than hanging the view.
        self.regex.is_match(text).unwrap_or_else(|err| {
            tracing::debug!(%err, pattern = %self.pattern, "multibuffer filter: match aborted");
            false
        })
    }

    /// Does `excerpt` match — its path, or any line it spans?
    fn matches(&self, excerpt: &Excerpt, source: Option<&Arc<dyn Document>>) -> bool {
        let path = excerpt
            .header
            .path
            .clone()
            .or_else(|| source.and_then(|s| s.path()));
        if let Some(path) = path
            && self.is_match(&display_path(&path, self.base.as_deref()))
        {
            return true;
        }
        let Some(source) = source else {
            return false;
        };
        let snap = source.snapshot();
        (excerpt.start_line..=excerpt.end_line)
            .filter_map(|row| snap.buffer.line(row))
            .any(|line| self.is_match(line.trim_end_matches(['\n', '\r'])))
    }

    fn admits(&self, excerpt: &Excerpt, source: Option<&Arc<dyn Document>>) -> bool {
        match self.kind {
            FilterKind::Keep => self.matches(excerpt, source),
            FilterKind::Reject => !self.matches(excerpt, source),
        }
    }
}

/// `path` as the user reads it: relative to `base` when under it.
///
/// Matching the absolute path would make `:multibuffer-keep src` keep
/// everything in a checkout that happens to live under `~/src`.
fn display_path(path: &Path, base: Option<&Path>) -> String {
    base.and_then(|b| path.strip_prefix(b).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// The `:` argument as a pattern: `/pat/` or a bare `pat`.
///
/// Only `/` delimits, as for `:Cfilter`; a pattern that must start and
/// end with a literal `/` is written `\/…\/`. Empty is an error — a
/// filter on nothing would keep everything or nothing, neither of which
/// is what someone typing it meant.
pub fn parse_filter_pattern(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let inner = match trimmed.strip_prefix('/') {
        // `/pat/`, or `/pat` with the closing delimiter left off.
        Some(rest) => rest.strip_suffix('/').unwrap_or(rest),
        None => trimmed,
    };
    if inner.is_empty() {
        return Err("a pattern is required: /pat/".to_string());
    }
    Ok(inner.to_string())
}

/// Active filters plus every excerpt the provider supplied.
#[derive(Debug, Default)]
pub(crate) struct FilterState {
    pub(crate) filters: Vec<ExcerptFilter>,
    /// The unfiltered list, in provider order. The visible list is
    /// always `all` filtered by `filters`.
    pub(crate) all: Vec<Excerpt>,
}

impl FilterState {
    pub(crate) fn admits(
        &self,
        excerpt: &Excerpt,
        sources: &HashMap<BufferId, Arc<dyn Document>>,
    ) -> bool {
        let source = sources.get(&excerpt.source);
        self.filters.iter().all(|f| f.admits(excerpt, source))
    }

    pub(crate) fn visible(&self, sources: &HashMap<BufferId, Arc<dyn Document>>) -> Vec<Excerpt> {
        self.all
            .iter()
            .filter(|ex| self.admits(ex, sources))
            .cloned()
            .collect()
    }

    /// The headerline's account of the filters: `keep /a/ · reject /b/ ·
    /// 12 of 40 shown`.
    pub(crate) fn note(&self, visible: usize) -> String {
        let mut parts: Vec<String> = self
            .filters
            .iter()
            .map(|f| {
                let verb = match f.kind {
                    FilterKind::Keep => "keep",
                    FilterKind::Reject => "reject",
                };
                format!("{verb} /{}/", f.pattern)
            })
            .collect();
        parts.push(format!("{visible} of {} shown", self.all.len()));
        parts.join(" · ")
    }
}

/// What a filter change did, for the echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterOutcome {
    pub visible: usize,
    pub total: usize,
}

impl MultibufferDocumentHandle {
    /// Add `filter` to the view's filters and show only what they all
    /// admit. Filters stack: `keep /a/` then `reject /b/` shows the
    /// excerpts matching `a` and not `b`.
    pub fn push_filter(&self, filter: ExcerptFilter) -> FilterOutcome {
        let mut state = self.lock_state();
        let mut f = state.filter.take().unwrap_or_else(|| FilterState {
            filters: Vec::new(),
            // First filter: everything shown so far IS everything.
            all: state.excerpts.clone(),
        });
        f.filters.push(filter);
        state.excerpts = f.visible(&state.sources);
        let outcome = FilterOutcome {
            visible: state.excerpts.len(),
            total: f.all.len(),
        };
        state.filter = Some(f);
        self.refresh_filter_note(&state);
        self.republish_whole(state);
        outcome
    }

    /// Drop every filter and show every excerpt again. `None` when the
    /// view was not filtered — nothing to restore, and nothing republished.
    pub fn clear_filters(&self) -> Option<FilterOutcome> {
        let mut state = self.lock_state();
        let f = state.filter.take()?;
        state.excerpts = f.all;
        let total = state.excerpts.len();
        self.refresh_filter_note(&state);
        self.republish_whole(state);
        Some(FilterOutcome {
            visible: total,
            total,
        })
    }

    /// The active filters, oldest first.
    pub fn filters(&self) -> Vec<ExcerptFilter> {
        self.lock_state()
            .filter
            .as_ref()
            .map(|f| f.filters.clone())
            .unwrap_or_default()
    }

    /// The headerline's filter note, as it would render now.
    pub fn filter_note(&self) -> Option<String> {
        (**self.inner.filter_note.load()).clone()
    }

    /// Recompute the note from `state` and, when it changed, bump the
    /// status row's version so the headerline re-renders. Called with
    /// the state lock held, so the note and the excerpts it counts never
    /// disagree.
    pub(crate) fn refresh_filter_note(&self, state: &MultibufferState) {
        let note = state.filter.as_ref().map(|f| f.note(state.excerpts.len()));
        if **self.inner.filter_note.load() == note {
            return;
        }
        self.inner.filter_note.store(Arc::new(note));
        self.inner
            .headerline_version
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::slide_anchors_for_source;

    #[test]
    fn delimiters_are_stripped_and_bare_patterns_pass_through() {
        assert_eq!(parse_filter_pattern("/foo/").unwrap(), "foo");
        assert_eq!(parse_filter_pattern("  /foo bar/ ").unwrap(), "foo bar");
        assert_eq!(parse_filter_pattern("/foo").unwrap(), "foo");
        assert_eq!(parse_filter_pattern("foo").unwrap(), "foo");
        assert_eq!(parse_filter_pattern(r"/tests\//").unwrap(), r"tests\/");
    }

    #[test]
    fn an_empty_pattern_is_refused() {
        assert!(parse_filter_pattern("").is_err());
        assert!(parse_filter_pattern("   ").is_err());
        assert!(parse_filter_pattern("//").is_err());
    }

    #[test]
    fn a_bad_regex_is_refused_at_creation() {
        assert!(ExcerptFilter::new(FilterKind::Keep, "(", None).is_err());
    }

    #[test]
    fn paths_are_matched_relative_to_the_base() {
        let base = Path::new("/home/me/src/proj");
        assert_eq!(
            display_path(Path::new("/home/me/src/proj/lib/a.rs"), Some(base)),
            "lib/a.rs"
        );
        // Outside the base: the whole path, not nothing.
        assert_eq!(display_path(Path::new("/etc/x"), Some(base)), "/etc/x");
    }

    // ── The view: filters as a property, not a one-off pass ──────────

    fn registry() -> lattice_grammar::CommandRegistryHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(
            lattice_grammar::CommandRegistry::new(),
        ))
    }

    /// One source, one excerpt per line — `:search` with no context.
    fn view_over(text: &str) -> (MultibufferDocumentHandle, BufferId, Vec<Excerpt>) {
        let id = BufferId::next();
        let doc: Arc<dyn Document> = Arc::new(lattice_runtime::spawn_document(
            id,
            lattice_core::Document::from_text(text),
            registry(),
        ));
        let lines = text.lines().count() as u32;
        let excerpts: Vec<Excerpt> = (0..lines).map(|r| Excerpt::new(id, r, r)).collect();
        let mut sources = HashMap::new();
        sources.insert(id, doc);
        let mb = MultibufferDocumentHandle::new(sources, Vec::new(), registry()).unwrap();
        (mb, id, excerpts)
    }

    fn keep(p: &str) -> ExcerptFilter {
        ExcerptFilter::new(FilterKind::Keep, p, None).unwrap()
    }

    fn reject(p: &str) -> ExcerptFilter {
        ExcerptFilter::new(FilterKind::Reject, p, None).unwrap()
    }

    fn text(mb: &MultibufferDocumentHandle) -> String {
        mb.snapshot().buffer.as_string()
    }

    /// A search is usually still streaming when you narrow it. What
    /// arrives afterwards must be narrowed the same, or the filter is
    /// only true of the moment it was typed.
    #[tokio::test(flavor = "multi_thread")]
    async fn results_that_arrive_after_the_filter_are_filtered() {
        let (mb, _, ex) = view_over("a1\nb1\na2\nb2\n");
        mb.append_excerpts(ex[..2].to_vec());
        let out = mb.push_filter(keep("^a"));
        assert_eq!(
            out,
            FilterOutcome {
                visible: 1,
                total: 2
            }
        );
        mb.append_excerpts(ex[2..].to_vec());
        assert_eq!(text(&mb), "a1\na2\n");
        assert_eq!(
            mb.filter_note().as_deref(),
            Some("keep /^a/ · 2 of 4 shown")
        );
    }

    /// `gr` replaces what the filters run over; it does not drop them.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refresh_keeps_the_filters() {
        let (mb, id, ex) = view_over("a1\nb1\na2\n");
        mb.append_excerpts(ex.clone());
        mb.push_filter(reject("^b"));
        // What a provider's refresh does: clear, then stream again.
        let sources = {
            let state = mb.lock_state();
            state.sources.clone()
        };
        mb.replace_excerpts(sources, Vec::new());
        assert_eq!(text(&mb), "");
        mb.append_excerpts(vec![Excerpt::new(id, 0, 0), Excerpt::new(id, 1, 1)]);
        assert_eq!(text(&mb), "a1\n");
        assert_eq!(mb.filters().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filters_stack_and_clearing_restores_every_excerpt_in_order() {
        let (mb, _, ex) = view_over("a1\nb1\na2\nab\n");
        mb.append_excerpts(ex);
        mb.push_filter(keep("a"));
        mb.push_filter(reject("b"));
        assert_eq!(text(&mb), "a1\na2\n");
        let out = mb.clear_filters().expect("was filtered");
        assert_eq!(
            out,
            FilterOutcome {
                visible: 4,
                total: 4
            }
        );
        assert_eq!(text(&mb), "a1\nb1\na2\nab\n");
        assert_eq!(mb.filter_note(), None, "an unfiltered view says nothing");
        assert_eq!(mb.clear_filters(), None, "nothing left to clear");
    }

    /// Clearing restores excerpts as they are NOW: one expanded while
    /// filtered comes back expanded, not at the size it was hidden at.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_excerpt_expanded_while_filtered_comes_back_expanded() {
        let (mb, _, ex) = view_over("x0\nhit\nx2\nother\n");
        mb.append_excerpts(vec![ex[1].clone(), ex[3].clone()]);
        mb.push_filter(keep("hit"));
        mb.expand_excerpt_at(0, 2);
        assert_eq!(text(&mb), "x0\nhit\nx2\n");
        mb.clear_filters();
        assert_eq!(text(&mb), "x0\nhit\nx2\nother\n");
    }

    /// A hidden excerpt is still anchored to its source: an edit above it
    /// slides it, and clearing shows it at its new row.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hidden_excerpt_slides_with_its_source() {
        let (mb, id, ex) = view_over("keep\nhidden\n");
        mb.append_excerpts(ex);
        mb.push_filter(keep("keep"));
        // An outside edit inserting a line above `hidden` (at row 0).
        slide_anchors_for_source(
            &mb.inner,
            id,
            &[lattice_protocol::event::AppliedEdit {
                original_range: lattice_protocol::position::Range::new(
                    lattice_protocol::position::Position::new(0, 0),
                    lattice_protocol::position::Position::new(0, 0),
                ),
                inserted_range: lattice_protocol::position::Range::new(
                    lattice_protocol::position::Position::new(0, 0),
                    lattice_protocol::position::Position::new(1, 0),
                ),
                replaced_text: String::new(),
                inserted_text: "new\n".to_string(),
            }],
        );
        let hidden = mb.lock_state().filter.as_ref().unwrap().all[1].clone();
        assert_eq!((hidden.start_line, hidden.end_line), (2, 2));
    }

    /// A filtered view always says so — after a provider's own status,
    /// or alone on a view that has none.
    #[test]
    fn the_status_row_carries_the_filter_note() {
        let row = |status: &crate::HeaderlineStatus, note: Option<&str>| -> Option<String> {
            crate::render_multibuffer_status(status, note, 1, 2, 3, 4).map(|r| {
                r.cells
                    .iter()
                    .filter_map(|c| char::from_u32(c.codepoint))
                    .collect()
            })
        };
        let done = crate::HeaderlineStatus::Complete {
            summary: "\"TODO\" — 9 hits in 3 files".into(),
            emphasis: None,
        };
        assert_eq!(
            row(&done, Some("reject /tests/ · 6 of 9 shown")).as_deref(),
            Some(" ◆ \"TODO\" — 9 hits in 3 files · reject /tests/ · 6 of 9 shown ")
        );
        assert_eq!(
            row(
                &crate::HeaderlineStatus::Idle,
                Some("keep /a/ · 1 of 2 shown")
            )
            .as_deref(),
            Some(" keep /a/ · 1 of 2 shown ")
        );
        assert_eq!(row(&crate::HeaderlineStatus::Idle, None), None);
    }

    #[cfg(feature = "search")]
    #[test]
    fn cwd_handle_is_the_search_providers_type() {
        assert_eq!(
            std::any::TypeId::of::<CwdHandle>(),
            std::any::TypeId::of::<crate::providers::search::CurrentDirHandle>()
        );
    }
}
