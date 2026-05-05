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
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
    /// Per-row scroll offset into `body` -- `<C-f>` / `<C-b>`
    /// page through long markdown bodies. Reset when
    /// `for_index` changes.
    pub scroll: u32,
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
        // The trait matches on `candidate.text` -- the canonical
        // searchable form. Surfaces that want to match on a
        // different field (e.g. picker rows whose `text` carries
        // routing payload separate from the user-visible label)
        // call [`fuzzy_match`] directly with their target string.
        fuzzy_match(query, &candidate.text)
    }
}

/// Free-function fuzzy match -- the algorithm beneath
/// [`FuzzyInsertMatcher`]. Exposed so surfaces that need to
/// match on a string other than `RawCandidate.text` (e.g. the
/// vertico picker, which matches on the user-visible `display`
/// while `text` carries a routing payload) get the same
/// 5-tier scoring without duplicating the algorithm.
///
/// Empty `query` yields a uniform score of 100; non-matches
/// return `None`. Returned byte ranges are into `target`.
pub fn fuzzy_match(
    query: &str,
    target: &str,
) -> Option<(crate::candidate::MatchScore, Vec<Range<usize>>)> {
    if query.is_empty() {
        return Some((crate::candidate::MatchScore(100), Vec::new()));
    }
    let q_lower: String = query.to_lowercase();
    let t_lower: String = target.to_lowercase();
    let q_bytes = q_lower.as_bytes();
    let t_bytes = t_lower.as_bytes();

    // Tier 1: exact (case-insensitive).
    if q_lower == t_lower {
        return Some((
            crate::candidate::MatchScore(1000),
            vec![0..target.len()],
        ));
    }

    // Tier 2: prefix (case-insensitive).
    if t_lower.starts_with(&q_lower) {
        let prefix_len = shared_prefix_byte_len(target, query);
        return Some((
            crate::candidate::MatchScore(800),
            vec![0..prefix_len.max(query.len())],
        ));
    }

    // Tier 3: word-boundary subsequence (camelCase /
    // snake_case / after-separator).
    if let Some(ranges) = match_word_boundary(target, query) {
        return Some((crate::candidate::MatchScore(600), ranges));
    }

    // Tier 4: contiguous substring.
    if let Some(start) = t_lower.find(&q_lower) {
        return Some((
            crate::candidate::MatchScore(400),
            vec![start..start + q_lower.len()],
        ));
    }

    // Tier 5: subsequence with skip-decay.
    let (matched, ranges) = subsequence_match(t_bytes, q_bytes);
    if matched {
        let skipped = target.len().saturating_sub(query.len());
        let score = 200u32.saturating_sub(skipped as u32 * 5);
        return Some((
            crate::candidate::MatchScore(score.max(1)),
            ranges,
        ));
    }

    None
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

    /// Cap on the per-item frequency bonus, per
    /// `docs/insert-completion.md` §3.6.
    pub const FREQUENCY_BONUS_CAP: u32 = 50;

    pub fn new() -> Self {
        Self
    }

    /// Rank by `score + bonus(raw)`, descending. The host
    /// owns the bonus composition: per
    /// `docs/insert-completion.md` §3.6,
    ///
    /// ```text
    /// final_score = base_score
    ///             + per_source_priority
    ///             + frequency_bonus       // capped at FREQUENCY_BONUS_CAP
    ///             + preselect_bonus
    ///             - deprecated_penalty
    /// ```
    ///
    /// Each term has its own clamp; the ranker doesn't apply
    /// policy because the per-term ranges and signs differ
    /// (frequency is bounded; preselect is a fixed jolt;
    /// deprecated subtracts). Hosts wrap the live lookups
    /// (`App::completion_accept_freq`,
    /// `App::priority_for_source`, ...) into a single bonus
    /// closure.
    pub fn rank_with_bonus(
        &self,
        scored: &mut Vec<ScoredCandidate>,
        bonus: impl Fn(&RawCandidate) -> u32,
    ) {
        scored.sort_by_cached_key(|s| {
            std::cmp::Reverse(s.score.0.saturating_add(bonus(&s.raw)))
        });
    }
}

impl CandidateRanker for InsertRanker {
    fn rank(&self, scored: &mut Vec<ScoredCandidate>) {
        scored.sort_by(|a, b| b.score.0.cmp(&a.score.0));
    }
}

// ---- Per-language overrides ----

/// Per-language overrides for the insert-completion popup. Each
/// field is `Option` so a TOML override at
/// `[completion.per-language.<lang>]` can flip exactly the keys
/// it cares about; unset fields fall back to the global typed
/// option (or, for `sources`, "every enabled source contributes").
///
/// The host (App) layers a TOML override on top of the spec-
/// driven defaults from [`per_language_defaults`]; the
/// effective-config resolver in the host walks
/// `per_language -> global option -> hardcoded fallback` for
/// every read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerLanguageOverrides {
    /// Subset of source ids that contribute for this language.
    /// `None` = every enabled source contributes (the global
    /// default). The producer layer skips emit + LSP fan-out
    /// for sources outside this list.
    pub sources: Option<Vec<SourceId>>,
    /// Whether typing identifier chars opens the popup
    /// automatically. `None` = inherit
    /// `completion.auto_trigger`. Plumbed today; auto-trigger
    /// firing itself lands later.
    pub auto_trigger: Option<bool>,
    /// Whether a single-candidate popup auto-accepts. `None` =
    /// inherit `completion.auto_insert_single`.
    pub auto_insert_single: Option<bool>,
    /// Tree-sitter scope strings (e.g. `"string"`, `"comment"`)
    /// where the popup should not fire. `None` = inherit; empty
    /// list = "fire everywhere." Plumbed today; scope-detect
    /// enforcement lands with the tree-sitter scope queries
    /// slice.
    pub suppress_in: Option<Vec<String>>,
}

impl PerLanguageOverrides {
    /// Layer `other` on top of `self`: every `Some` field in
    /// `other` wins; `None` fields preserve `self`'s value.
    /// Used when a TOML override merges onto the spec defaults.
    pub fn merge(&mut self, other: PerLanguageOverrides) {
        if other.sources.is_some() {
            self.sources = other.sources;
        }
        if other.auto_trigger.is_some() {
            self.auto_trigger = other.auto_trigger;
        }
        if other.auto_insert_single.is_some() {
            self.auto_insert_single = other.auto_insert_single;
        }
        if other.suppress_in.is_some() {
            self.suppress_in = other.suppress_in;
        }
    }
}

/// Spec-driven defaults shipping with v1
/// (`docs/insert-completion.md` §9). Markdown / text restrict
/// to snippet + buffer-words (no LSP for prose); rust enables
/// auto-fire + auto-insert-single since rust-analyzer's items
/// are precise.
///
/// Returned as a fresh map -- callers (App init) own the data
/// and merge TOML overrides on top.
pub fn per_language_defaults() -> std::collections::HashMap<String, PerLanguageOverrides> {
    let mut m = std::collections::HashMap::new();
    let prose_sources = vec![
        SourceId::new(SNIPPET_SOURCE_ID),
        SourceId::new(BufferWordsSource::ID),
    ];
    m.insert(
        "markdown".into(),
        PerLanguageOverrides {
            sources: Some(prose_sources.clone()),
            auto_trigger: Some(false),
            ..Default::default()
        },
    );
    m.insert(
        "text".into(),
        PerLanguageOverrides {
            sources: Some(prose_sources),
            ..Default::default()
        },
    );
    m.insert(
        "rust".into(),
        PerLanguageOverrides {
            auto_trigger: Some(true),
            auto_insert_single: Some(true),
            ..Default::default()
        },
    );
    m
}

/// Map a user-friendly source label (`"lsp"`, `"snippet"`,
/// `"buffer-words"`) to its canonical source id. Unknown labels
/// pass through as-is, so plugin sources can be referenced by
/// their full id (`"plugin:my-source"`).
pub fn canonical_source_id(label: &str) -> SourceId {
    match label {
        "lsp" => SourceId::new(LSP_COMPLETION_SOURCE_ID),
        "snippet" | "snippets" => SourceId::new(SNIPPET_SOURCE_ID),
        "buffer-words" | "buffer_words" | "words" => {
            SourceId::new(BufferWordsSource::ID)
        }
        // `path` (4.2.g.6) and `tree-sitter` (4.2.g.6) are
        // recognised here so users can list them in TOML
        // ahead of the source landing -- the producer skips
        // unknown ids gracefully.
        "path" => SourceId::new(PATH_SOURCE_ID),
        "tree-sitter" | "treesitter" | "ts" => SourceId::new(TREE_SITTER_SYMBOL_SOURCE_ID),
        other => SourceId::new(other),
    }
}

// ---- Source id constants (host-orchestrated sources) ----
//
// `BufferWordsSource::ID` lives on the impl below; the LSP and
// snippet sources are orchestrated host-side (LSP via the
// async tokio path, snippet via the app's per-language
// registry) and have no `InsertSource` struct to hang an `ID`
// off. Surface their canonical ids here so the host's tagging
// matches the strings used in `:set
// completion.source.<id>.priority=…` and in `:help
// completion-sources`.

/// Source id for the host-orchestrated LSP completion source.
pub const LSP_COMPLETION_SOURCE_ID: &str = "gen:lsp-completion";

/// Source id for the host-orchestrated snippet completion source.
pub const SNIPPET_SOURCE_ID: &str = "gen:snippet";

/// Source id for the host-orchestrated tree-sitter local-symbol
/// source (Phase 4.2.g.6 (1/2)). Walks the buffer's syntax tree
/// per popup-trigger via `lattice_syntax::Syntax::collect_symbols`.
pub const TREE_SITTER_SYMBOL_SOURCE_ID: &str = "gen:tree-sitter-symbol";

/// Source id for the host-orchestrated path-completion source
/// (Phase 4.2.g.6 (2/2)). Triggered when the cursor sits inside
/// a string literal (per tree-sitter scope detection); walks
/// the directory of the partial path and emits filesystem
/// entries.
pub const PATH_SOURCE_ID: &str = "gen:path";

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
            out.push(
                RawCandidate::plain(
                    word.to_string(),
                    crate::candidate::CandidateKind::Plain,
                )
                .with_source(self.id().clone()),
            );
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

    #[test]
    fn rank_with_bonus_lifts_higher_bonus_above_tied_peer() {
        // Two candidates tied on matcher score; the one with
        // the larger host-supplied bonus sorts above the peer.
        let r = InsertRanker::new();
        let mut scored = vec![
            ScoredCandidate {
                raw: RawCandidate::plain("alpha", CandidateKind::Plain),
                score: crate::candidate::MatchScore(100),
                match_ranges: Vec::new(),
            },
            ScoredCandidate {
                raw: RawCandidate::plain("bravo", CandidateKind::Plain),
                score: crate::candidate::MatchScore(100),
                match_ranges: Vec::new(),
            },
        ];
        r.rank_with_bonus(&mut scored, |raw| match raw.text.as_str() {
            "bravo" => 5,
            _ => 0,
        });
        assert_eq!(scored[0].raw.text, "bravo");
        assert_eq!(scored[1].raw.text, "alpha");
    }

    #[test]
    fn rank_with_bonus_zero_bonus_matches_plain_rank() {
        // With every lookup returning 0, behaviour matches the
        // plain `rank` call: pure descending sort by score.
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
        ];
        r.rank_with_bonus(&mut scored, |_| 0);
        assert_eq!(scored[0].raw.text, "high");
        assert_eq!(scored[1].raw.text, "low");
    }

    #[test]
    fn rank_with_bonus_respects_host_supplied_cap() {
        // Host caps the frequency bonus at FREQUENCY_BONUS_CAP
        // (50) before passing it in; even a huge raw count
        // can't overtake a much higher base score.
        let r = InsertRanker::new();
        let mut scored = vec![
            ScoredCandidate {
                raw: RawCandidate::plain("rare-but-strong", CandidateKind::Plain),
                score: crate::candidate::MatchScore(200),
                match_ranges: Vec::new(),
            },
            ScoredCandidate {
                raw: RawCandidate::plain("frequent-but-weak", CandidateKind::Plain),
                score: crate::candidate::MatchScore(100),
                match_ranges: Vec::new(),
            },
        ];
        let raw_count = 9999_u32;
        r.rank_with_bonus(&mut scored, |raw| {
            if raw.text == "frequent-but-weak" {
                raw_count.min(InsertRanker::FREQUENCY_BONUS_CAP)
            } else {
                0
            }
        });
        // 100 + 50 = 150 < 200, so strong stays first.
        assert_eq!(scored[0].raw.text, "rare-but-strong");
        assert_eq!(scored[1].raw.text, "frequent-but-weak");
    }

    #[test]
    fn per_language_overrides_merge_replaces_only_some_fields() {
        let mut base = PerLanguageOverrides {
            sources: Some(vec![SourceId::new("a")]),
            auto_trigger: Some(false),
            auto_insert_single: Some(false),
            suppress_in: Some(vec!["string".into()]),
        };
        let overlay = PerLanguageOverrides {
            auto_trigger: Some(true),
            ..Default::default()
        };
        base.merge(overlay);
        // Only auto_trigger flipped; the others survive.
        assert_eq!(base.auto_trigger, Some(true));
        assert_eq!(base.auto_insert_single, Some(false));
        assert_eq!(base.sources, Some(vec![SourceId::new("a")]));
        assert_eq!(base.suppress_in, Some(vec!["string".into()]));
    }

    #[test]
    fn per_language_defaults_match_spec_examples() {
        let m = per_language_defaults();
        let md = m.get("markdown").expect("markdown default");
        assert_eq!(md.auto_trigger, Some(false));
        let md_sources = md.sources.as_ref().expect("markdown sources set");
        assert!(md_sources.iter().any(|s| s.as_str() == SNIPPET_SOURCE_ID));
        assert!(
            md_sources
                .iter()
                .any(|s| s.as_str() == BufferWordsSource::ID),
        );
        assert!(
            !md_sources
                .iter()
                .any(|s| s.as_str() == LSP_COMPLETION_SOURCE_ID),
            "markdown drops LSP per spec",
        );
        let rust = m.get("rust").expect("rust default");
        assert_eq!(rust.auto_trigger, Some(true));
        assert_eq!(rust.auto_insert_single, Some(true));
    }

    #[test]
    fn canonical_source_id_maps_short_labels() {
        assert_eq!(
            canonical_source_id("lsp").as_str(),
            LSP_COMPLETION_SOURCE_ID,
        );
        assert_eq!(canonical_source_id("snippet").as_str(), SNIPPET_SOURCE_ID);
        assert_eq!(
            canonical_source_id("snippets").as_str(),
            SNIPPET_SOURCE_ID,
        );
        assert_eq!(
            canonical_source_id("buffer-words").as_str(),
            BufferWordsSource::ID,
        );
        assert_eq!(
            canonical_source_id("words").as_str(),
            BufferWordsSource::ID,
        );
        // Unknown label passes through verbatim so plugin
        // sources work by full id.
        assert_eq!(
            canonical_source_id("plugin:my-source").as_str(),
            "plugin:my-source",
        );
    }

    #[test]
    fn rank_with_bonus_combines_priority_and_frequency_terms() {
        // Spec §3.6 stacks per-source priority on top of the
        // frequency bonus. Demonstrate that the host can roll
        // both into one closure: high-priority source wins at
        // tied matcher score, even when the low-priority side
        // has a larger frequency count.
        let r = InsertRanker::new();
        let lsp_src = SourceId::new("gen:lsp-completion");
        let words_src = SourceId::new("gen:buffer-words");
        let mut scored = vec![
            ScoredCandidate {
                raw: RawCandidate::plain("from_lsp", CandidateKind::Plain)
                    .with_source(lsp_src.clone()),
                score: crate::candidate::MatchScore(100),
                match_ranges: Vec::new(),
            },
            ScoredCandidate {
                raw: RawCandidate::plain("from_words", CandidateKind::Plain)
                    .with_source(words_src.clone()),
                score: crate::candidate::MatchScore(100),
                match_ranges: Vec::new(),
            },
        ];
        r.rank_with_bonus(&mut scored, |raw| {
            let priority = match raw.source.as_ref().map(|s| s.as_str()) {
                Some("gen:lsp-completion") => 200,
                Some("gen:buffer-words") => 100,
                _ => 0,
            };
            let freq = if raw.text == "from_words" {
                InsertRanker::FREQUENCY_BONUS_CAP
            } else {
                0
            };
            priority + freq
        });
        // LSP: 100 + 200 = 300; words: 100 + 100 + 50 = 250.
        // LSP wins despite the freq lift.
        assert_eq!(scored[0].raw.text, "from_lsp");
    }
}
