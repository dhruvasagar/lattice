//! Insert-mode completion -- the editor surface that turns the
//! existing pipeline (cmdline today) into a buffer-level input
//! flow.
//!
//! Behavioural spec lives in
//! [`docs/insert-completion.md`](../../../docs/insert-completion.md).
//! This module is the data-flow layer: state types, trigger
//! enum, sync source trait, fuzzy matcher tuned for code
//! completion, and the per-buffer aggregator that holds it all
//! together. The host (`lattice-ui-tui`) owns the async glue
//! (LSP request fan-out, tokio spawn, the popup widget); this
//! crate stays sync + pure-data so plugins can target it
//! without pulling in tokio.
//!
//! ## Two kinds of source
//!
//! - **Sync ([`InsertSource`]).** Buffer-words, snippets, path,
//!   tree-sitter -- everything that runs in microseconds. The
//!   aggregator calls `produce()` directly when the popup query
//!   changes.
//! - **Async (host-orchestrated).** LSP, plugin generators,
//!   anything that round-trips. The host spawns a tokio task
//!   that pushes [`RawCandidate`]s through a channel into
//!   [`InsertCompletionState::raw`]; the aggregator coalesces
//!   pushes on a 16 ms tick and re-runs matcher / ranker.
//!
//! The split keeps this crate dependency-light. Hosts add the
//! async dance; we don't.

use std::collections::HashMap;
use std::ops::Range;

use lattice_core::Buffer;
use lattice_protocol::Position;

use crate::candidate::{RawCandidate, RenderedCandidate, ScoredCandidate};
use crate::traits::{CandidateMatcher, CandidateRanker};

/// Stable identifier for a source. Strings keep the registry
/// transparent (`"gen:lsp-completion"`, `"gen:buffer-words"`,
/// `"plugin:foo"`); the host's per-source priority / enable
/// config keys off this string so users see the same name in
/// `:set completion.source.<id>.priority=…` and in `:help
/// completion-sources`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What opened the popup. Stays constant for the popup's
/// lifetime; rides on LSP completion requests as
/// `CompletionTriggerKind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionTrigger {
    /// User typed a server-advertised character (`'.'`, `'::'`,
    /// etc.). The char rides along so the host's LSP request
    /// builder fills `triggerCharacter`.
    TriggerChar(char),
    /// Auto-trigger threshold reached: the inserted char was an
    /// identifier char and the prefix at the cursor crossed the
    /// `min_chars` config bar. Off by default
    /// (`completion.auto_trigger = false`).
    IdentifierThreshold,
    /// Manual: `<C-x><C-o>` / `<C-Space>` / smart-tab /
    /// `cmd:completion-trigger`. Manual triggers always re-fire
    /// the LSP source even when prior responses were cached.
    Manual,
    /// Server returned `isIncomplete: true` and the user kept
    /// typing -- the host re-fires the LSP source on each
    /// keystroke.
    IncompleteRefresh,
}

/// Snapshot of editor state a source reads when producing
/// candidates. Held by reference so the aggregator borrows from
/// the surrounding frame for the duration of `produce()`.
///
/// Sources that need richer state (the LSP source needs the
/// `ServerHandle`, the snippet source needs the per-language
/// snippet registry) get those out-of-band -- they're not part
/// of the generic context.
pub struct InsertContext<'a> {
    /// Active buffer's rope text. Sources read it via
    /// `buffer.line(line_idx)` etc.
    pub buffer: &'a Buffer,
    /// Cursor position at popup-open time. The popup's
    /// "current word" is `buffer[anchor..cursor]`.
    pub cursor: Position,
    /// Anchor: where the replacement region starts. Same as
    /// `InsertCompletionState::anchor`.
    pub anchor: Position,
    /// Live filter text -- `buffer[anchor..cursor]`. Sources
    /// can use this to filter their own output (snippet source
    /// looks up by prefix, e.g.) but the aggregator's matcher
    /// also filters globally so sources don't strictly need to.
    pub query: &'a str,
    /// What triggered the popup.
    pub trigger: &'a CompletionTrigger,
    /// Whether matching should be case-sensitive. Default
    /// matcher honours this.
    pub case_sensitive: bool,
}

/// One side popup showing the focused candidate's full
/// documentation. Lazy: not opened until `<C-d>` /
/// `cmd:completion-toggle-docs` flips it on.
#[derive(Debug, Clone)]
pub struct DocPopupState {
    /// Index into `InsertCompletionState::rendered` the popup
    /// is showing for. Re-resolves when the selection changes.
    pub for_index: usize,
    /// Resolved markdown body. `Some(empty)` means "we asked
    /// the server but the item has no documentation"; `None`
    /// means "we haven't asked yet" (in which case the popup
    /// renders a placeholder while the resolve fires).
    pub body: Option<String>,
}

/// Live state for an in-flight Insert-mode completion. Held by
/// the host (`App.insert_completion: Option<…>`) while the
/// popup is up; dropped on dismiss. Per-source channels and
/// cancellation tokens live host-side -- they pull in tokio
/// types this crate avoids -- so this struct stays
/// dependency-light enough that any host (TUI today, GPU
/// later) can reuse it.
#[derive(Debug, Clone)]
pub struct InsertCompletionState {
    pub trigger: CompletionTrigger,
    /// Where the replacement region starts. The aggregator's
    /// `query` string is `buffer[anchor..cursor]`.
    pub anchor: Position,
    /// The cursor at open-time -- carried so the host can
    /// detect cursor moves outside `[anchor, cursor]` and
    /// dismiss.
    pub cursor: Position,
    /// Live filter text. Re-derived by the host on every
    /// keystroke from `buffer[anchor..cursor]`.
    pub query: String,
    /// All raw candidates seen so far. Sync sources push their
    /// full output once per query change; async sources push
    /// incrementally as their tokio tasks return.
    pub raw: Vec<RawCandidate>,
    /// Matched + scored + ranked + annotated. Re-derived from
    /// `raw` whenever `query` or `raw` changes.
    pub rendered: Vec<RenderedCandidate>,
    /// Selected index into `rendered`. Sticky across re-rank
    /// when the same candidate is still in the list.
    pub selected: usize,
    /// "Pinned" -- the user moved off the default top selection
    /// at least once. After pinning, refilter doesn't reset to
    /// index 0; we hold the user's latest pick instead.
    pub user_picked: bool,
    /// Documentation popup, when open. `None` means no popup.
    pub doc_popup: Option<DocPopupState>,
    /// Whether the LSP source said `isIncomplete: true` last
    /// time. The host uses this to decide whether to re-fire
    /// LSP on each keystroke.
    pub lsp_incomplete: bool,
}

impl InsertCompletionState {
    /// Open a fresh popup state at `cursor` with the given
    /// trigger. `anchor` is computed by the caller (typically
    /// "scan back to a word boundary"). `query` starts as the
    /// text between `anchor` and `cursor`.
    pub fn open(
        trigger: CompletionTrigger,
        anchor: Position,
        cursor: Position,
        query: String,
    ) -> Self {
        Self {
            trigger,
            anchor,
            cursor,
            query,
            raw: Vec::new(),
            rendered: Vec::new(),
            selected: 0,
            user_picked: false,
            doc_popup: None,
            lsp_incomplete: false,
        }
    }

    /// True if no candidates have been produced yet. The host
    /// renders an empty popup with a "loading…" placeholder
    /// in this state when an async source is in-flight.
    pub fn is_empty(&self) -> bool {
        self.rendered.is_empty()
    }

    /// Currently-selected candidate, if any.
    pub fn selected_candidate(&self) -> Option<&RenderedCandidate> {
        self.rendered.get(self.selected)
    }

    /// Move the selection one step down, wrapping at the end.
    /// Marks `user_picked` so subsequent refilters don't snap
    /// back to index 0.
    pub fn select_next(&mut self) {
        if self.rendered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.rendered.len();
        self.user_picked = true;
    }

    /// Move the selection one step up, wrapping at the start.
    pub fn select_prev(&mut self) {
        if self.rendered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.rendered.len() - 1
        } else {
            self.selected - 1
        };
        self.user_picked = true;
    }
}

/// A sync candidate source. Implementations are cheap to call
/// -- the aggregator invokes `produce()` once per query change
/// (a re-filter on each keystroke). Async sources don't
/// implement this trait; they are orchestrated host-side via
/// `tokio::spawn` + a channel that pushes into
/// `InsertCompletionState.raw` directly.
///
/// Default priority is 100; the host's `[completion.source.<id>]`
/// config can override.
pub trait InsertSource: Send + Sync + std::fmt::Debug {
    /// Stable id (e.g. `"gen:buffer-words"`). Surfaces in
    /// `:set completion.source.<id>.priority=…` keys.
    fn id(&self) -> &SourceId;

    /// Default priority bucket. Higher buckets sort above
    /// lower; the ranker adds it to the matcher's score.
    fn default_priority(&self) -> u32 {
        100
    }

    /// Whether this source contributes when the popup opens
    /// via auto-trigger (identifier threshold). Manual
    /// triggers and `<C-x>`-prefixed dedicated chords always
    /// include every enabled source.
    fn auto_trigger(&self) -> bool {
        true
    }

    /// Server-advertised characters that should fire this
    /// source. Empty = "I'm fine on identifier-threshold or
    /// manual." The LSP source's bridge populates this from
    /// `completionProvider.triggerCharacters`.
    fn trigger_chars(&self, _ctx: &InsertContext<'_>) -> Vec<char> {
        Vec::new()
    }

    /// Produce raw candidates for the supplied context. Cheap
    /// (microseconds); the aggregator calls this whenever the
    /// query changes.
    fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate>;
}

/// Built-in fuzzy matcher tuned for Insert-mode completion.
///
/// Scoring tiers (descending):
///
/// | Tier | Meaning | Score base |
/// |---|---|---|
/// | Exact | `query == text` (case-insensitive) | 1000 |
/// | Prefix | `text.starts_with(query)` (case-insensitive) | 800 |
/// | Word boundary | All query chars match at word boundaries (camelCase / snake_case) | 600 |
/// | Substring | `text.contains(query)` (case-insensitive) | 400 |
/// | Subsequence | Each query char appears in order in text | 200 - skipped × 5 |
///
/// Empty query yields a uniform score of 100 -- everything
/// matches, sorted by per-source priority + ranker bonuses.
///
/// Returns `None` for non-matches (filtered out before the
/// ranker stage).
#[derive(Debug, Default)]
pub struct FuzzyInsertMatcher;

impl FuzzyInsertMatcher {
    pub const ID: &'static str = "match:fuzzy-insert";

    pub fn new() -> Self {
        Self
    }
}

impl CandidateMatcher for FuzzyInsertMatcher {
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(crate::candidate::MatchScore, Vec<Range<usize>>)> {
        let text = &candidate.text;
        if query.is_empty() {
            return Some((crate::candidate::MatchScore(100), Vec::new()));
        }
        // Lowercase folding for the comparisons. The ranges we
        // return are byte ranges into the *original* text.
        let q_lower: String = query.to_lowercase();
        let t_lower: String = text.to_lowercase();
        let q_bytes = q_lower.as_bytes();
        let t_bytes = t_lower.as_bytes();

        // Tier 1: exact (case-insensitive).
        if q_lower == t_lower {
            return Some((
                crate::candidate::MatchScore(1000),
                vec![0..text.len()],
            ));
        }

        // Tier 2: prefix (case-insensitive). Match range is the
        // prefix bytes in the original text.
        if t_lower.starts_with(&q_lower) {
            // The original-text prefix length matches the
            // lowercase prefix length when both are pure-ASCII;
            // for non-ASCII inputs we still highlight up to the
            // shared codepoint count.
            let prefix_len = shared_prefix_byte_len(text, query);
            return Some((
                crate::candidate::MatchScore(800),
                vec![0..prefix_len.max(query.len())],
            ));
        }

        // Tier 3: word-boundary subsequence. Each query char
        // matches at a word boundary (start of text, after a
        // non-alpha, or at a camelCase boundary).
        if let Some(ranges) = match_word_boundary(text, query) {
            return Some((crate::candidate::MatchScore(600), ranges));
        }

        // Tier 4: contiguous substring (case-insensitive).
        if let Some(start) = t_lower.find(&q_lower) {
            return Some((
                crate::candidate::MatchScore(400),
                vec![start..start + q_lower.len()],
            ));
        }

        // Tier 5: subsequence. Walk t with q's characters in
        // order; the score decays with the number of skipped
        // characters between matches.
        let (matched, ranges) = subsequence_match(t_bytes, q_bytes);
        if matched {
            let skipped = text.len().saturating_sub(query.len());
            let score = 200u32.saturating_sub(skipped as u32 * 5);
            return Some((
                crate::candidate::MatchScore(score.max(1)),
                ranges,
            ));
        }

        None
    }
}

fn shared_prefix_byte_len(text: &str, query: &str) -> usize {
    let mut t = text.chars();
    let mut q = query.chars();
    let mut consumed = 0;
    loop {
        match (t.next(), q.next()) {
            (Some(tc), Some(qc)) if tc.eq_ignore_ascii_case(&qc) => {
                consumed += tc.len_utf8();
            }
            _ => return consumed,
        }
    }
}

/// Walk `text` matching `query` characters at word boundaries
/// only. Three boundary conditions:
///
/// - **Separator boundary.** Previous char was a non-identifier
///   character (whitespace, punctuation, etc.), or we're at the
///   start of `text`. Covers `"foo bar".matches("fb")` and
///   `"foo.bar".matches("fb")`.
/// - **Snake-case boundary.** Previous char was `_`. Covers
///   `"foo_bar".matches("fb")` -- `_` is part of the
///   identifier (so `foo_bar` is one word) but the position
///   *after* it is a boundary.
/// - **Camel-case boundary.** This char is uppercase and the
///   previous was lowercase. Covers `"fooBar".matches("fb")`.
///
/// Returns the matched byte ranges in `text` if every query
/// character found a boundary match in order.
fn match_word_boundary(text: &str, query: &str) -> Option<Vec<Range<usize>>> {
    let q_lower: Vec<char> = query.to_lowercase().chars().collect();
    let mut q_iter = q_lower.iter().peekable();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut prev_was_lower = false;
    let mut prev_was_sep = true; // start of text counts as boundary
    let mut prev_was_underscore = false;
    let mut byte_idx = 0;
    for c in text.chars() {
        let len = c.len_utf8();
        let lower = c.to_ascii_lowercase();
        let is_alpha = c.is_alphanumeric() || c == '_';
        let is_sep = !is_alpha;
        let at_boundary = prev_was_sep
            || prev_was_underscore
            || (c.is_ascii_uppercase() && prev_was_lower);
        if at_boundary {
            if let Some(&&want) = q_iter.peek() {
                if want == lower {
                    ranges.push(byte_idx..byte_idx + len);
                    q_iter.next();
                }
            }
        }
        prev_was_sep = is_sep;
        prev_was_lower = c.is_ascii_lowercase();
        prev_was_underscore = c == '_';
        byte_idx += len;
    }
    if q_iter.peek().is_none() {
        Some(ranges)
    } else {
        None
    }
}

/// Subsequence match -- every query byte appears in `text` in
/// order. Returns the per-byte match positions.
fn subsequence_match(t: &[u8], q: &[u8]) -> (bool, Vec<Range<usize>>) {
    let mut ranges = Vec::with_capacity(q.len());
    let mut i = 0;
    let mut j = 0;
    while i < t.len() && j < q.len() {
        if t[i] == q[j] {
            ranges.push(i..i + 1);
            j += 1;
        }
        i += 1;
    }
    (j == q.len(), ranges)
}

/// Built-in ranker tuned for Insert-mode completion. Sorts by
/// `final_score` descending, where:
///
/// ```text
/// final_score = base_score                  // matcher output
///             + per_source_priority         // CandidateData::Plain → no per-source bias
///             + frequency_bonus             // 0–50, host-side LRU
///             + preselect_bonus             // +200 for LSP preselect items
///             - deprecated_penalty          // -100 for deprecated tags
/// ```
///
/// 4.2.g.1 ships only `base_score` (the matcher's tier score).
/// Per-source priority + frequency + preselect + deprecated
/// land in 4.2.g.2 (LSP source) and 4.2.g.5 (ranking polish);
/// the host adds them via the existing `RawCandidate.data`
/// payload before calling the ranker.
#[derive(Debug, Default)]
pub struct InsertRanker;

impl InsertRanker {
    pub const ID: &'static str = "rank:insert";

    pub fn new() -> Self {
        Self
    }
}

impl CandidateRanker for InsertRanker {
    fn rank(&self, scored: &mut Vec<ScoredCandidate>) {
        scored.sort_by(|a, b| b.score.0.cmp(&a.score.0));
    }
}

// ---- Built-in: gen:buffer-words ----

/// Sync source emitting word-completions from a buffer's text.
/// Cheap enough to walk the rope once per query change. Words
/// shorter than `min_word_length` are skipped; duplicates are
/// deduped.
///
/// The cursor's own current word (`query`) is NOT included --
/// no point completing a word with itself.
#[derive(Debug)]
pub struct BufferWordsSource {
    id: SourceId,
    pub min_word_length: usize,
    pub max_words: usize,
}

impl Default for BufferWordsSource {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferWordsSource {
    pub const ID: &'static str = "gen:buffer-words";

    pub fn new() -> Self {
        Self {
            id: SourceId::new(Self::ID),
            min_word_length: 3,
            max_words: 200,
        }
    }
}

impl InsertSource for BufferWordsSource {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn default_priority(&self) -> u32 {
        100
    }

    fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate> {
        // Walk the rope as a string. For very large buffers
        // (> 1 MB) this would be too costly; future phases
        // walk only the visible region first and the rest in
        // a background pass. v1 is the simple shape.
        let text = ctx.buffer.as_string();
        let mut seen: HashMap<String, ()> = HashMap::new();
        let mut out: Vec<RawCandidate> = Vec::new();
        for word in iter_words(&text) {
            if word.len() < self.min_word_length {
                continue;
            }
            // Skip the cursor's own word (avoid completing a
            // word with itself when the user typed the prefix
            // and the buffer already contains that prefix
            // verbatim somewhere).
            if word == ctx.query {
                continue;
            }
            if seen.insert(word.to_string(), ()).is_some() {
                continue;
            }
            out.push(RawCandidate::plain(
                word.to_string(),
                crate::candidate::CandidateKind::Plain,
            ));
            if out.len() >= self.max_words {
                break;
            }
        }
        out
    }
}

/// Walk `text` yielding contiguous identifier-character runs
/// (alphanumeric + `_`). Allocation-free; iteration cost is
/// linear in `text.len()`.
fn iter_words(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    let len = bytes.len();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < len {
            let b = bytes[i];
            let is_word = b.is_ascii_alphanumeric() || b == b'_';
            match (start, is_word) {
                (None, true) => {
                    start = Some(i);
                    i += 1;
                }
                (Some(s), false) => {
                    let word = &text[s..i];
                    start = None;
                    i += 1;
                    return Some(word);
                }
                (None, false) => {
                    i += 1;
                }
                (Some(_), true) => {
                    i += 1;
                }
            }
        }
        if let Some(s) = start.take() {
            return Some(&text[s..len]);
        }
        None
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::candidate::CandidateKind;
    use lattice_core::Buffer;
    use lattice_protocol::Edit;

    fn buffer_with(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        let _ = b.apply_edit(&Edit::insert(Position::ZERO, text));
        b
    }

    fn ctx<'a>(buffer: &'a Buffer, query: &'a str) -> InsertContext<'a> {
        InsertContext {
            buffer,
            cursor: Position::new(0, query.len() as u32),
            anchor: Position::ZERO,
            query,
            trigger: CONTEXT_TRIGGER,
            case_sensitive: false,
        }
    }

    static CONTEXT_TRIGGER: &CompletionTrigger = &CompletionTrigger::Manual;

    #[test]
    fn fuzzy_exact_match_wins_top_score() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("foo", CandidateKind::Plain);
        let (score, ranges) = m.matches("foo", &c).unwrap();
        assert_eq!(score.0, 1000);
        assert_eq!(ranges, vec![0..3]);
    }

    #[test]
    fn fuzzy_exact_is_case_insensitive() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("Foo", CandidateKind::Plain);
        let (score, _) = m.matches("foo", &c).unwrap();
        assert_eq!(score.0, 1000);
    }

    #[test]
    fn fuzzy_prefix_scores_below_exact() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("foobar", CandidateKind::Plain);
        let (score, ranges) = m.matches("foo", &c).unwrap();
        assert_eq!(score.0, 800);
        assert_eq!(ranges, vec![0..3]);
    }

    #[test]
    fn fuzzy_word_boundary_matches_camelcase() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("getFooBar", CandidateKind::Plain);
        // gFB matches at 'g', 'F', 'B' boundaries.
        let (score, ranges) = m.matches("gfb", &c).unwrap();
        assert_eq!(score.0, 600);
        assert_eq!(ranges.len(), 3);
    }

    #[test]
    fn fuzzy_word_boundary_matches_snake_case() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("get_foo_bar", CandidateKind::Plain);
        let (score, _) = m.matches("gfb", &c).unwrap();
        assert_eq!(score.0, 600);
    }

    #[test]
    fn fuzzy_substring_falls_below_word_boundary() {
        let m = FuzzyInsertMatcher::new();
        // "ooba" is a substring but not at a word boundary in
        // "getFooBar".
        let c = RawCandidate::plain("getFooBar", CandidateKind::Plain);
        let (score, _) = m.matches("ooba", &c).unwrap();
        assert_eq!(score.0, 400);
    }

    #[test]
    fn fuzzy_subsequence_matches_when_no_substring() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("alphaBetaGamma", CandidateKind::Plain);
        // "abg" -- subsequence but not substring.
        let (score, _) = m.matches("abg", &c).unwrap();
        // Word-boundary actually matches alpha/Beta/Gamma so
        // tier 3 wins (score 600), not tier 5.
        assert_eq!(score.0, 600);
    }

    #[test]
    fn fuzzy_no_match_returns_none() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("foo", CandidateKind::Plain);
        assert!(m.matches("xyz", &c).is_none());
    }

    #[test]
    fn fuzzy_empty_query_uniform_score() {
        let m = FuzzyInsertMatcher::new();
        let c = RawCandidate::plain("anything", CandidateKind::Plain);
        let (score, ranges) = m.matches("", &c).unwrap();
        assert_eq!(score.0, 100);
        assert!(ranges.is_empty());
    }

    #[test]
    fn buffer_words_returns_unique_words_above_threshold() {
        let buf = buffer_with("hello world hello\nfoo bar foo baz");
        let src = BufferWordsSource::new();
        let words: Vec<String> = src
            .produce(&ctx(&buf, ""))
            .into_iter()
            .map(|c| c.text)
            .collect();
        // "hello", "world", "foo", "bar", "baz" -- in order of
        // first appearance, each unique.
        assert_eq!(words, vec!["hello", "world", "foo", "bar", "baz"]);
    }

    #[test]
    fn buffer_words_skips_words_below_min_length() {
        let buf = buffer_with("ok foo at ax");
        let mut src = BufferWordsSource::new();
        src.min_word_length = 3;
        let words: Vec<String> = src
            .produce(&ctx(&buf, ""))
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert_eq!(words, vec!["foo"]);
    }

    #[test]
    fn buffer_words_skips_cursor_own_word() {
        // The user is typing `foo` and the buffer already
        // contains the literal `foo` at the cursor; we
        // shouldn't surface `foo` as a completion of itself.
        let buf = buffer_with("foo bar baz foo");
        let src = BufferWordsSource::new();
        let words: Vec<String> = src
            .produce(&ctx(&buf, "foo"))
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert_eq!(words, vec!["bar", "baz"]);
    }

    #[test]
    fn iter_words_ignores_punctuation() {
        let s = "foo, bar.baz! qux";
        let words: Vec<&str> = iter_words(s).collect();
        assert_eq!(words, vec!["foo", "bar", "baz", "qux"]);
    }

    #[test]
    fn iter_words_handles_underscores_and_digits() {
        let s = "foo_bar baz123 _under";
        let words: Vec<&str> = iter_words(s).collect();
        assert_eq!(words, vec!["foo_bar", "baz123", "_under"]);
    }

    #[test]
    fn state_select_next_wraps() {
        let mut s = InsertCompletionState::open(
            CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        let cand = |t: &str| {
            crate::candidate::ScoredCandidate {
                raw: RawCandidate::plain(t, CandidateKind::Plain),
                score: crate::candidate::MatchScore(0),
                match_ranges: Vec::new(),
            }
        };
        s.rendered = vec![
            crate::candidate::RenderedCandidate::from_scored(cand("a")),
            crate::candidate::RenderedCandidate::from_scored(cand("b")),
            crate::candidate::RenderedCandidate::from_scored(cand("c")),
        ];
        assert_eq!(s.selected, 0);
        s.select_next();
        assert_eq!(s.selected, 1);
        s.select_next();
        s.select_next();
        // wrapped
        assert_eq!(s.selected, 0);
        assert!(s.user_picked);
    }

    #[test]
    fn state_select_prev_wraps_from_zero_to_last() {
        let mut s = InsertCompletionState::open(
            CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        let cand = |t: &str| {
            crate::candidate::ScoredCandidate {
                raw: RawCandidate::plain(t, CandidateKind::Plain),
                score: crate::candidate::MatchScore(0),
                match_ranges: Vec::new(),
            }
        };
        s.rendered = vec![
            crate::candidate::RenderedCandidate::from_scored(cand("a")),
            crate::candidate::RenderedCandidate::from_scored(cand("b")),
        ];
        assert_eq!(s.selected, 0);
        s.select_prev();
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn ranker_sorts_descending_by_score() {
        let r = InsertRanker::new();
        let mut scored = vec![
            ScoredCandidate {
                raw: RawCandidate::plain("low", CandidateKind::Plain),
                score: crate::candidate::MatchScore(10),
                match_ranges: Vec::new(),
            },
            ScoredCandidate {
                raw: RawCandidate::plain("high", CandidateKind::Plain),
                score: crate::candidate::MatchScore(100),
                match_ranges: Vec::new(),
            },
            ScoredCandidate {
                raw: RawCandidate::plain("mid", CandidateKind::Plain),
                score: crate::candidate::MatchScore(50),
                match_ranges: Vec::new(),
            },
        ];
        r.rank(&mut scored);
        assert_eq!(scored[0].raw.text, "high");
        assert_eq!(scored[1].raw.text, "mid");
        assert_eq!(scored[2].raw.text, "low");
    }
}
