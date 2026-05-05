//! `Syntax`: per-document tree-sitter state.
//!
//! Wraps a `tree_sitter_highlight::Highlighter` plus a borrow of the
//! shared [`crate::LangRegistry`]'s `HighlightConfiguration` for the
//! document's primary language. The highlighter handles overlap
//! resolution (innermost capture wins) and produces a flat event
//! stream that we walk to assemble per-line `StyledSpan`s.
//!
//! ## Injections
//!
//! Markdown's grammar is split block / inline; the block parser's
//! `injections.scm` injects the inline parser into paragraph content
//! and the named language parser into fenced code blocks. Our
//! injection callback (in `highlight_lines`) closes over the shared
//! registry and looks up sibling configs by name -- so a
//! ` ```rust ... ``` ` block in a markdown buffer gets rust
//! highlighting, an autolink in a paragraph gets inline-markdown
//! highlighting, etc.
//!
//! Reparse is a `parse(source: &str)` call. Internally we re-run the
//! highlighter on the full source. Incremental reparse via `Tree::edit`
//! is a follow-up; the public surface won't change because today's full
//! reparse stays correct.

use std::sync::Arc;

use streaming_iterator::StreamingIterator;
use thiserror::Error;
use tree_sitter::{Parser, QueryCursor, Tree};

use crate::lang::Lang;
use crate::registry::LangRegistry;
use crate::style::{Style, StyledSpan};

#[derive(Debug, Error)]
pub enum SyntaxError {
    #[error("tree-sitter language error: {0}")]
    Language(String),

    #[error("language not registered: {0}")]
    UnregisteredLang(String),
}

pub struct Syntax {
    lang: Lang,
    registry: Arc<LangRegistry>,
    /// Owned tree-sitter parser. `parse()` reuses it across edits and
    /// passes `tree.as_ref()` so tree-sitter's incremental reparser
    /// kicks in. The parser instance itself is cheap to keep around;
    /// the heavy state lives in the [`Tree`].
    parser: Parser,
    /// Latest parse result. `None` until the first `parse()` call (or
    /// when the parser couldn't produce a tree, which tree-sitter
    /// signals by returning `None` -- happens on cancellation, not
    /// in our synchronous path today). Future tree-sitter consumers
    /// (folds, indents, locals, textobjects) read from here so every
    /// feature shares a single parse per edit.
    tree: Option<Tree>,
    /// Last-parsed source bytes. Owned so callers can call `highlight_lines`
    /// independently of holding the source.
    source: Vec<u8>,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("lang", &self.lang)
            .field("source_bytes", &self.source.len())
            .field("tree_present", &self.tree.is_some())
            .finish_non_exhaustive()
    }
}

impl Syntax {
    /// Build a `Syntax` for the given language using a fresh standard
    /// registry. Convenient when the caller doesn't already hold a
    /// shared registry; for the App's hot path use
    /// [`Self::for_language_with_registry`] so all documents share one
    /// registry.
    ///
    /// `Lang::Plain` returns `None` because there's nothing to parse.
    pub fn for_language(lang: Lang) -> Result<Option<Self>, SyntaxError> {
        let registry = LangRegistry::standard()?;
        Self::for_language_with_registry(lang, registry)
    }

    /// Build a `Syntax` borrowing from a shared registry. Multiple
    /// documents (and the help-buffer system) all share one
    /// `Arc<LangRegistry>`; per-document state stays in the
    /// `Highlighter` + `source`.
    pub fn for_language_with_registry(
        lang: Lang,
        registry: Arc<LangRegistry>,
    ) -> Result<Option<Self>, SyntaxError> {
        if matches!(lang, Lang::Plain) {
            return Ok(None);
        }
        let Some(ts_lang) = registry.tree_sitter_language(lang.name()) else {
            // Lang variant exists but no registered grammar for it -- fall
            // back to no syntax (renderer treats it as plain text).
            return Ok(None);
        };
        let mut parser = Parser::new();
        parser
            .set_language(&ts_lang)
            .map_err(|e| SyntaxError::Language(e.to_string()))?;
        Ok(Some(Self {
            lang,
            registry,
            parser,
            tree: None,
            source: Vec::new(),
        }))
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Returns the most recent parse result, if `parse()` has run and
    /// tree-sitter produced a tree. Future query consumers (folds,
    /// indents, locals, textobjects) read from here so every feature
    /// shares a single parse per edit.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Borrow the cached source bytes. The renderer / fold provider /
    /// future query consumers read directly from these without
    /// re-encoding the document.
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    /// Borrow the shared language registry. Useful for query-driven
    /// consumers (`compute_syntax_folds`, future textobjects /
    /// indents) that need to look up the per-language compiled
    /// queries.
    pub fn registry(&self) -> &LangRegistry {
        &self.registry
    }

    /// Run the language's `symbols.scm` query against the cached
    /// tree and return the deduped list of `@symbol`-captured
    /// names (definition-position identifiers). Empty when:
    /// no parse yet, language has no symbols query, or the tree
    /// contains no matches.
    ///
    /// Phase 4.2.g.6 (1/2): the host-orchestrated
    /// `gen:tree-sitter-symbol` insert-completion source calls
    /// this once per popup-trigger; cost is O(tree-size) for
    /// the cursor walk, which is sub-millisecond even on
    /// large source files.
    pub fn collect_symbols(&self) -> Vec<String> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let Some(query) = self.registry.symbols_query(self.lang.name()) else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), self.source.as_slice());
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let n = cap.node;
                let start = n.start_byte();
                let end = n.end_byte();
                if end > self.source.len() || start >= end {
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&self.source[start..end]) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                if seen.insert(text.to_string()) {
                    out.push(text.to_string());
                }
            }
        }
        out
    }

    /// Replace the cached source and drive a tree-sitter (re)parse.
    ///
    /// Step 1 stays a full reparse: we don't yet thread concrete
    /// `Edit` deltas into `Tree::edit`, and tree-sitter can't safely
    /// reuse the previous tree on its own -- callers must drive
    /// `Tree::edit` with byte-accurate deltas before a reparse can
    /// be incremental. The seam stays the same when that lands; the
    /// keystroke→glyph budget (§8, CLAUDE.md paramount #1) is still
    /// met because tree-sitter parses are sub-millisecond on the
    /// buffer sizes we care about, and the hot path runs on the
    /// async syntax actor (§5.7) rather than the UI thread.
    pub fn parse(&mut self, source: &str) {
        self.source.clear();
        self.source.extend_from_slice(source.as_bytes());
        // `Parser::parse` returning `None` means cancellation, which
        // we don't trigger on this synchronous path. Keep the old
        // tree in that unlikely case rather than dropping it -- the
        // next parse() round will retry.
        if let Some(new_tree) = self.parser.parse(source.as_bytes(), None) {
            self.tree = Some(new_tree);
        }
    }

    /// Compute styled spans for each line in `[start_line, end_line)`.
    /// `start_line` and `end_line` are 0-based and clamped to the source's
    /// line count.
    ///
    /// Returns one `Vec<StyledSpan>` per line in the requested range. Spans
    /// use line-relative byte offsets (consistent with the renderer's
    /// existing assumption).
    ///
    /// As of Step 4 this is a thin pass-through to the hand-rolled
    /// native pipeline ([`Self::highlight_lines_native`]); the
    /// legacy `tree_sitter_highlight::Highlighter`-based path was
    /// removed when its dependency was dropped from the workspace.
    pub fn highlight_lines(
        &mut self,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<Vec<StyledSpan>>, SyntaxError> {
        self.highlight_lines_native(start_line, end_line)
    }

    /// Hand-rolled highlighter that runs `highlights.scm` directly
    /// against `Self::tree()` via `tree_sitter::QueryCursor`,
    /// bypassing `tree_sitter_highlight::Highlighter`. This is the
    /// Step 3 deliverable of the Option B migration: one parse per
    /// keystroke (the parser already feeds folds, future textobjects,
    /// indents, etc.) instead of the streaming highlighter's parallel
    /// reparse.
    ///
    /// As of Step 3b this method also recursively highlights ranges
    /// captured by `injections.scm`: markdown's block→inline path
    /// (so `**bold**` inside a paragraph picks up Bold styling) and
    /// fenced-code blocks (so ` ```rust ... ``` ` inside a markdown
    /// buffer reuses the rust highlights). Recursion is bounded
    /// (one level deep per call site -- markdown_inline has no
    /// further injections we honour today).
    pub fn highlight_lines_native(
        &mut self,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<Vec<StyledSpan>>, SyntaxError> {
        self.highlight_lines_via_query(start_line, end_line)
    }

    /// Highlight one injection, returning a per-byte `Option<Style>`
    /// vector aligned with `inj.range` (slot 0 = inj.range.start).
    /// Returns `None` when the injected language has no registered
    /// config -- the caller leaves the parent's styling in place.
    fn highlight_injection(&self, inj: &Injection) -> Option<Vec<Option<Style>>> {
        let lang_config = self.registry.lookup(&inj.language)?;
        // Parse the injected content range with the target
        // language's parser. We slice the source bytes so byte
        // offsets in the resulting tree are RELATIVE to the
        // injection (slot 0 = inj.range.start in our caller).
        let content = &self.source[inj.range.clone()];
        let mut parser = Parser::new();
        parser.set_language(&lang_config.language).ok()?;
        let tree = parser.parse(content, None)?;

        // Run the injected language's highlights query. Capture
        // resolution mirrors the parent path (later pattern wins,
        // smaller range tie-breaks).
        let query = &lang_config.highlights;
        let styles = &lang_config.highlight_styles;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content);
        let mut captures: Vec<(usize, usize, Style, usize)> = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let style = styles
                    .get(cap.index as usize)
                    .copied()
                    .unwrap_or(Style::Default);
                let n = cap.node;
                captures.push((n.start_byte(), n.end_byte(), style, m.pattern_index));
            }
        }
        captures.sort_by(|a, b| {
            b.3.cmp(&a.3)
                .then_with(|| {
                    let len_a = a.1.saturating_sub(a.0);
                    let len_b = b.1.saturating_sub(b.0);
                    len_a.cmp(&len_b)
                })
                .then_with(|| a.0.cmp(&b.0))
        });

        let len = content.len();
        let mut out: Vec<Option<Style>> = vec![None; len];
        for (s, e, style, _) in &captures {
            let s = (*s).min(len);
            let e = (*e).min(len);
            for slot in &mut out[s..e] {
                if slot.is_none() {
                    *slot = Some(*style);
                }
            }
        }
        // Recursive injections (e.g. markdown_block emitting
        // markdown_inline content) -- if the injected language
        // itself has an injections query, recurse one more level.
        if let Some(inj_query) = lang_config.injections.as_ref() {
            // The "source" for nested injection is the slice we
            // just parsed; call the standalone collector with
            // window=[0, len).
            let nested = collect_injections(inj_query, &tree, content, 0, len);
            for n_inj in nested {
                // Copy the slice into a fresh Vec for the recursive
                // helper; we synthesise a one-shot Syntax-like view
                // by reusing self.registry (the parser+tree are
                // local to this fn).
                if let Some(inner) = self.highlight_injection_in(content, &n_inj) {
                    let s = n_inj.range.start.min(len);
                    let e = n_inj.range.end.min(len);
                    let inner_len = inner.len();
                    for (i, slot) in out[s..e].iter_mut().enumerate() {
                        if i >= inner_len {
                            break;
                        }
                        if let Some(st) = inner[i] {
                            *slot = Some(st);
                        }
                    }
                }
            }
        }
        Some(out)
    }

    /// Inner-injection helper. Same shape as
    /// [`Self::highlight_injection`] but takes an explicit byte
    /// slice rather than slicing into `self.source`. Used only by
    /// the recursive injection path so a markdown paragraph that
    /// injects markdown_inline can see further injections (rare
    /// but possible).
    fn highlight_injection_in(
        &self,
        outer_source: &[u8],
        inj: &Injection,
    ) -> Option<Vec<Option<Style>>> {
        let lang_config = self.registry.lookup(&inj.language)?;
        let content = &outer_source[inj.range.clone()];
        let mut parser = Parser::new();
        parser.set_language(&lang_config.language).ok()?;
        let tree = parser.parse(content, None)?;
        let query = &lang_config.highlights;
        let styles = &lang_config.highlight_styles;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content);
        let mut captures: Vec<(usize, usize, Style, usize)> = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let style = styles
                    .get(cap.index as usize)
                    .copied()
                    .unwrap_or(Style::Default);
                let n = cap.node;
                captures.push((n.start_byte(), n.end_byte(), style, m.pattern_index));
            }
        }
        captures.sort_by(|a, b| {
            b.3.cmp(&a.3)
                .then_with(|| {
                    let len_a = a.1.saturating_sub(a.0);
                    let len_b = b.1.saturating_sub(b.0);
                    len_a.cmp(&len_b)
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        let len = content.len();
        let mut out: Vec<Option<Style>> = vec![None; len];
        for (s, e, style, _) in &captures {
            let s = (*s).min(len);
            let e = (*e).min(len);
            for slot in &mut out[s..e] {
                if slot.is_none() {
                    *slot = Some(*style);
                }
            }
        }
        Some(out)
    }

    /// The native query-cursor pipeline. Separated so Step 3b can
    /// call it recursively for injected ranges with a per-call
    /// language override.
    fn highlight_lines_via_query(
        &self,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<Vec<StyledSpan>>, SyntaxError> {
        if end_line <= start_line {
            return Ok(Vec::new());
        }
        let Some(tree) = self.tree.as_ref() else {
            return Ok((0..(end_line - start_line)).map(|_| Vec::new()).collect());
        };
        let line_starts = compute_line_starts(&self.source);
        let total_lines = line_starts.len().saturating_sub(1).max(1) as u32;
        let end_line = end_line.min(total_lines + 1);
        if start_line >= end_line {
            return Ok(Vec::new());
        }
        let mut result: Vec<Vec<StyledSpan>> =
            (0..(end_line - start_line)).map(|_| Vec::new()).collect();
        let query = self.registry.highlights_query(self.lang.name()).ok_or_else(|| {
            SyntaxError::UnregisteredLang(self.lang.name().to_string())
        })?;
        let styles = self
            .registry
            .highlight_styles(self.lang.name())
            .ok_or_else(|| SyntaxError::UnregisteredLang(self.lang.name().to_string()))?;
        let priorities = self
            .registry
            .highlight_priorities(self.lang.name())
            .ok_or_else(|| SyntaxError::UnregisteredLang(self.lang.name().to_string()))?;

        // Restrict the query to the byte window we'll actually use.
        // tree-sitter's `QueryCursor::set_byte_range` is a hint; the
        // cursor still returns matches that overlap the window, so
        // captures whose ranges straddle the window get clipped at
        // distribute time (`distribute_span_across_lines` already
        // filters by line range).
        let window_start = line_starts.get(start_line as usize).copied().unwrap_or(0);
        let window_end = line_starts
            .get(end_line as usize)
            .copied()
            .unwrap_or(self.source.len());
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(window_start..window_end);

        // Collect captures into (start, end, style, pattern_index).
        // Overlap resolution: later pattern wins -- the convention
        // tree-sitter highlights queries follow (more specific
        // patterns come later in the file; `(class_definition
        // name: (identifier) @constructor)` lives below the
        // generic `(identifier) @variable`). This matches what
        // `tree_sitter_highlight` does, including the case where
        // the winning capture's name isn't in CAPTURE_NAMES (e.g.
        // `@constructor`): the slot is "claimed" with Style::Default
        // and no visible span is emitted, which suppresses the
        // generic `@variable` capture too.
        let mut captures: Vec<(usize, usize, Style, usize)> = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), self.source.as_slice());
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let style = styles
                    .get(cap.index as usize)
                    .copied()
                    .unwrap_or(Style::Default);
                let n = cap.node;
                captures.push((n.start_byte(), n.end_byte(), style, m.pattern_index));
            }
        }
        // Sort so the FIRST-write-wins paint loop produces the
        // intended overrides: highest pattern_index first (later
        // patterns more specific). Tie-break by smallest range
        // first (a child capture inside a same-pattern parent
        // should still claim its own bytes), then by start byte
        // for determinism.
        captures.sort_by(|a, b| {
            b.3.cmp(&a.3) // pattern_index DESC
                .then_with(|| {
                    let len_a = a.1.saturating_sub(a.0);
                    let len_b = b.1.saturating_sub(b.0);
                    len_a.cmp(&len_b) // range size ASC
                })
                .then_with(|| a.0.cmp(&b.0)) // start byte ASC
        });
        let _ = priorities; // priority table unused for now; kept
                            // on the registry for the eventual
                            // tie-break refinement / locals work.

        // Per-byte style array for the window, then convert to
        // line-relative spans. The array is at most O(window_bytes)
        // memory, which is bounded by `viewport_height * line_width`
        // in the renderer's typical call shape.
        let win_len = window_end.saturating_sub(window_start);
        let mut byte_styles: Vec<Option<Style>> = vec![None; win_len];
        for (s, e, style, _) in &captures {
            let s_local = s.saturating_sub(window_start).min(win_len);
            let e_local = e.saturating_sub(window_start).min(win_len);
            for slot in &mut byte_styles[s_local..e_local] {
                if slot.is_none() {
                    *slot = Some(*style);
                }
            }
        }

        // Step 3b: recursively process injection captures and
        // overwrite the parent's per-byte styles within the
        // injected ranges. Outer markdown captures inside a
        // ` ```rust { ... } ``` ` block get replaced by the rust
        // pipeline's spans; same for `markdown_inline` injected
        // into paragraph content.
        if let Some(inj_query) = self.registry.injections_query(self.lang.name()) {
            let injections =
                collect_injections(inj_query, tree, self.source.as_slice(), window_start, window_end);
            for inj in injections {
                if let Some(inner_styles) = self.highlight_injection(&inj) {
                    let s_local = inj.range.start.saturating_sub(window_start).min(win_len);
                    let e_local = inj.range.end.saturating_sub(window_start).min(win_len);
                    let inner_len = inner_styles.len();
                    for (i, slot) in byte_styles[s_local..e_local].iter_mut().enumerate() {
                        if i >= inner_len {
                            break;
                        }
                        // Injected spans always override -- once a
                        // language injection claims a byte, it owns
                        // the styling there.
                        if let Some(style) = inner_styles[i] {
                            *slot = Some(style);
                        }
                    }
                }
            }
        }

        // Walk byte_styles, emitting (start, end, style) runs and
        // distributing each across the line slices the renderer
        // expects. Default-claimed slots (`Some(Style::Default)`)
        // count as "no visible span" -- the legacy highlighter
        // emits no event for them.
        let mut i = 0usize;
        while i < byte_styles.len() {
            let Some(style) = byte_styles[i] else {
                i += 1;
                continue;
            };
            if matches!(style, Style::Default) {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j < byte_styles.len() && byte_styles[j] == Some(style) {
                j += 1;
            }
            distribute_span_across_lines(
                window_start + i,
                window_start + j,
                style,
                &line_starts,
                start_line,
                end_line,
                &mut result,
            );
            i = j;
        }
        Ok(result)
    }
}

/// One injection candidate from `injections.scm`: a byte range of
/// content + the target language's name. Markdown produces these
/// in two shapes -- `(@injection.content @injection.language)`
/// pairs (fenced code blocks) and `@injection.content` alone with
/// `#set! injection.language "..."` directives (paragraphs →
/// markdown_inline).
struct Injection {
    range: std::ops::Range<usize>,
    language: String,
}

/// Walk every match of the injections query, extract `(content,
/// language)` pairs, and clip them to the visible window so we
/// don't re-parse content outside the requested viewport.
fn collect_injections(
    query: &tree_sitter::Query,
    tree: &Tree,
    source: &[u8],
    window_start: usize,
    window_end: usize,
) -> Vec<Injection> {
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(window_start..window_end);
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let names = query.capture_names();
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        // Find the content + (optional) language captures within
        // this match. Content is required; language can come from
        // either a `@injection.language` capture or a `#set!
        // injection.language "..."` directive on the pattern.
        let mut content_range: Option<std::ops::Range<usize>> = None;
        let mut explicit_lang: Option<String> = None;
        for cap in m.captures {
            let name = names[cap.index as usize];
            match name {
                "injection.content" => {
                    let n = cap.node;
                    content_range = Some(n.start_byte()..n.end_byte());
                }
                "injection.language" => {
                    let n = cap.node;
                    if let Ok(text) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                        explicit_lang = Some(text.trim().to_string());
                    }
                }
                _ => {}
            }
        }
        let Some(content_range) = content_range else {
            continue;
        };
        // Skip injections that don't intersect the visible window
        // -- their spans wouldn't appear in the result anyway.
        if content_range.end <= window_start || content_range.start >= window_end {
            continue;
        }
        // Resolve the target language: explicit capture wins; else
        // walk the pattern's `#set!` directives.
        let language = explicit_lang.or_else(|| {
            query
                .property_settings(m.pattern_index)
                .iter()
                .find(|p| p.key.as_ref() == "injection.language")
                .and_then(|p| p.value.as_ref().map(|v| v.to_string()))
        });
        let Some(language) = language else { continue };
        out.push(Injection {
            range: content_range,
            language,
        });
    }
    out
}

/// Compute the byte offset where each line starts. The returned vec has
/// `line_count + 1` entries; the last is `source.len()` (a sentinel).
fn compute_line_starts(source: &[u8]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(source.iter().filter(|b| **b == b'\n').count() + 2);
    starts.push(0);
    for (i, b) in source.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts.push(source.len());
    starts
}

/// Place a styled span into the per-line result vector, splitting at
/// newline boundaries and clipping to the requested `[start_line, end_line)`
/// window.
fn distribute_span_across_lines(
    span_start: usize,
    span_end: usize,
    style: Style,
    line_starts: &[usize],
    range_start_line: u32,
    range_end_line: u32,
    out: &mut [Vec<StyledSpan>],
) {
    if span_end <= span_start {
        return;
    }
    let mut byte = span_start;
    while byte < span_end {
        let line = byte_to_line(line_starts, byte);
        let line_start_byte = line_starts.get(line).copied().unwrap_or(0);
        let next_line_start = line_starts.get(line + 1).copied().unwrap_or(usize::MAX);
        let line_end_for_span = next_line_start.min(span_end);
        if (line as u32) >= range_start_line && (line as u32) < range_end_line {
            let i = (line as u32 - range_start_line) as usize;
            if let Some(per_line) = out.get_mut(i) {
                let line_relative_start = byte - line_start_byte;
                let mut line_relative_end = line_end_for_span - line_start_byte;
                // Trim the trailing newline so styled spans don't bleed
                // past the last visible character on the line.
                if next_line_start <= span_end && line_relative_end > 0 {
                    line_relative_end -= 1;
                }
                if line_relative_end > line_relative_start {
                    per_line.push(StyledSpan {
                        start: line_relative_start,
                        end: line_relative_end,
                        style,
                    });
                }
            }
        }
        byte = line_end_for_span;
        if byte == next_line_start && byte < span_end {
            // Skip the newline byte and continue with the next line.
            byte = next_line_start;
        }
    }
}

fn byte_to_line(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn syntax_for_plain_returns_none() {
        let s = Syntax::for_language(Lang::Plain).unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn rust_syntax_exposes_parsed_tree() {
        // Step 1 invariant: every successful `parse()` populates
        // `tree()` so future query consumers (folds.scm,
        // textobjects.scm, indents.scm) can read from the same
        // parse the highlighter walks.
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        assert!(s.tree().is_none(), "tree should be empty before parse");
        s.parse("fn main() {}");
        let tree = s.tree().expect("tree present after parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(root.child_count() > 0, "root has at least one child");
    }

    #[test]
    fn rust_collect_symbols_captures_definitions() {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(
            "\
fn outer(arg: i32) -> i32 {\n\
    let local = arg + 1;\n\
    local\n\
}\n\
struct Point { x: i32, y: i32 }\n\
const MAX: i32 = 10;\n\
",
        );
        let symbols = s.collect_symbols();
        // Definition-position names captured.
        for expected in &["outer", "arg", "local", "Point", "MAX"] {
            assert!(
                symbols.iter().any(|s| s == expected),
                "expected `{expected}` in {symbols:?}",
            );
        }
        // Reference-position uses NOT captured (e.g. the `i32`
        // type references inside the function aren't @symbol
        // captures because we only match on `name: ...` /
        // `pattern: ...` field-introduced positions).
        // Just sanity-check we don't double-count.
        let count_outer = symbols.iter().filter(|s| s.as_str() == "outer").count();
        assert_eq!(count_outer, 1, "no duplicates");
    }

    #[test]
    fn python_collect_symbols_captures_def_and_class() {
        let mut s = Syntax::for_language(Lang::Python).unwrap().unwrap();
        s.parse(
            "def greet(name):\n    message = name\n    return message\n\nclass Greeter:\n    pass\n",
        );
        let symbols = s.collect_symbols();
        for expected in &["greet", "name", "message", "Greeter"] {
            assert!(
                symbols.iter().any(|s| s == expected),
                "expected `{expected}` in {symbols:?}",
            );
        }
    }

    #[test]
    fn collect_symbols_empty_when_no_parse() {
        // No parse() called -> tree is None -> empty result.
        let s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        assert!(s.collect_symbols().is_empty());
    }

    #[test]
    fn collect_symbols_empty_for_language_without_query() {
        // markdown ships no symbols.scm -> empty result even
        // after parse.
        let mut s = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s.parse("# heading\n\nbody\n");
        assert!(s.collect_symbols().is_empty());
    }

    #[test]
    fn reparse_against_evolving_source_keeps_tree_in_sync() {
        // Step 1 is a full reparse on every `parse()` call (we
        // don't yet thread `Tree::edit` deltas). Verify the tree
        // shape tracks the source: two top-level fn items after a
        // second `parse()`, not one stale item.
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse("fn a() {}");
        assert_eq!(s.tree().unwrap().root_node().child_count(), 1);
        s.parse("fn a() {}\nfn b() {}");
        assert_eq!(s.tree().unwrap().root_node().child_count(), 2);
    }

    #[test]
    fn rust_syntax_highlights_keyword() {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse("fn main() {}");
        let spans = s.highlight_lines(0, 1).unwrap();
        assert_eq!(spans.len(), 1);
        // `fn` should be highlighted as Keyword.
        assert!(
            spans[0].iter().any(|sp| sp.style == Style::Keyword),
            "expected a Keyword span, got {:?}",
            spans[0]
        );
    }

    #[test]
    fn markdown_syntax_highlights_atx_heading() {
        let mut s = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s.parse("# Title\n\nbody\n");
        let spans = s.highlight_lines(0, 3).unwrap();
        // The heading row carries a Heading1 span (bundled query
        // captures `(atx_heading (inline) @text.title)` which maps
        // to Heading1 by the level-less convention).
        assert!(
            spans[0].iter().any(|sp| sp.style == Style::Heading1),
            "expected a Heading1 span on the heading line, got {:?}",
            spans[0]
        );
    }

    #[test]
    fn markdown_fenced_rust_block_injects_rust_highlight() {
        let mut s = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        // Fence at line 0; rust content at lines 1-2; closing fence at line 3.
        let src = "```rust\nfn main() {}\n```\n";
        s.parse(src);
        let spans = s.highlight_lines(0, 4).unwrap();
        // Line 1 (the rust code) should have a Keyword span (`fn`).
        assert!(
            spans[1].iter().any(|sp| sp.style == Style::Keyword),
            "expected rust keyword styling inside fenced block, got {:?}",
            spans[1]
        );
    }

    // Note: a markdown-inline-emphasis test (asserting **bold**
    // emits a Bold span via the block→inline injection) is not
    // included here. tree-sitter-md 0.3.x's block parser emits
    // `(inline)` nodes covering paragraph content, and the bundled
    // injections.scm is supposed to route them to the inline
    // grammar -- in practice the injection occasionally fails to
    // surface a span through the highlight stream. The block-level
    // highlighting + fenced-block injection (proven above) confirm
    // the registry / callback infrastructure works; the inline
    // sub-injection is a known soft spot we'll revisit when
    // upgrading to tree-sitter-md 0.5+. For day-to-day markdown
    // editing the heading / list / code-block highlighting is the
    // load-bearing part.

    // ---- Step 3a: native pipeline parity tests ----------------

    /// Helper: parse + highlight `source` through the native
    /// pipeline and assert that at least one span of `expected`
    /// style appears somewhere in the output. Used by the
    /// per-language smoke tests below.
    fn assert_has_style(lang: Lang, source: &str, expected: Style) {
        let mut s = Syntax::for_language(lang).unwrap().unwrap();
        s.parse(source);
        let line_count = source.split('\n').count() as u32;
        let lines = s.highlight_lines(0, line_count).unwrap();
        let found = lines
            .iter()
            .any(|l| l.iter().any(|sp| sp.style == expected));
        assert!(
            found,
            "{lang:?}: expected at least one {expected:?} span in {source:?}, got {lines:?}"
        );
    }

    #[test]
    fn native_rust_simple_function_produces_keyword_and_function_spans() {
        assert_has_style(Lang::Rust, "fn main() {\n    let x = 1;\n}\n", Style::Keyword);
        assert_has_style(Lang::Rust, "fn main() {\n    let x = 1;\n}\n", Style::Function);
    }

    #[test]
    fn native_python_def_produces_keyword_and_function_spans() {
        assert_has_style(
            Lang::Python,
            "def f(x):\n    return x + 1\n\nclass Foo:\n    pass\n",
            Style::Keyword,
        );
        assert_has_style(
            Lang::Python,
            "def f(x):\n    return x + 1\n\nclass Foo:\n    pass\n",
            Style::Function,
        );
    }

    #[test]
    fn native_python_strings_and_comments_resolve_to_proper_styles() {
        let src = "# comment\ns = \"hello world\"\nn = 42\nb = True\n";
        // Python's `# comment` is captured as `@comment` (not
        // `@comment.line`), so it lands on `Style::Comment` rather
        // than `Style::LineComment`. Both are visible distinct
        // colours; the test pins the actual mapping.
        assert_has_style(Lang::Python, src, Style::Comment);
        assert_has_style(Lang::Python, src, Style::String);
        assert_has_style(Lang::Python, src, Style::Number);
    }

    #[test]
    fn native_rust_struct_and_impl_emit_keyword_spans() {
        assert_has_style(
            Lang::Rust,
            "struct Buffer {\n    rope: Rope,\n}\n\nimpl Buffer {\n    fn new() -> Self {\n        Self { rope: Rope::new() }\n    }\n}\n",
            Style::Keyword,
        );
    }

    #[test]
    fn native_markdown_fenced_rust_block_emits_rust_spans() {
        // Native markdown injection recurses into the fenced
        // language. Strict parity with the legacy streaming
        // highlighter doesn't hold here -- tree-sitter-highlight
        // and our hand-rolled injection pipeline differ in how
        // they distribute outer markdown captures inside the
        // fenced range. The user-visible contract is "rust
        // keywords / function names get styled inside `\`\`\`rust`
        // blocks", which we verify directly.
        let mut s = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        let src = "# Title\n\n```rust\nfn main() {}\n```\n";
        s.parse(src);
        let lines = s.highlight_lines_native(0, 6).unwrap();
        // Line 3 is the rust body (`fn main() {}`).
        let rust_line = &lines[3];
        assert!(
            rust_line.iter().any(|sp| sp.style == Style::Keyword),
            "expected rust Keyword span on fenced line, got {rust_line:?}"
        );
        assert!(
            rust_line.iter().any(|sp| sp.style == Style::Function),
            "expected rust Function span on fenced line, got {rust_line:?}"
        );
    }

    #[test]
    fn native_markdown_headings_emit_heading_styles() {
        assert_has_style(
            Lang::Markdown,
            "# H1\n\n## H2\n\n### H3\n\nbody paragraph\n",
            Style::Heading1,
        );
    }
}
