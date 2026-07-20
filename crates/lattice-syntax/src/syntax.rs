//! `Syntax`: per-document tree-sitter state.
//!
//! Owns a `tree_sitter::Parser` + the latest cached `Tree` plus a
//! shared [`crate::LangRegistry`] for the document's primary
//! language and any injection targets. The hand-rolled native
//! pipeline runs `highlights.scm` directly via
//! `tree_sitter::QueryCursor`, walks each match into per-line
//! `StyledSpan`s, and recursively highlights ranges captured by
//! `injections.scm`.
//!
//! ## Reparse
//!
//! Two entry points (slice B.2):
//!
//! - [`Syntax::parse_at`]: full reparse. Used for cold-start /
//!   file-load / fallback. `Parser::parse(bytes, None)`.
//! - [`Syntax::parse_at_with_edits`]: incremental reparse.
//!   Applies each `EditDelta` to the cached tree via
//!   `tree.edit()` then `Parser::parse(bytes, Some(&old_tree))`,
//!   so tree-sitter reuses unchanged subtrees. Falls back to
//!   full reparse if any guard fails (no cached tree,
//!   `from_version` mismatch, or post-edit byte-length mismatch
//!   between accumulated deltas and new source).
//!
//! Both methods stamp the resulting snapshot with a caller-
//! supplied `text_version` so consumers (renderer / fold provider
//! / completion) can compare freshness against
//! `DocumentSnapshot::text_version`.
//!
//! ## Injections
//!
//! Markdown's grammar is split block / inline; the block parser's
//! `injections.scm` injects the inline parser into paragraph
//! content and the named language parser into fenced code blocks.
//! Our injection callback (in `highlight_lines`) closes over the
//! shared registry and looks up sibling configs by name -- so a
//! ` ```rust ... ``` ` block in a markdown buffer gets rust
//! highlighting, an autolink in a paragraph gets inline-markdown
//! highlighting, etc.

use std::sync::Arc;

use streaming_iterator::StreamingIterator;
use thiserror::Error;
use tree_sitter::{InputEdit, Parser, Point, QueryCursor, Tree};

use lattice_protocol::edit::EditDelta;

use crate::lang::Lang;
use crate::registry::LangRegistry;
use crate::style::{Style, StyledSpan};

/// Convert a [`lattice_protocol::edit::EditDelta`] (parser-agnostic
/// edit shape) to a [`tree_sitter::InputEdit`] (parser-shaped edit
/// the cached tree mutates by). Six casts + a struct constructor;
/// runs in the noise floor (~1ns).
///
/// Free function rather than `From` impl because both types are
/// foreign to this crate -- Rust's orphan rule blocks the trait
/// impl. Lives in `lattice-syntax` (not `lattice-protocol`) so the
/// protocol crate stays parser-agnostic. The fields map 1:1:
/// `Position.line` -> `Point.row`, `Position.byte` -> `Point.column`
/// (both are byte-within-line, despite the column-named field).
pub fn edit_delta_to_input_edit(d: EditDelta) -> InputEdit {
    InputEdit {
        start_byte: d.start_byte as usize,
        old_end_byte: d.old_end_byte as usize,
        new_end_byte: d.new_end_byte as usize,
        start_position: Point {
            row: d.start_position.line as usize,
            column: d.start_position.byte as usize,
        },
        old_end_position: Point {
            row: d.old_end_position.line as usize,
            column: d.old_end_position.byte as usize,
        },
        new_end_position: Point {
            row: d.new_end_position.line as usize,
            column: d.new_end_position.byte as usize,
        },
    }
}

#[derive(Debug, Error)]
pub enum SyntaxError {
    #[error("tree-sitter language error: {0}")]
    Language(String),

    #[error("language not registered: {0}")]
    UnregisteredLang(String),
}

/// Read-only view of one parse result.
///
/// Holds everything downstream consumers (renderer / folds /
/// completion / picker) need to compute highlights, walk the
/// tree, or query symbols -- and nothing they don't. Cheap to
/// clone (every non-trivial field is `Arc`-shareable: `Tree` is
/// internally Arc'd by tree-sitter; `source` is `Arc<[u8]>`;
/// `registry` is `Arc<LangRegistry>`).
///
/// Lives behind an `ArcSwap<SyntaxSnapshot>` inside
/// [`crate::SyntaxHandle`] so the render thread reads the
/// latest parse at hardware-floor speed while a worker task
/// runs the next reparse off the UI thread (paramount goal #1).
#[derive(Clone)]
pub struct SyntaxSnapshot {
    lang: Lang,
    registry: Arc<LangRegistry>,
    /// Last-parsed source bytes. `Arc<[u8]>` so cloning a
    /// snapshot doesn't copy the buffer.
    source: Arc<[u8]>,
    /// H.3d (2026-06-04): memoized line→byte start table for
    /// `source` (`line_starts[i]` = byte offset of line `i`; final
    /// entry = `source.len()`). Recomputed once per source mutation
    /// (via [`Self::set_source_bytes`]) instead of on every
    /// `highlight_lines` call. The per-call rescan was an O(file)
    /// term that defeated viewport-scoped highlight on large files
    /// (it ran two full passes over the whole source even when the
    /// query only needed a viewport window) — caught by the
    /// `cells_worker_windowed_build` bench. `Arc<[usize]>` so cloning
    /// a snapshot stays cheap.
    line_starts: Arc<[usize]>,
    /// Latest parse result. `None` until the first parse has
    /// run. `Tree` is internally Arc'd by tree-sitter, so
    /// cloning is cheap.
    tree: Option<Tree>,
    /// Document `text_version` this snapshot reflects. The
    /// async path uses this to skip republishing identical
    /// state.
    text_version: u64,
    /// H.2 (2026-06-04): inclusive source-line ranges whose syntax tree
    /// differs from the snapshot this one was reparsed FROM
    /// (`reparsed_from_version`), via `Tree::changed_ranges`. `None` =
    /// full parse / unknown → consumers must treat the whole file as
    /// dirty. `Some(empty)` = nothing changed. The cells worker uses this
    /// to rebuild only the dirty rows on a reparse-completion republish —
    /// but ONLY when its cached matrix's syntax version equals
    /// `reparsed_from_version` (else the delta doesn't apply and it
    /// full-rebuilds).
    changed_lines: Option<Vec<(u32, u32)>>,
    /// The `text_version` this snapshot's tree was reparsed FROM — i.e.
    /// the version `changed_lines` is the delta against. Meaningful only
    /// when `changed_lines` is `Some`.
    reparsed_from_version: u64,
}

impl std::fmt::Debug for SyntaxSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxSnapshot")
            .field("lang", &self.lang)
            .field("source_bytes", &self.source.len())
            .field("tree_present", &self.tree.is_some())
            .field("text_version", &self.text_version)
            .finish_non_exhaustive()
    }
}

pub struct Syntax {
    /// Owned tree-sitter parser. `parse()` reuses it across edits
    /// and passes the previous tree so tree-sitter's incremental
    /// reparser kicks in. The parser instance itself is cheap to
    /// keep around; the heavy state lives in the [`Tree`].
    parser: Parser,
    /// Read-only state -- exposed via [`Self::snapshot`] for
    /// callers that want to share it cheaply (this is what
    /// [`crate::SyntaxHandle`] publishes via `ArcSwap`).
    inner: SyntaxSnapshot,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("inner", &self.inner)
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
            parser,
            inner: SyntaxSnapshot {
                lang,
                registry,
                source: Arc::from(Vec::<u8>::new()),
                // H.3d: line table for the (empty) initial source.
                line_starts: Arc::from(compute_line_starts(&[])),
                tree: None,
                text_version: 0,
                changed_lines: None,
                reparsed_from_version: 0,
            },
        }))
    }

    /// Borrow the read-only snapshot of the latest parse state.
    /// Callers that want to share the snapshot cheaply (across
    /// threads, into `ArcSwap`) clone it; readers that just need
    /// one method call use the pass-through helpers below.
    pub fn snapshot(&self) -> &SyntaxSnapshot {
        &self.inner
    }

    /// Owned clone of the snapshot. Cheap (Arc-shareable
    /// fields).
    pub fn snapshot_owned(&self) -> SyntaxSnapshot {
        self.inner.clone()
    }
}

impl Syntax {
    /// Convenience getter for `self.snapshot().lang()`. Kept on
    /// `Syntax` so `&Syntax` callers (help-buffer markdown
    /// highlighting, tests) don't have to thread `.snapshot()`
    /// through.
    pub fn lang(&self) -> Lang {
        self.inner.lang()
    }

    /// Convenience getter for `self.snapshot().tree()`.
    pub fn tree(&self) -> Option<&Tree> {
        self.inner.tree()
    }

    /// Convenience getter for `self.snapshot().source()`.
    pub fn source(&self) -> &[u8] {
        self.inner.source()
    }

    /// Convenience getter for `self.snapshot().registry()`.
    pub fn registry(&self) -> &LangRegistry {
        self.inner.registry()
    }

    /// Convenience pass-through to
    /// [`SyntaxSnapshot::cursor_in_string_scope`].
    pub fn cursor_in_string_scope(&self, cursor_byte: usize) -> bool {
        self.inner.cursor_in_string_scope(cursor_byte)
    }

    /// Convenience pass-through to
    /// [`SyntaxSnapshot::collect_symbols`].
    pub fn collect_symbols(&self) -> Vec<String> {
        self.inner.collect_symbols()
    }

    /// Convenience pass-through to
    /// [`SyntaxSnapshot::collect_symbol_locations`].
    pub fn collect_symbol_locations(&self) -> Vec<(String, u32, u32)> {
        self.inner.collect_symbol_locations()
    }

    /// Convenience pass-through to
    /// [`SyntaxSnapshot::scope_at_cursor`].
    pub fn scope_at_cursor(
        &self,
        line: u32,
        col_byte: u32,
        capture_suffix: &str,
    ) -> Option<lattice_protocol::position::Range> {
        self.inner.scope_at_cursor(line, col_byte, capture_suffix)
    }

    /// Convenience pass-through to
    /// [`SyntaxSnapshot::highlight_lines`]. Note: takes `&self`
    /// (the read API never needed `&mut`).
    pub fn highlight_lines(
        &self,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<Vec<StyledSpan>>, SyntaxError> {
        self.inner.highlight_lines(start_line, end_line)
    }

    /// Convenience pass-through to
    /// [`SyntaxSnapshot::highlight_lines_native`].
    pub fn highlight_lines_native(
        &self,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<Vec<StyledSpan>>, SyntaxError> {
        self.inner.highlight_lines_native(start_line, end_line)
    }

    /// Replace the cached source and drive a tree-sitter (re)parse.
    /// This is the only mutating call on `Syntax`; everything else
    /// is read-only against the snapshot. Production callers should
    /// route parse requests through [`crate::SyntaxHandle`] so the
    /// parse runs off the UI thread (paramount goal #1).
    pub fn parse(&mut self, source: &str) {
        self.parse_at(source, self.inner.text_version.wrapping_add(1));
    }

    /// `parse` variant that also stamps a caller-supplied
    /// `text_version` onto the resulting snapshot. The async
    /// handle uses this so consumers can deduplicate stale
    /// snapshots.
    ///
    /// Full reparse (passes `None` as the prior tree). The
    /// incremental sibling [`Self::parse_at_with_edits`] is the
    /// keystroke-path entry point; this method is the file-load
    /// / cold-start / fallback path.
    ///
    /// `Parser::parse` returning `None` means cancellation, which
    /// we don't trigger on this synchronous path. Keep the old
    /// tree in that unlikely case rather than dropping it -- the
    /// next parse round will retry.
    pub fn parse_at(&mut self, source: &str, text_version: u64) {
        let bytes = source.as_bytes();
        let new_tree = self
            .parser
            .parse(bytes, None)
            .or_else(|| self.inner.tree.take());
        self.inner.set_source_bytes(bytes);
        self.inner.tree = new_tree;
        self.inner.text_version = text_version;
        // H.2: a full reparse has no incremental old-vs-new diff, so the
        // whole file is considered dirty (`None` ⇒ consumers full-rebuild).
        self.inner.changed_lines = None;
        self.inner.reparsed_from_version = text_version;
    }

    /// Incremental reparse: apply `edits` to the cached tree (sync
    /// pre-step on the worker, ~500ns per edit), then parse with
    /// the edited tree as the seed (~50µs floor on medium files
    /// per §8.2). Falls back to [`Self::parse_at`] (full reparse)
    /// if the guards on [`Self::try_apply_intermediate`] fail.
    ///
    /// Slice C.2 split this into two halves so the worker can
    /// publish an intermediate snapshot between them -- byte
    /// ranges shifted to track the edit but tree shape pre-parse,
    /// so renderers see byte-aligned spans during the entire
    /// parse window. This convenience runs both halves back-to-
    /// back without an intermediate publish; the worker calls
    /// the two halves directly with the publish in between.
    pub fn parse_at_with_edits(
        &mut self,
        source: &str,
        text_version: u64,
        from_version: u64,
        edits: &[EditDelta],
    ) {
        match self.try_apply_intermediate(source, text_version, from_version, edits) {
            Ok(_) => self.reparse_with_cached_tree(from_version),
            Err(_) => self.parse_at(source, text_version),
        }
    }

    /// Slice C.2: try to apply `edits` to the cached tree and
    /// update `source` + `text_version`, WITHOUT running
    /// `Parser::parse`. Returns `Ok(())` if the resulting state
    /// is valid for the worker to publish as an intermediate
    /// snapshot (byte ranges shifted via `tree.edit` to track the
    /// edits; tree shape is pre-parse for the changed regions).
    /// Returns `Err(())` if any of:
    ///
    /// 1. **No cached tree** -- first reparse / worker recovered
    ///    from prior cancellation; nothing to seed with.
    /// 2. **`from_version` mismatch** -- worker's tree isn't at
    ///    the version the edits expect to start from. Indicates
    ///    a dropped reparse request, file load, or document
    ///    replace; the cached tree's byte ranges don't match the
    ///    edits.
    /// 3. **`edits` empty** -- nothing to apply.
    /// 4. **Byte-length mismatch** -- accumulated edit delta
    ///    doesn't match the source-length delta. Catches dropped
    ///    or truncated edit lists.
    ///
    /// On `Err`, the caller falls back to [`Self::parse_at`]
    /// (full reparse).
    ///
    /// Tree-sitter's failure mode for a malformed `InputEdit` is
    /// a silently wrong tree, not a panic. The layered guards
    /// here + the slice-B.4 parametrized parity matrix
    /// (incremental == full reparse across 27 edit shapes ×
    /// Rust / Python / JavaScript / Markdown) keep the silent-
    /// corruption surface contained.
    pub fn try_apply_intermediate(
        &mut self,
        source: &str,
        text_version: u64,
        from_version: u64,
        edits: &[EditDelta],
    ) -> Result<(), ()> {
        let cached_at_baseline = self.inner.tree.is_some()
            && self.inner.text_version == from_version
            && !edits.is_empty();
        if !cached_at_baseline {
            return Err(());
        }
        let prior_len = self.inner.source.len() as i64;
        let new_len = source.len() as i64;
        let edit_delta_sum: i64 = edits
            .iter()
            .map(|d| (d.new_end_byte as i64) - (d.old_end_byte as i64))
            .sum();
        if prior_len + edit_delta_sum != new_len {
            return Err(());
        }
        // All guards passed. Apply each edit to the cached tree
        // in order. tree-sitter mutates `Tree` in place via
        // `edit`; the mutation shifts every affected node's byte
        // range to track the edit. Source + text_version updated
        // so the snapshot's `source.len()` matches the edited
        // tree's byte ranges.
        let bytes = source.as_bytes();
        if let Some(tree) = self.inner.tree.as_mut() {
            for d in edits {
                tree.edit(&edit_delta_to_input_edit(*d));
            }
        }
        self.inner.set_source_bytes(bytes);
        self.inner.text_version = text_version;
        Ok(())
    }

    /// Slice C.2: re-parse using `self.inner.source` and the
    /// cached tree (assumed to already have `tree.edit` applied
    /// via [`Self::try_apply_intermediate`]) as seed. Updates
    /// `self.inner.tree` to the freshly-parsed shape.
    ///
    /// Pairs with `try_apply_intermediate`: the worker calls
    /// `try_apply_intermediate` (fast), publishes the intermediate
    /// snapshot, then calls this to run the actual parse (slow).
    pub fn reparse_with_cached_tree(&mut self, reparsed_from: u64) {
        let bytes = self.inner.source.clone();
        // Own the old tree so we can diff it against the new one after the
        // parse (tree-sitter `Tree` is cheap to clone — internally Arc'd).
        let old_tree = self.inner.tree.clone();
        let new_tree = self
            .parser
            .parse(&*bytes, old_tree.as_ref())
            .or_else(|| self.inner.tree.take());
        // H.2: `old.changed_ranges(new)` gives exactly the byte ranges whose
        // tree differs; the Range points carry rows, so map to inclusive
        // source-line ranges. Only valid when both trees exist (else the
        // whole file is dirty → `None`).
        self.inner.changed_lines = match (&old_tree, &new_tree) {
            (Some(old), Some(new)) => Some(
                old.changed_ranges(new)
                    .map(|r| (r.start_point.row as u32, r.end_point.row as u32))
                    .collect(),
            ),
            _ => None,
        };
        self.inner.reparsed_from_version = reparsed_from;
        self.inner.tree = new_tree;
    }
}

/// N.1.4b (2026-06-10): bridge the snapshot into the grammar
/// dispatcher's tree-sitter text-object resolution. The grammar
/// crate defines the `ScopeResolver` trait (cursor -> enclosing
/// scope row range) and stays tree-sitter-agnostic; this impl
/// forwards to the snapshot's existing
/// [`SyntaxSnapshot::scope_at_cursor`] (N.1.0). The host coerces
/// `Arc<SyntaxSnapshot>` to `Arc<dyn ScopeResolver + Send + Sync>`
/// and threads it through `Document::dispatch_with_scope_resolver`
/// so `daf` / `yic` etc. resolve against the live syntax tree off
/// the UI thread (paramount #1: the snapshot is immutable, the
/// query is bounded to the cursor's 1-byte window).
impl lattice_grammar::ScopeResolver for SyntaxSnapshot {
    fn scope_at(
        &self,
        line: u32,
        col_byte: u32,
        suffix: &str,
    ) -> Option<lattice_protocol::position::Range> {
        self.scope_at_cursor(line, col_byte, suffix)
    }

    // TSM.2: real tree walk -- forwards to the inherent
    // `SyntaxSnapshot::scope_toward` below, mirroring `scope_at`'s
    // forward to `scope_at_cursor`.
    fn scope_toward(
        &self,
        line: u32,
        col_byte: u32,
        suffix: &str,
        dir: lattice_grammar::NavDir,
        boundary: lattice_grammar::NavBoundary,
        count: u32,
    ) -> Option<lattice_protocol::Position> {
        self.scope_toward(line, col_byte, suffix, dir, boundary, count)
    }
}

impl SyntaxSnapshot {
    /// H.3d (2026-06-04): set `source` and recompute the memoized
    /// `line_starts` together so the two never drift. Every source
    /// mutation (full parse, incremental intermediate apply) routes
    /// through here; `highlight_lines_via_query` then reads
    /// `self.line_starts` instead of rescanning the whole source per
    /// call (the O(file) term the windowed-build bench exposed).
    fn set_source_bytes(&mut self, bytes: &[u8]) {
        self.source = Arc::from(bytes.to_vec());
        self.line_starts = Arc::from(compute_line_starts(&self.source));
    }

    /// The document language this snapshot was built for.
    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Latest parse result. `None` until the first parse has run.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Cached source bytes that produced [`Self::tree`].
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    /// Shared language registry. Query-driven consumers
    /// (`compute_syntax_folds`, future textobjects / indents)
    /// look up per-language compiled queries here.
    pub fn registry(&self) -> &LangRegistry {
        &self.registry
    }

    /// Document `text_version` this snapshot was built from.
    /// Used by the async handle to skip republishing identical
    /// state and by consumers that want to compare freshness
    /// against a `DocumentSnapshot::text_version`.
    pub fn text_version(&self) -> u64 {
        self.text_version
    }

    /// H.2 (2026-06-04): inclusive source-line ranges whose syntax tree
    /// changed between [`Self::reparsed_from_version`] and this snapshot,
    /// from `Tree::changed_ranges`. `None` ⇒ full parse / unknown (treat
    /// the whole file as dirty). The cells worker rebuilds only the
    /// intersecting rows on a reparse-completion republish, gated on its
    /// cached matrix's syntax version matching `reparsed_from_version`.
    pub fn changed_lines(&self) -> Option<&[(u32, u32)]> {
        self.changed_lines.as_deref()
    }

    /// The `text_version` this snapshot's tree was reparsed FROM — the
    /// baseline [`Self::changed_lines`] is the delta against. Meaningful
    /// only when `changed_lines()` is `Some`.
    pub fn reparsed_from_version(&self) -> u64 {
        self.reparsed_from_version
    }

    /// True when the byte position `cursor_byte` falls inside a
    /// string-literal node according to the cached tree.
    /// Walks ancestors from the deepest descendant covering the
    /// position and matches their `kind()` against a hardcoded
    /// set of string-shaped node names that span the v1
    /// language coverage (rust / python / javascript). Returns
    /// `false` when no parse is cached, when the position falls
    /// outside the source bytes, or when no ancestor matches.
    ///
    /// Used by the host's `gen:path` insert-completion source
    /// (Phase 4.2.g.6 (2/2)) -- the spec triggers path-completion
    /// inside string literals so file-path text is the only
    /// place where `/` opens the popup.
    pub fn cursor_in_string_scope(&self, cursor_byte: usize) -> bool {
        // Hardcoded set across the v1 grammars. Source for the
        // names: `tree-sitter-{rust,python,javascript}` node
        // catalogues. New languages append entries here when
        // they land. Substring matching ("kind contains
        // 'string'") would catch more variants but also misfire
        // on names like `string_concatenation` -- explicit list
        // stays safer.
        const STRING_NODE_KINDS: &[&str] = &[
            "string",
            "string_literal",
            "raw_string_literal",
            "byte_string_literal",
            "template_string",
            "string_fragment",
            "interpolated_string_literal",
        ];
        let Some(tree) = self.tree.as_ref() else {
            return false;
        };
        if cursor_byte > self.source.len() {
            return false;
        }
        let root = tree.root_node();
        let mut node = match root.descendant_for_byte_range(cursor_byte, cursor_byte) {
            Some(n) => n,
            None => return false,
        };
        loop {
            let kind = node.kind();
            if STRING_NODE_KINDS.contains(&kind) {
                return true;
            }
            match node.parent() {
                Some(p) => node = p,
                None => return false,
            }
        }
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
        let mut matches = cursor.matches(query, tree.root_node(), &self.source[..]);
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

    /// Like [`Self::collect_symbols`] but also reports each
    /// symbol's location -- `(name, line, byte_column)` with
    /// 0-based line and 0-based utf-8 byte column. Used by
    /// the picker's `:picker outline` source so accept can
    /// jump directly to the symbol definition. Dedup keys on
    /// `(name, line, col)` to keep redundant captures
    /// (function name appearing in both `@name` and `@definition`
    /// captures of the same query) from doubling up.
    pub fn collect_symbol_locations(&self) -> Vec<(String, u32, u32)> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let Some(query) = self.registry.symbols_query(self.lang.name()) else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), &self.source[..]);
        let mut seen: std::collections::HashSet<(String, u32, u32)> =
            std::collections::HashSet::new();
        let mut out: Vec<(String, u32, u32)> = Vec::new();
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
                let pos = n.start_position();
                let key = (text.to_string(), pos.row as u32, pos.column as u32);
                if seen.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
        // Stable sort by (line, col) so the popup reads top-
        // down through the file.
        out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
        out
    }

    /// Find the innermost tree-sitter scope containing the cursor whose
    /// `textobjects.scm` capture name *ends with* `capture_suffix`
    /// (e.g. `"function.outer"`, `"class.outer"`, `"block.outer"`) and
    /// return its inclusive 0-based `(start_line, end_line)` source rows.
    ///
    /// "Innermost" = smallest byte span among the matching captures that
    /// contain the cursor byte, so a cursor inside a closure nested in a
    /// function resolves the closure for `"function.outer"`, and a cursor
    /// on a statement resolves the tightest enclosing brace block for
    /// `"block.outer"`. Returns `None` when no parse is cached, the
    /// language ships no `textobjects.scm`, the cursor line is out of
    /// range, or no matching capture contains the cursor.
    ///
    /// `line` / `col_byte` are 0-based; `col_byte` is a utf-8 byte offset
    /// within the line (the snapshot's position convention). Powers
    /// narrow-mode's tree-sitter targets (`:narrow-function` /
    /// `:narrow-class` / `:narrow-block`, N.1.3); the plain `(u32, u32)`
    /// return keeps multibuffer / narrow types out of this crate
    /// (dependency direction: `lattice-multibuffer` -> `lattice-syntax`).
    pub fn scope_at_cursor(
        &self,
        line: u32,
        col_byte: u32,
        capture_suffix: &str,
    ) -> Option<lattice_protocol::position::Range> {
        let tree = self.tree.as_ref()?;
        let query = self.registry.textobjects_query(self.lang.name())?;
        // Absolute cursor byte = line-start + column. `line_starts` holds
        // `line_count + 1` entries (final = source length); an out-of-range
        // line yields `None`.
        let line_start = self.line_starts.get(line as usize).copied()?;
        let cursor_byte = (line_start + col_byte as usize).min(self.source.len());

        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        // Restrict to the 1-byte window at the cursor: a node `[start, end)`
        // overlaps `[cursor, cursor+1)` iff `start <= cursor < end` -- exactly
        // the half-open containment we want, so the explicit filter below is a
        // belt-and-suspenders guard, not a second condition. Bounds the match
        // set to enclosing scopes on large files.
        cursor.set_byte_range(cursor_byte..cursor_byte.saturating_add(1));
        let mut matches = cursor.matches(query, tree.root_node(), &self.source[..]);
        // Smallest-span containing capture so far: (span_len, start_pos,
        // end_pos). N.1.4c: track byte-precise positions (line + byte
        // column), not just rows, so intra-line objects (`aa`/`ia`) are
        // charwise-accurate.
        let mut best: Option<(
            usize,
            lattice_protocol::Position,
            lattice_protocol::Position,
        )> = None;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = names[cap.index as usize];
                if !name.ends_with(capture_suffix) {
                    continue;
                }
                let n = cap.node;
                let start = n.start_byte();
                let end = n.end_byte();
                // Half-open containment, matching tree-sitter node range
                // semantics: the cursor on the construct's last token (e.g.
                // `}` at byte `end - 1`) counts as inside; one past does not.
                if cursor_byte < start || cursor_byte >= end {
                    continue;
                }
                let span = end - start;
                // tree-sitter `Point.column` is a byte offset within the row,
                // which is exactly `Position.byte`. `end_position` is one past
                // the last byte (half-open), matching ProtoRange's exclusive end.
                let sp = n.start_position();
                let ep = n.end_position();
                let start_pos = lattice_protocol::Position::new(sp.row as u32, sp.column as u32);
                let end_pos = lattice_protocol::Position::new(ep.row as u32, ep.column as u32);
                // Strictly-smaller replaces, so the first capture seen at a
                // given span wins ties deterministically (query-pattern order).
                let replace = match best {
                    Some((best_span, _, _)) => span < best_span,
                    None => true,
                };
                if replace {
                    best = Some((span, start_pos, end_pos));
                }
            }
        }
        best.map(|(_, s, e)| lattice_protocol::position::Range::new(s, e))
    }

    /// The `count`-th node whose `textobjects.scm` capture name *ends
    /// with* `suffix`, scanning in `dir`, targeting the node's
    /// `boundary`. Backs the structural motions (`]f`/`[c`/…, TSM.4)
    /// via [`lattice_grammar::ScopeResolver::scope_toward`].
    ///
    /// Respects the enclosing-object rule (treesitter-motions.md
    /// §4.1): `(Forward, Start)` and `(Backward, End)` skip the object
    /// the cursor is currently inside (candidates strictly past the
    /// cursor byte); `(Backward, Start)` and `(Forward, End)` may land
    /// on the current object's own boundary (candidates at-or-past the
    /// cursor byte), so e.g. jumping backward to a function start from
    /// inside its body lands on that function's own `fn` keyword
    /// rather than skipping past it.
    ///
    /// `NavBoundary::End` returns `end_position` (one past the last
    /// byte), matching [`Self::scope_at_cursor`]'s half-open
    /// convention -- the operator's inclusive-end handling adds the
    /// final byte back for `d]F`-style deletes.
    ///
    /// Returns `None` gracefully (heuristic #5, no-op) when: there is
    /// no cached tree, the language ships no `textobjects.scm`, the
    /// cursor line is out of range, or there are fewer than `count`
    /// matching candidates in `dir` -- never panics.
    ///
    /// `line` / `col_byte` are 0-based, `col_byte` a utf-8 byte offset
    /// within the line (the snapshot's position convention, same as
    /// [`Self::scope_at_cursor`]).
    pub fn scope_toward(
        &self,
        line: u32,
        col_byte: u32,
        suffix: &str,
        dir: lattice_grammar::NavDir,
        boundary: lattice_grammar::NavBoundary,
        count: u32,
    ) -> Option<lattice_protocol::Position> {
        use lattice_grammar::{NavBoundary, NavDir};
        if count == 0 {
            return None;
        }
        let tree = self.tree.as_ref()?;
        let query = self.registry.textobjects_query(self.lang.name())?;
        let line_start = self.line_starts.get(line as usize).copied()?;
        let cursor_byte = (line_start + col_byte as usize).min(self.source.len());

        // Restrict the query to the half of the file we scan (perf:
        // bounds the match set on large files -- paramount #1).
        let mut cursor = QueryCursor::new();
        match dir {
            NavDir::Forward => cursor.set_byte_range(cursor_byte..self.source.len()),
            NavDir::Backward => cursor.set_byte_range(0..cursor_byte.saturating_add(1)),
        };

        let names = query.capture_names();
        // Collect candidate boundary bytes + their (row, col) positions.
        let mut cands: Vec<(usize, lattice_protocol::Position)> = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), &self.source[..]);
        while let Some(m) = matches.next() {
            for cap in m.captures {
                if !names[cap.index as usize].ends_with(suffix) {
                    continue;
                }
                let n = cap.node;
                let (b, pt) = match boundary {
                    NavBoundary::Start => (n.start_byte(), n.start_position()),
                    NavBoundary::End => (n.end_byte(), n.end_position()),
                };
                // Enclosing-object rule (treesitter-motions.md §4.1): all four
                // arms compare STRICTLY against the cursor byte. When the cursor
                // sits inside an object's body the enclosing object is still
                // reached (its start < cursor / end > cursor). The strictness
                // matters only when the cursor sits exactly ON a boundary byte
                // (e.g. right after `]f` landed on a function start): there the
                // current object is NOT re-selected, so the `]f`->`[f` round-trip
                // moves to the previous object instead of no-oping.
                let keep = match (dir, boundary) {
                    (NavDir::Forward, NavBoundary::Start) => b > cursor_byte,
                    (NavDir::Backward, NavBoundary::End) => b < cursor_byte,
                    (NavDir::Backward, NavBoundary::Start) => b < cursor_byte,
                    (NavDir::Forward, NavBoundary::End) => b > cursor_byte,
                };
                if keep {
                    cands.push((
                        b,
                        lattice_protocol::Position::new(pt.row as u32, pt.column as u32),
                    ));
                }
            }
        }
        // Sort in the direction of travel; dedup by byte (a node can be
        // captured by multiple patterns).
        cands.sort_by_key(|(b, _)| *b);
        cands.dedup_by_key(|(b, _)| *b);
        let ordered: Vec<_> = match dir {
            NavDir::Forward => cands,
            NavDir::Backward => cands.into_iter().rev().collect(),
        };
        ordered
            .get((count as usize).saturating_sub(1))
            .map(|(_, p)| *p)
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
        &self,
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
        &self,
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
        // H.3d: read the memoized line table (recomputed once per
        // source mutation) instead of rescanning the whole source on
        // every call — this is what keeps highlight O(window) on
        // large files.
        let line_starts: &[usize] = &self.line_starts;
        let total_lines = line_starts.len().saturating_sub(1).max(1) as u32;
        let end_line = end_line.min(total_lines + 1);
        if start_line >= end_line {
            return Ok(Vec::new());
        }
        let mut result: Vec<Vec<StyledSpan>> =
            (0..(end_line - start_line)).map(|_| Vec::new()).collect();
        let query = self
            .registry
            .highlights_query(self.lang.name())
            .ok_or_else(|| SyntaxError::UnregisteredLang(self.lang.name().to_string()))?;
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
        let mut matches = cursor.matches(query, tree.root_node(), &self.source[..]);
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
                collect_injections(inj_query, tree, &self.source[..], window_start, window_end);
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
                line_starts,
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
    fn cursor_in_string_scope_true_inside_rust_string_literal() {
        let source = "fn main() { let p = \"src/foo.rs\"; }\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(source);
        // Pick a byte that's inside the literal -- between the
        // opening quote and the closing one.
        let lit_start = source.find('"').unwrap();
        let lit_end = source.rfind('"').unwrap();
        let inside = lit_start + 4; // somewhere mid-string
        assert!(inside < lit_end);
        assert!(s.cursor_in_string_scope(inside));
    }

    #[test]
    fn cursor_in_string_scope_false_outside_string_literal() {
        let source = "fn main() { let p = \"src/foo.rs\"; }\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(source);
        // Position at `let` keyword -- no string ancestor.
        let outside = source.find("let").unwrap() + 1;
        assert!(!s.cursor_in_string_scope(outside));
    }

    #[test]
    fn cursor_in_string_scope_true_inside_python_string() {
        let source = "p = \"src/foo.py\"\n";
        let mut s = Syntax::for_language(Lang::Python).unwrap().unwrap();
        s.parse(source);
        let lit_start = source.find('"').unwrap();
        let inside = lit_start + 3;
        assert!(s.cursor_in_string_scope(inside));
    }

    #[test]
    fn cursor_in_string_scope_false_when_no_parse() {
        // Without a parse the helper returns false safely
        // rather than panicking on missing tree.
        let s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        assert!(!s.cursor_in_string_scope(0));
    }

    #[test]
    fn collect_symbols_empty_for_language_without_query() {
        // markdown ships no symbols.scm -> empty result even
        // after parse.
        let mut s = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s.parse("# heading\n\nbody\n");
        assert!(s.collect_symbols().is_empty());
    }

    // ---- N.1.0: scope_at_cursor (narrow-mode tree-sitter targets) ----

    /// N.1.4c: byte-precise expected-range helper. `scope_at_cursor`
    /// now returns a half-open `[start, end)` `ProtoRange` (line + byte
    /// column), not just rows.
    fn rng(sl: u32, sb: u32, el: u32, eb: u32) -> Option<lattice_protocol::position::Range> {
        Some(lattice_protocol::position::Range::new(
            lattice_protocol::Position::new(sl, sb),
            lattice_protocol::Position::new(el, eb),
        ))
    }

    #[test]
    fn scope_at_cursor_rust_fn_returns_correct_range() {
        // line 0: fn outer() {
        // line 1:     let x = 1;
        // line 2:     x
        // line 3: }
        let src = "fn outer() {\n    let x = 1;\n    x\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        // Cursor inside the body returns the whole function_item.
        assert_eq!(s.scope_at_cursor(1, 8, "function.outer"), rng(0, 0, 3, 1));
    }

    #[test]
    fn scope_at_cursor_selects_innermost_when_nested() {
        // A closure nested in a function: the closure is the innermost
        // @function.outer match, so its (smaller) range wins.
        // line 0: fn outer() {
        // line 1:     let f = || {
        // line 2:         1
        // line 3:     };
        // line 4: }
        let src = "fn outer() {\n    let f = || {\n        1\n    };\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(2, 8, "function.outer"), rng(1, 12, 3, 5));
    }

    #[test]
    fn scope_at_cursor_returns_none_outside_any_scope() {
        // line 0: use std::io;   <- not inside any function
        // line 1: fn main() {}
        let src = "use std::io;\nfn main() {}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(0, 4, "function.outer"), None);
    }

    #[test]
    fn scope_at_cursor_class_rust_struct() {
        // line 0: struct Point {
        // line 1:     x: i32,
        // line 2:     y: i32,
        // line 3: }
        let src = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(1, 4, "class.outer"), rng(0, 0, 3, 1));
        // The struct is not a function.
        assert_eq!(s.scope_at_cursor(1, 4, "function.outer"), None);
    }

    #[test]
    fn scope_at_cursor_block_targets_innermost_brace_scope() {
        // line 0: fn main() {
        // line 1:     if x > 0 {
        // line 2:         y = 1;
        // line 3:     }
        // line 4: }
        // Cursor on `y = 1;` -> innermost @block.outer is the if's
        // then-block (rows 1..3), not the whole function body (0..4).
        let src = "fn main() {\n    if x > 0 {\n        y = 1;\n    }\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(2, 8, "block.outer"), rng(1, 13, 3, 5));
    }

    #[test]
    fn scope_at_cursor_none_when_no_textobjects_query() {
        // markdown ships no textobjects.scm -> None even after parse.
        let mut s = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s.parse("# heading\n\nbody\n");
        assert_eq!(s.scope_at_cursor(0, 2, "function.outer"), None);
    }

    #[test]
    fn scope_at_cursor_none_when_no_parse() {
        // No parse -> tree is None -> None, no panic.
        let s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        assert_eq!(s.scope_at_cursor(0, 0, "function.outer"), None);
    }

    #[test]
    fn scope_at_cursor_python_function() {
        // line 0: def greet(name):
        // line 1:     msg = name
        // line 2:     return msg
        let src = "def greet(name):\n    msg = name\n    return msg\n";
        let mut s = Syntax::for_language(Lang::Python).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(1, 4, "function.outer"), rng(0, 0, 2, 14));
    }

    // ---- N.1.4c: inner bodies, parameters, loops (byte-precise) ----

    #[test]
    fn scope_at_cursor_function_inner_is_body_block() {
        // line 0: fn outer() {   <- `{` at col 11
        // line 3: }              <- `}` at col 0, exclusive end col 1
        let src = "fn outer() {\n    let x = 1;\n    x\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        // `if` (inner function) = the body block (braces included, v1).
        assert_eq!(s.scope_at_cursor(1, 8, "function.inner"), rng(0, 11, 3, 1));
        // `af` (outer) still spans the whole function_item.
        assert_eq!(s.scope_at_cursor(1, 8, "function.outer"), rng(0, 0, 3, 1));
    }

    #[test]
    fn scope_at_cursor_class_inner_is_field_list() {
        // line 0: struct Point {  <- `{` at col 13
        let src = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(1, 4, "class.inner"), rng(0, 13, 3, 1));
    }

    #[test]
    fn scope_at_cursor_parameter_byte_precise() {
        // line 0: fn add(x: i32, y: i32) -> i32 { x + y }
        //                ^col 7        ^col 15
        let src = "fn add(x: i32, y: i32) -> i32 { x + y }\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        // `aa` on the first parameter -> exactly `x: i32` (cols 7..13),
        // NOT the whole signature line -- this is the byte-precision win.
        assert_eq!(s.scope_at_cursor(0, 7, "parameter.outer"), rng(0, 7, 0, 13));
        assert_eq!(s.scope_at_cursor(0, 7, "parameter.inner"), rng(0, 7, 0, 13));
        // Cursor on the second parameter resolves the second span.
        assert_eq!(
            s.scope_at_cursor(0, 15, "parameter.outer"),
            rng(0, 15, 0, 21)
        );
    }

    #[test]
    fn scope_at_cursor_loop_outer_and_inner() {
        // line 0: fn main() {
        // line 1:     for i in 0..10 {   <- `for` col 4, body `{` col 19
        // line 2:         x += i;
        // line 3:     }                  <- exclusive end col 5
        // line 4: }
        let src = "fn main() {\n    for i in 0..10 {\n        x += i;\n    }\n}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        assert_eq!(s.scope_at_cursor(2, 8, "loop.outer"), rng(1, 4, 3, 5));
        assert_eq!(s.scope_at_cursor(2, 8, "loop.inner"), rng(1, 19, 3, 5));
    }

    #[test]
    fn scope_at_cursor_python_inner_and_parameter() {
        // line 0: def greet(name):   <- `name` at cols 10..14
        // line 1:     msg = name
        // line 2:     return msg
        let src = "def greet(name):\n    msg = name\n    return msg\n";
        let mut s = Syntax::for_language(Lang::Python).unwrap().unwrap();
        s.parse(src);
        assert_eq!(
            s.scope_at_cursor(0, 10, "parameter.outer"),
            rng(0, 10, 0, 14)
        );
        // Inner function = the suite body (delimiter-free in Python).
        assert_eq!(s.scope_at_cursor(1, 4, "function.inner"), rng(1, 4, 2, 14));
    }

    #[test]
    fn scope_at_cursor_javascript_parameter_byte_precise() {
        // line 0: function add(x, y) { return x + y; }
        //                      ^col 13  ^col 16
        let src = "function add(x, y) { return x + y; }\n";
        let mut s = Syntax::for_language(Lang::JavaScript).unwrap().unwrap();
        s.parse(src);
        assert_eq!(
            s.scope_at_cursor(0, 13, "parameter.outer"),
            rng(0, 13, 0, 14)
        );
        assert_eq!(
            s.scope_at_cursor(0, 16, "parameter.outer"),
            rng(0, 16, 0, 17)
        );
    }

    #[test]
    fn daf_deletes_a_whole_function_end_to_end() {
        // N.1.4c end-to-end: operator (`d`) + structural text object
        // (`af`) + the byte-precise scope resolver -> a real edit. Proves
        // the whole chain below the keymap: `register_syntax_text_objects`
        // mints `around_function`; the dispatcher resolves
        // `Target::TextObject(around_function)` by calling the object's
        // apply with the `SyntaxSnapshot` as `scope_resolver`; the
        // resolved byte span feeds the delete operator.
        use lattice_grammar::{
            Args, CancellationToken, CommandInvocation, Target, TextObjectEnv, execute_with_env,
        };

        let src = "fn keep() {}\nfn drop_me() {\n    let x = 1;\n}\nfn also_keep() {}\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);

        let mut registry = lattice_grammar::CommandRegistry::new();
        let builtins = lattice_grammar::builtins::populate(&mut registry);
        let ids = crate::text_objects::register_syntax_text_objects(&mut registry);

        let mut doc = lattice_core::Document::from_text(src);
        // Cursor inside `drop_me`'s body (line 2).
        let cursor = lattice_protocol::Position::new(2, 8);
        let inv = CommandInvocation::of(builtins.delete.0)
            .with_target(Target::TextObject(ids.around_function, Args::None));
        execute_with_env(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
            TextObjectEnv {
                // `&s.inner` is the SyntaxSnapshot; coerces to &dyn ScopeResolver.
                scope_resolver: Some(&s.inner),
                comment_syntax: None,
                syntax: None,
            },
        )
        .expect("daf dispatch ok");

        let after = doc.text();
        assert!(
            !after.contains("drop_me"),
            "`daf` should delete the whole function, got: {after:?}"
        );
        assert!(
            after.contains("keep") && after.contains("also_keep"),
            "neighbouring functions stay intact: {after:?}"
        );
    }

    // ---- TSM.2: scope_toward (structural motions tree walk) ----

    use lattice_grammar::{NavBoundary, NavDir};

    fn snapshot_rust(src: &str) -> SyntaxSnapshot {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        s.snapshot_owned()
    }

    fn snapshot_python(src: &str) -> SyntaxSnapshot {
        let mut s = Syntax::for_language(Lang::Python).unwrap().unwrap();
        s.parse(src);
        s.snapshot_owned()
    }

    /// A snapshot with source set but no parse ever run, so `tree` stays
    /// `None` -- mirrors a document whose first parse hasn't landed yet.
    fn snapshot_plain(src: &str) -> SyntaxSnapshot {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.inner.set_source_bytes(src.as_bytes());
        s.snapshot_owned()
    }

    // Source: 3 top-level fns at rows 0, 2, 4.
    //   row 0: fn a() {}
    //   row 2: fn b() {}
    //   row 4: fn c() {}
    fn three_fns() -> SyntaxSnapshot {
        snapshot_rust("fn a() {}\n\nfn b() {}\n\nfn c() {}\n")
    }

    #[test]
    fn scope_toward_forward_start_skips_enclosing() {
        let s = three_fns();
        // Cursor inside fn a (row 0) -> next function START is fn b (row 2).
        let p = s.scope_toward(
            0,
            3,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            1,
        );
        assert_eq!(p, Some(lattice_protocol::Position::new(2, 0)));
    }

    #[test]
    fn scope_toward_forward_start_count_two() {
        let s = three_fns();
        // From row 0, 2nd next function start is fn c (row 4).
        let p = s.scope_toward(
            0,
            3,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            2,
        );
        assert_eq!(p, Some(lattice_protocol::Position::new(4, 0)));
    }

    #[test]
    fn scope_toward_backward_start_lands_on_current() {
        let s = three_fns();
        // Cursor inside fn b past its start (row 2, col 5) -> prev START is fn b's
        // OWN start (row 2, col 0), per the enclosing rule.
        let p = s.scope_toward(
            2,
            5,
            "function.outer",
            NavDir::Backward,
            NavBoundary::Start,
            1,
        );
        assert_eq!(p, Some(lattice_protocol::Position::new(2, 0)));
    }

    #[test]
    fn scope_toward_backward_start_on_boundary_moves_to_previous() {
        // Regression: the `]f` -> `[f` round-trip. After `]f` the cursor sits
        // EXACTLY on fn b's start (row 2, col 0). `[f` from there must move to
        // the PREVIOUS function (fn a, row 0), not no-op on fn b's own start.
        // A non-strict `<=` comparison would re-select fn b here.
        let s = three_fns();
        let p = s.scope_toward(
            2,
            0,
            "function.outer",
            NavDir::Backward,
            NavBoundary::Start,
            1,
        );
        assert_eq!(p, Some(lattice_protocol::Position::new(0, 0)));
    }

    #[test]
    fn scope_toward_forward_end_on_boundary_moves_to_next() {
        // Symmetric regression for `]F`. Cursor EXACTLY on fn a's end
        // (row 0, col 9 -- one past `}`). `]F` must move to the NEXT function's
        // end (fn b, row 2, col 9), not no-op on fn a's own end.
        let s = three_fns();
        let p = s.scope_toward(0, 9, "function.outer", NavDir::Forward, NavBoundary::End, 1);
        assert_eq!(p, Some(lattice_protocol::Position::new(2, 9)));
    }

    #[test]
    fn scope_toward_forward_end_lands_on_current_end() {
        let s = three_fns();
        // Cursor inside fn b (row 2, col 5) -> next END is fn b's own closing
        // brace. "fn b() {}" -- end_position is row 2, col 9 (one past `}`).
        let p = s.scope_toward(2, 5, "function.outer", NavDir::Forward, NavBoundary::End, 1);
        assert_eq!(p, Some(lattice_protocol::Position::new(2, 9)));
    }

    #[test]
    fn scope_toward_stops_at_boundary() {
        let s = three_fns();
        // From inside the LAST fn, forward-start has no next -> None (no wrap).
        let p = s.scope_toward(
            4,
            3,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            1,
        );
        assert_eq!(p, None);
    }

    #[test]
    fn scope_toward_none_without_tree() {
        let s = snapshot_plain("plain text no tree\n");
        let p = s.scope_toward(
            0,
            0,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            1,
        );
        assert_eq!(p, None);
    }

    #[test]
    fn scope_toward_python_forward_start_skips_enclosing() {
        // row 0: def a(): pass
        // row 1: def b(): pass
        // row 2: def c(): pass
        let s = snapshot_python("def a(): pass\ndef b(): pass\ndef c(): pass\n");
        // Cursor inside def a (row 0) -> next function START is def b (row 1).
        let p = s.scope_toward(
            0,
            3,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            1,
        );
        assert_eq!(p, Some(lattice_protocol::Position::new(1, 0)));
        // Count 2 -> def c (row 2).
        let p2 = s.scope_toward(
            0,
            3,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            2,
        );
        assert_eq!(p2, Some(lattice_protocol::Position::new(2, 0)));
    }

    #[test]
    fn scope_toward_backward_end_skips_enclosing() {
        let s = three_fns();
        // Cursor inside fn b (row 2, col 5) -> prev END is fn a's END, NOT fn b's
        // own end. The enclosing rule for (Backward, End) keeps candidates
        // strictly before the cursor (`b < cursor_byte`), so fn b's own closing
        // brace (past the cursor) is skipped. "fn a() {}" -> end_position row 0,
        // col 9 (one past the `}`).
        let p = s.scope_toward(
            2,
            5,
            "function.outer",
            NavDir::Backward,
            NavBoundary::End,
            1,
        );
        assert_eq!(p, Some(lattice_protocol::Position::new(0, 9)));
    }

    #[test]
    fn scope_toward_count_zero_is_none() {
        let s = three_fns();
        // count == 0 has no "0th" candidate -> None (guard, never panics).
        let p = s.scope_toward(
            0,
            3,
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
            0,
        );
        assert_eq!(p, None);
    }

    #[test]
    fn scope_toward_empty_candidate_set_is_none() {
        // Tree present + textobjects query present, but the source has no loops,
        // so `loop.outer` captures nothing -> empty candidate set -> None.
        let s = three_fns();
        let p = s.scope_toward(0, 3, "loop.outer", NavDir::Forward, NavBoundary::Start, 1);
        assert_eq!(p, None);
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
        assert_has_style(
            Lang::Rust,
            "fn main() {\n    let x = 1;\n}\n",
            Style::Keyword,
        );
        assert_has_style(
            Lang::Rust,
            "fn main() {\n    let x = 1;\n}\n",
            Style::Function,
        );
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
        let src = "# H1\n\n## H2\n\n### H3\n\nbody paragraph\n";
        assert_has_style(Lang::Markdown, src, Style::Heading1);
        // Lattice's custom markdown highlights query distinguishes heading
        // LEVELS (the bundled tree-sitter-md query is level-less). `##` →
        // Heading2, `###` → Heading3, so the theme can size + colour each
        // level differently (Thread F + per-level heading colours).
        assert_has_style(Lang::Markdown, src, Style::Heading2);
        assert_has_style(Lang::Markdown, src, Style::Heading3);
    }

    /// Reproduction (2026-06-03): markdown highlighting must survive
    /// an incremental edit. The `parity_*` tests only compare TREE
    /// SHAPE (`to_sexp`); this compares the actual HIGHLIGHT SPANS
    /// produced incremental-after-edit vs a full reparse of the final
    /// text — the untested gap behind "markdown highlighting never
    /// comes back after an edit". If this fails, the reparse/highlight
    /// path drops markdown styling on a keystroke.
    #[test]
    fn markdown_highlight_survives_incremental_edit() {
        let src_a = "# Heading\n\nHello world\n";
        // Type a character inside the paragraph (byte 16 sits in
        // "Hello world").
        let (src_b, delta) = delta_for_edit(src_a, 16, 16, "X");
        let lc = src_b.split('\n').count() as u32;

        let mut s_inc = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s_inc.parse_at(src_a, 1);
        s_inc.parse_at_with_edits(&src_b, 2, 1, &[delta]);
        let inc = s_inc.highlight_lines(0, lc).unwrap();

        let mut s_full = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s_full.parse_at(&src_b, 1);
        let full = s_full.highlight_lines(0, lc).unwrap();

        assert_eq!(
            inc, full,
            "markdown highlight spans diverge incremental vs full after edit"
        );
        assert!(
            inc[0].iter().any(|sp| sp.style == Style::Heading1),
            "heading highlight lost after incremental edit: {:?}",
            inc[0]
        );
    }

    /// H.2 (2026-06-04): an incremental reparse must publish
    /// `changed_lines` covering the edited line (so the cells worker can
    /// rebuild only dirty rows on reparse-completion), with
    /// `reparsed_from_version` set to the baseline it diffed against.
    #[test]
    fn changed_lines_covers_the_edited_line() {
        let src_a = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        // Insert 'X' after "fn b" on line 1 → "fn bX() {}".
        let (src_b, delta) = delta_for_edit(src_a, 13, 13, "X");
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse_at(src_a, 1);
        s.parse_at_with_edits(&src_b, 2, 1, &[delta]);

        assert_eq!(
            s.snapshot().reparsed_from_version(),
            1,
            "changed_lines baseline must be the from_version"
        );
        let changed = s
            .snapshot()
            .changed_lines()
            .expect("an incremental reparse must yield Some(changed_lines)");
        assert!(
            changed.iter().any(|&(lo, hi)| lo <= 1 && 1 <= hi),
            "changed_lines must cover the edited line 1, got {changed:?}"
        );
    }

    /// Diagnostic (2026-06-04): does an UNCHANGED line carrying inline
    /// injection content (a `code span` + a [link]) keep IDENTICAL
    /// highlight spans across a reparse triggered by editing a
    /// DIFFERENT line? If not, markdown's inline injection is
    /// non-deterministic across reparses — which is why those lines
    /// flicker on every keystroke (B.1 full-rebuilds on reparse and the
    /// inline colours flip).
    #[test]
    fn markdown_inline_spans_stable_across_unrelated_edit() {
        let src_a = "# H\n\nUse `code` and [link](http://x)\n\ntail\n";
        // Edit "tail" (line 4), well away from the inline-content line 2.
        let (src_b, delta) = delta_for_edit(src_a, 42, 42, "X");

        let mut s1 = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s1.parse_at(src_a, 1);
        let line2_v1 = s1.highlight_lines(2, 3).unwrap().remove(0);

        let mut s2 = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        s2.parse_at(src_a, 1);
        s2.parse_at_with_edits(&src_b, 2, 1, &[delta]);
        let line2_v2 = s2.highlight_lines(2, 3).unwrap().remove(0);

        assert_eq!(
            line2_v1, line2_v2,
            "inline-content line 2 must keep identical spans across an unrelated edit \
             (v1 = full parse, v2 = incremental reparse after editing line 4)"
        );
    }

    // ---- Slice B.2: incremental reparse parity tests -----------
    //
    // Tree-sitter's failure mode for a malformed `InputEdit` is a
    // silently wrong tree (no error, just stale node ranges).
    // These tests pin that incremental reparse produces the SAME
    // tree shape as full reparse on the same final source --
    // catching any future drift in `parse_at_with_edits` or the
    // `EditDelta -> InputEdit` conversion.

    /// Helper: parse `source_a` then drive an incremental reparse
    /// to `source_b` using the supplied edits. Compare to a fresh
    /// full reparse on `source_b` directly. Returns the two trees'
    /// s-expressions for assertion.
    fn incremental_vs_full_reparse(
        lang: Lang,
        source_a: &str,
        source_b: &str,
        edits: &[EditDelta],
    ) -> (String, String) {
        let mut s_inc = Syntax::for_language(lang).unwrap().unwrap();
        s_inc.parse_at(source_a, 1);
        s_inc.parse_at_with_edits(source_b, 2, 1, edits);
        let inc = s_inc.tree().unwrap().root_node().to_sexp();

        let mut s_full = Syntax::for_language(lang).unwrap().unwrap();
        s_full.parse_at(source_b, 1);
        let full = s_full.tree().unwrap().root_node().to_sexp();

        (inc, full)
    }

    #[test]
    fn incremental_reparse_single_insert_matches_full_reparse() {
        // Insert "x" at byte 3 of "fn main() {}". Single-edit
        // case -- the simplest incremental path.
        let edits = [EditDelta {
            start_byte: 3,
            old_end_byte: 3,
            new_end_byte: 4,
            start_position: lattice_protocol::Position::new(0, 3),
            old_end_position: lattice_protocol::Position::new(0, 3),
            new_end_position: lattice_protocol::Position::new(0, 4),
        }];
        let (inc, full) =
            incremental_vs_full_reparse(Lang::Rust, "fn main() {}", "fn xmain() {}", &edits);
        assert_eq!(inc, full, "incremental tree must match full reparse");
    }

    #[test]
    fn incremental_reparse_single_delete_matches_full_reparse() {
        // Delete byte 3 of "fn xmain() {}".
        let edits = [EditDelta {
            start_byte: 3,
            old_end_byte: 4,
            new_end_byte: 3,
            start_position: lattice_protocol::Position::new(0, 3),
            old_end_position: lattice_protocol::Position::new(0, 4),
            new_end_position: lattice_protocol::Position::new(0, 3),
        }];
        let (inc, full) =
            incremental_vs_full_reparse(Lang::Rust, "fn xmain() {}", "fn main() {}", &edits);
        assert_eq!(inc, full);
    }

    #[test]
    fn incremental_reparse_multiline_replace_matches_full_reparse() {
        // Replace `let x = 1;` (line 1) with `let x = 42;`.
        // Source A: "fn main() {\n    let x = 1;\n}"
        // Source B: "fn main() {\n    let x = 42;\n}"
        // Replacement byte range starts at line 1 col 12, ends at
        // line 1 col 13. Insert "42" (2 bytes) for "1" (1 byte).
        let source_a = "fn main() {\n    let x = 1;\n}";
        let source_b = "fn main() {\n    let x = 42;\n}";
        // "fn main() {\n" is 12 bytes. "    let x = " is 12 more
        // = byte 24. "1" is at byte 24. End at byte 25.
        let edits = [EditDelta {
            start_byte: 24,
            old_end_byte: 25,
            new_end_byte: 26,
            start_position: lattice_protocol::Position::new(1, 12),
            old_end_position: lattice_protocol::Position::new(1, 13),
            new_end_position: lattice_protocol::Position::new(1, 14),
        }];
        let (inc, full) = incremental_vs_full_reparse(Lang::Rust, source_a, source_b, &edits);
        assert_eq!(inc, full);
    }

    #[test]
    fn incremental_reparse_multi_edit_batch_matches_full_reparse() {
        // Two edits applied in sequence: insert "y" at byte 3,
        // then "z" at (post-first-edit) byte 5. The cumulative
        // shape is "fn yxmzain() {}" (Position fields shift after
        // first edit).
        // Source A: "fn xmain() {}"
        // After edit 1: "fn yxmain() {}"
        // After edit 2: "fn yxmzain() {}"
        let source_a = "fn xmain() {}";
        let source_b = "fn yxmzain() {}";
        let edits = [
            EditDelta {
                start_byte: 3,
                old_end_byte: 3,
                new_end_byte: 4,
                start_position: lattice_protocol::Position::new(0, 3),
                old_end_position: lattice_protocol::Position::new(0, 3),
                new_end_position: lattice_protocol::Position::new(0, 4),
            },
            EditDelta {
                start_byte: 6,
                old_end_byte: 6,
                new_end_byte: 7,
                start_position: lattice_protocol::Position::new(0, 6),
                old_end_position: lattice_protocol::Position::new(0, 6),
                new_end_position: lattice_protocol::Position::new(0, 7),
            },
        ];
        let (inc, full) = incremental_vs_full_reparse(Lang::Rust, source_a, source_b, &edits);
        assert_eq!(inc, full);
    }

    #[test]
    fn parse_at_with_edits_falls_back_to_full_reparse_when_no_cached_tree() {
        // Fresh Syntax, no cached tree. parse_at_with_edits with
        // edits should fall back to full reparse rather than
        // panicking or producing a wrong tree.
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        let edits = [EditDelta {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 5,
            start_position: lattice_protocol::Position::new(0, 0),
            old_end_position: lattice_protocol::Position::new(0, 0),
            new_end_position: lattice_protocol::Position::new(0, 5),
        }];
        s.parse_at_with_edits("hello", 1, 0, &edits);
        let tree = s.tree().expect("tree present after fallback");
        // Tree should match a direct full-reparse on the same
        // source -- proves the fallback path produced a correct
        // tree, not a stale one.
        let mut s_full = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s_full.parse_at("hello", 1);
        assert_eq!(
            tree.root_node().to_sexp(),
            s_full.tree().unwrap().root_node().to_sexp(),
        );
    }

    #[test]
    fn parse_at_with_edits_falls_back_when_from_version_mismatches() {
        // Cached tree at version 5; request claims from_version=3.
        // The deltas from v3->v6 don't apply to a tree at v5, so
        // the worker MUST fall back to full reparse rather than
        // silently corrupt the cached tree.
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse_at("fn a() {}", 5);
        // Construct a delta that would be wrong for the cached
        // tree's actual state -- but since from_version mismatch
        // triggers fallback, the wrong delta is never applied.
        let edits = [EditDelta {
            start_byte: 100,
            old_end_byte: 100,
            new_end_byte: 105,
            start_position: lattice_protocol::Position::new(99, 0),
            old_end_position: lattice_protocol::Position::new(99, 0),
            new_end_position: lattice_protocol::Position::new(99, 5),
        }];
        // from_version=3 != cached version 5 -> full reparse.
        s.parse_at_with_edits("fn b() {}", 6, 3, &edits);
        // Result tree must match a full reparse on "fn b() {}",
        // not contain stale "fn a() {}" structure or weird
        // out-of-range nodes from the bogus delta.
        let mut s_full = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s_full.parse_at("fn b() {}", 6);
        assert_eq!(
            s.tree().unwrap().root_node().to_sexp(),
            s_full.tree().unwrap().root_node().to_sexp(),
        );
    }

    #[test]
    fn parse_at_with_edits_falls_back_on_byte_length_mismatch() {
        // Edit claims to net +0 bytes (insert "ab", delete "cd")
        // but the actual source delta is +5 bytes. The byte-length
        // guard catches the dropped/missing edit and routes to
        // full reparse.
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        let source_a = "fn a() {}";
        s.parse_at(source_a, 1);
        let edits = [EditDelta {
            start_byte: 3,
            old_end_byte: 4,
            new_end_byte: 4,
            start_position: lattice_protocol::Position::new(0, 3),
            old_end_position: lattice_protocol::Position::new(0, 4),
            new_end_position: lattice_protocol::Position::new(0, 4),
        }];
        // Source B is much longer than the edit accounts for ->
        // length mismatch -> full reparse.
        let source_b = "fn aaaaaaa() {}";
        s.parse_at_with_edits(source_b, 2, 1, &edits);
        // Verify result matches full reparse.
        let mut s_full = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s_full.parse_at(source_b, 2);
        assert_eq!(
            s.tree().unwrap().root_node().to_sexp(),
            s_full.tree().unwrap().root_node().to_sexp(),
        );
    }

    #[test]
    fn edit_delta_to_input_edit_maps_fields_one_to_one() {
        let d = EditDelta {
            start_byte: 10,
            old_end_byte: 15,
            new_end_byte: 20,
            start_position: lattice_protocol::Position::new(2, 3),
            old_end_position: lattice_protocol::Position::new(2, 8),
            new_end_position: lattice_protocol::Position::new(2, 13),
        };
        let inp = edit_delta_to_input_edit(d);
        assert_eq!(inp.start_byte, 10);
        assert_eq!(inp.old_end_byte, 15);
        assert_eq!(inp.new_end_byte, 20);
        assert_eq!(inp.start_position.row, 2);
        assert_eq!(inp.start_position.column, 3);
        assert_eq!(inp.old_end_position.row, 2);
        assert_eq!(inp.old_end_position.column, 8);
        assert_eq!(inp.new_end_position.row, 2);
        assert_eq!(inp.new_end_position.column, 13);
    }

    // ---- Slice B.4: parametrized parity matrix ------------------
    //
    // Tree-sitter's failure mode for a malformed `InputEdit` is a
    // silently wrong tree -- the parser produces a syntactically
    // valid tree whose node ranges are off, with no error. The
    // representative-shape parity tests above (B.2) catch the
    // common cases; this matrix broadens to the long tail:
    //
    // - **Edge positions**: edits at byte 0, edits at end-of-buffer,
    //   edits at line boundaries.
    // - **Multi-line shape changes**: insert / delete newlines
    //   so the line count itself shifts.
    // - **Sequential batches**: simulate keystroke bursts (each
    //   delta operating on the post-prior-edit state) and indent-
    //   style multi-line edits.
    // - **Per-language**: same shape in Rust / Python /
    //   JavaScript so language-specific drift in the
    //   `EditDelta -> InputEdit` mapping or `tree.edit()` semantics
    //   surfaces.
    //
    // Each test asserts the incremental parse's tree s-expression
    // equals the full-reparse s-expression on the same final
    // source. Failures pinpoint the (language, edit shape) where
    // the deltas drift -- a precise regression net.

    /// Build the post-edit source + an `EditDelta` for an edit
    /// described by `(start_byte, old_end_byte, new_text)`. Self-
    /// contained -- doesn't depend on `lattice-core::Buffer` so
    /// `lattice-syntax` tests stay free of that dependency.
    fn delta_for_edit(
        source_a: &str,
        start_byte: usize,
        old_end_byte: usize,
        new_text: &str,
    ) -> (String, EditDelta) {
        let pos_at = |byte: usize, src: &str| -> lattice_protocol::Position {
            let prefix = &src[..byte];
            let line = prefix.matches('\n').count() as u32;
            let col = (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
            lattice_protocol::Position::new(line, col)
        };
        let mut source_b =
            String::with_capacity(source_a.len() - (old_end_byte - start_byte) + new_text.len());
        source_b.push_str(&source_a[..start_byte]);
        source_b.push_str(new_text);
        source_b.push_str(&source_a[old_end_byte..]);
        let new_end_byte = start_byte + new_text.len();
        let delta = EditDelta {
            start_byte: start_byte as u32,
            old_end_byte: old_end_byte as u32,
            new_end_byte: new_end_byte as u32,
            start_position: pos_at(start_byte, source_a),
            old_end_position: pos_at(old_end_byte, source_a),
            new_end_position: pos_at(new_end_byte, &source_b),
        };
        (source_b, delta)
    }

    /// Apply a sequence of edits to `source_a`, returning the
    /// final source + the per-edit deltas (in apply order). Each
    /// edit's positions are derived against the buffer state
    /// AFTER the prior edit applied -- mirroring how App's
    /// chokepoint produces deltas via successive
    /// `Buffer::apply_edit` calls.
    fn apply_sequential_edits(
        source_a: &str,
        edits: &[(usize, usize, &str)],
    ) -> (String, Vec<EditDelta>) {
        let mut current = source_a.to_string();
        let mut deltas = Vec::with_capacity(edits.len());
        for (start, old_end, new_text) in edits {
            let (next, delta) = delta_for_edit(&current, *start, *old_end, new_text);
            current = next;
            deltas.push(delta);
        }
        (current, deltas)
    }

    /// Run incremental + full reparse on `source_a` -> `source_b`
    /// via `edits`, asserting tree-shape equality. Failure
    /// message names the language + the source pair.
    fn assert_parity(lang: Lang, source_a: &str, edits: &[EditDelta], source_b: &str, case: &str) {
        let mut s_inc = Syntax::for_language(lang).unwrap().unwrap();
        s_inc.parse_at(source_a, 1);
        s_inc.parse_at_with_edits(source_b, 2, 1, edits);
        let inc = s_inc.tree().unwrap().root_node().to_sexp();

        let mut s_full = Syntax::for_language(lang).unwrap().unwrap();
        s_full.parse_at(source_b, 1);
        let full = s_full.tree().unwrap().root_node().to_sexp();

        assert_eq!(
            inc, full,
            "incremental != full reparse for {lang:?} / {case}\n  source_a: {source_a:?}\n  source_b: {source_b:?}"
        );
    }

    /// Single-edit parity helper: derive delta from
    /// `(start_byte, old_end_byte, new_text)`, run parity check.
    fn assert_single_edit_parity(
        lang: Lang,
        source_a: &str,
        start_byte: usize,
        old_end_byte: usize,
        new_text: &str,
        case: &str,
    ) {
        let (source_b, delta) = delta_for_edit(source_a, start_byte, old_end_byte, new_text);
        assert_parity(lang, source_a, &[delta], &source_b, case);
    }

    // ==== Edge-position single edits ============================

    #[test]
    fn parity_insert_at_byte_zero_rust() {
        assert_single_edit_parity(
            Lang::Rust,
            "fn main() {}",
            0,
            0,
            "// header\n",
            "insert at byte 0",
        );
    }

    #[test]
    fn parity_insert_at_end_of_buffer_rust() {
        let src = "fn main() {}";
        assert_single_edit_parity(
            Lang::Rust,
            src,
            src.len(),
            src.len(),
            "\nfn b() {}",
            "insert at end",
        );
    }

    #[test]
    fn parity_delete_first_char_rust() {
        assert_single_edit_parity(Lang::Rust, "Xfn main() {}", 0, 1, "", "delete first byte");
    }

    #[test]
    fn parity_delete_last_char_rust() {
        let src = "fn main() {};";
        assert_single_edit_parity(
            Lang::Rust,
            src,
            src.len() - 1,
            src.len(),
            "",
            "delete last byte",
        );
    }

    #[test]
    fn parity_replace_whole_buffer_rust() {
        let src = "fn a() {}";
        assert_single_edit_parity(
            Lang::Rust,
            src,
            0,
            src.len(),
            "fn b(x: i32) -> i32 { x + 1 }",
            "replace whole buffer",
        );
    }

    #[test]
    fn parity_insert_at_line_boundary_rust() {
        // After the newline ending line 0; before any content
        // on line 1.
        let src = "fn a() {}\n";
        assert_single_edit_parity(
            Lang::Rust,
            src,
            10,
            10,
            "fn b() {}\n",
            "insert at line boundary",
        );
    }

    // ==== Multi-line shape changes ==============================

    #[test]
    fn parity_insert_newline_splitting_a_line_rust() {
        // Insert "\n    " mid-statement, breaking it across lines.
        let src = "fn a() { let x = 1; }";
        assert_single_edit_parity(Lang::Rust, src, 9, 9, "\n    ", "insert newline mid-line");
    }

    #[test]
    fn parity_delete_newline_joining_lines_rust() {
        // Source has two lines; delete the connecting newline.
        let src = "fn a() {\n    1;\n}";
        assert_single_edit_parity(Lang::Rust, src, 8, 9, "", "delete newline");
    }

    #[test]
    fn parity_replace_single_with_multi_line_rust() {
        let src = "fn a() { 1 }";
        assert_single_edit_parity(
            Lang::Rust,
            src,
            9,
            10,
            "\n    let x = 1;\n    x\n",
            "single-line -> multi-line",
        );
    }

    #[test]
    fn parity_replace_multi_with_single_line_rust() {
        let src = "fn a() {\n    let x = 1;\n    x\n}";
        // Replace lines 1-2 ("    let x = 1;\n    x\n") with " 42 ".
        assert_single_edit_parity(Lang::Rust, src, 9, 30, " 42 ", "multi-line -> single-line");
    }

    // ==== Whitespace-only edits =================================

    #[test]
    fn parity_insert_indent_whitespace_rust() {
        let src = "fn a() {\nlet x = 1;\n}";
        assert_single_edit_parity(Lang::Rust, src, 9, 9, "    ", "insert indentation");
    }

    #[test]
    fn parity_delete_trailing_whitespace_rust() {
        let src = "fn a() {    \n}";
        assert_single_edit_parity(Lang::Rust, src, 8, 12, "", "delete trailing whitespace");
    }

    // ==== Sequential edit batches ===============================

    #[test]
    fn parity_three_keystroke_burst_rust() {
        // Simulate typing "abc" one char at a time inside an
        // identifier slot.
        let (source_b, deltas) =
            apply_sequential_edits("fn  () {}", &[(3, 3, "a"), (4, 4, "b"), (5, 5, "c")]);
        assert_parity(
            Lang::Rust,
            "fn  () {}",
            &deltas,
            &source_b,
            "3-keystroke burst",
        );
    }

    #[test]
    fn parity_indent_batch_rust() {
        // Simulate `>>` over two lines: insert 4 spaces at the
        // start of each. Each subsequent edit's position is
        // shifted by the prior edit's effect.
        let src = "fn a() {\nlet x = 1;\nlet y = 2;\n}";
        // Line 1 starts at byte 9; line 2 starts at byte 20 in
        // the original. After inserting 4 spaces at byte 9, line
        // 2 starts at byte 24.
        let (source_b, deltas) = apply_sequential_edits(src, &[(9, 9, "    "), (24, 24, "    ")]);
        assert_parity(Lang::Rust, src, &deltas, &source_b, "indent batch");
    }

    #[test]
    fn parity_backspace_burst_rust() {
        // Simulate pressing backspace 3 times -- delete one byte
        // at a time from a known position.
        let src = "fn aaaa() {}";
        let (source_b, deltas) = apply_sequential_edits(src, &[(6, 7, ""), (5, 6, ""), (4, 5, "")]);
        assert_parity(Lang::Rust, src, &deltas, &source_b, "backspace burst");
    }

    // ==== Per-language coverage =================================

    #[test]
    fn parity_python_insert_in_def() {
        let src = "def f(x):\n    return x\n";
        assert_single_edit_parity(Lang::Python, src, 6, 6, "y, ", "insert arg in python def");
    }

    #[test]
    fn parity_python_delete_a_line() {
        let src = "def f(x):\n    y = 1\n    return x + y\n";
        // Delete "    y = 1\n" (indices 10..23).
        assert_single_edit_parity(Lang::Python, src, 10, 23, "", "delete line in python");
    }

    #[test]
    fn parity_python_replace_function_body() {
        let src = "def f(x):\n    return x\n";
        assert_single_edit_parity(
            Lang::Python,
            src,
            10,
            22,
            "    return x * 2",
            "replace python body",
        );
    }

    #[test]
    fn parity_javascript_insert_in_function() {
        let src = "function f(x) { return x; }";
        assert_single_edit_parity(
            Lang::JavaScript,
            src,
            24,
            24,
            " + 1",
            "insert in JS function",
        );
    }

    #[test]
    fn parity_javascript_replace_arrow_body() {
        let src = "const f = (x) => x;";
        assert_single_edit_parity(Lang::JavaScript, src, 17, 18, "x * 2", "replace arrow body");
    }

    #[test]
    fn parity_javascript_indent_batch() {
        let src = "function f() {\nlet x = 1;\nlet y = 2;\n}";
        let (source_b, deltas) = apply_sequential_edits(src, &[(15, 15, "  "), (28, 28, "  ")]);
        assert_parity(Lang::JavaScript, src, &deltas, &source_b, "JS indent batch");
    }

    // ==== Pathological / minimal-buffer cases ===================

    #[test]
    fn parity_edit_in_single_char_buffer_rust() {
        // Single-char buffer: delete the only char.
        assert_single_edit_parity(Lang::Rust, ";", 0, 1, "", "delete only char");
    }

    #[test]
    fn parity_insert_into_minimal_buffer_rust() {
        // Empty / near-empty buffer; insert valid syntax.
        assert_single_edit_parity(
            Lang::Rust,
            "fn",
            2,
            2,
            " a() {}",
            "insert into minimal buffer",
        );
    }

    #[test]
    fn parity_replace_with_empty_string_rust() {
        // Pure delete via replace with empty new text.
        let src = "fn a() {}\nfn b() {}";
        assert_single_edit_parity(Lang::Rust, src, 9, 19, "", "replace with empty");
    }

    // ==== Markdown =============================================
    //
    // Markdown is the trickiest language because of its
    // injections (block grammar -> inline grammar -> any fenced
    // language). The full-reparse path runs the same injection
    // pipeline as incremental, so tree-shape parity at the
    // outer (block) level is the right invariant -- inner
    // injection trees aren't part of `tree()` (they're managed
    // by the highlighter, not the parser).

    #[test]
    fn parity_markdown_insert_heading() {
        let src = "body paragraph\n";
        assert_single_edit_parity(Lang::Markdown, src, 0, 0, "# Title\n\n", "insert heading");
    }

    #[test]
    fn parity_markdown_replace_in_paragraph() {
        let src = "first line\nsecond line\n";
        assert_single_edit_parity(
            Lang::Markdown,
            src,
            6,
            10,
            "FOO",
            "replace word in paragraph",
        );
    }

    // ---- Slice C.2: intermediate-snapshot byte-alignment -------
    //
    // try_apply_intermediate must produce a snapshot whose tree's
    // byte ranges match the new source. Spans walked against this
    // intermediate must land at correct byte positions even though
    // Parser::parse hasn't run yet -- that's what makes lines
    // below a delete (or after a multi-byte insert) paint without
    // flicker.

    #[test]
    fn try_apply_intermediate_shifts_byte_ranges_to_new_source() {
        // Insert "X" at byte 3 of "fn a() {}" -> "fn aX() {}".
        // The function-name node was [3, 4) for "a"; after
        // tree.edit it should span [3, 5) for "aX". Pre-parse
        // shape (the tree still thinks of it as a single
        // identifier node), but byte ranges shifted.
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse_at("fn a() {}", 1);
        let edit = EditDelta {
            start_byte: 4,
            old_end_byte: 4,
            new_end_byte: 5,
            start_position: lattice_protocol::Position::new(0, 4),
            old_end_position: lattice_protocol::Position::new(0, 4),
            new_end_position: lattice_protocol::Position::new(0, 5),
        };
        let new_source = "fn aX() {}";
        s.try_apply_intermediate(new_source, 2, 1, &[edit])
            .expect("intermediate should succeed");
        // Source updated.
        assert_eq!(s.source(), new_source.as_bytes());
        // Tree present, byte ranges shifted.
        let tree = s.tree().expect("tree present");
        let root = tree.root_node();
        // Find the source_file's last byte; should match new
        // source length (10).
        assert_eq!(root.end_byte(), new_source.len());
    }

    #[test]
    fn try_apply_intermediate_then_reparse_matches_parse_at_with_edits() {
        // Two-stage path (try_apply_intermediate + reparse_with_cached_tree)
        // must produce the SAME final tree as the convenience
        // parse_at_with_edits path. Sanity check on the split.
        let edit = EditDelta {
            start_byte: 4,
            old_end_byte: 4,
            new_end_byte: 5,
            start_position: lattice_protocol::Position::new(0, 4),
            old_end_position: lattice_protocol::Position::new(0, 4),
            new_end_position: lattice_protocol::Position::new(0, 5),
        };
        let mut split = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        split.parse_at("fn a() {}", 1);
        split
            .try_apply_intermediate("fn aX() {}", 2, 1, &[edit])
            .unwrap();
        split.reparse_with_cached_tree(1);

        let mut combined = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        combined.parse_at("fn a() {}", 1);
        combined.parse_at_with_edits("fn aX() {}", 2, 1, &[edit]);

        assert_eq!(
            split.tree().unwrap().root_node().to_sexp(),
            combined.tree().unwrap().root_node().to_sexp(),
        );
    }

    #[test]
    fn try_apply_intermediate_shifts_ranges_after_line_delete() {
        // The user-reported scenario: deleting a line should
        // shift every subsequent byte range. Verify highlight
        // spans land at correct positions in the new source
        // BEFORE Parser::parse runs.
        let source_a = "fn a() {}\nfn b() {}\nfn c() {}";
        let source_b = "fn a() {}\nfn c() {}";
        // Delete "fn b() {}\n" at bytes 10..20.
        let edit = EditDelta {
            start_byte: 10,
            old_end_byte: 20,
            new_end_byte: 10,
            start_position: lattice_protocol::Position::new(1, 0),
            old_end_position: lattice_protocol::Position::new(2, 0),
            new_end_position: lattice_protocol::Position::new(1, 0),
        };
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse_at(source_a, 1);
        s.try_apply_intermediate(source_b, 2, 1, &[edit])
            .expect("intermediate should succeed");
        // Highlight against the intermediate. The "fn" keyword
        // span on the second line of the new source (was the
        // third line of the old source) should land at byte 10
        // (start of "fn c"), not at byte 20 (where the OLD tree
        // would have placed it without the tree.edit shift).
        let lines = s.highlight_lines(0, 2).unwrap();
        // Line 1 of the new source is "fn c() {}" -- it should
        // have at least one span (the "fn" keyword). With pre-
        // C.2 (no tree.edit shift), that span would land on the
        // wrong line.
        assert!(
            !lines[1].is_empty(),
            "line 1 of intermediate (post-delete) must have spans -- \
             this is what eliminates 'lines below flicker' on line delete"
        );
    }

    #[test]
    fn parity_markdown_delete_fenced_block() {
        let src = "intro\n\n```rust\nfn x() {}\n```\n\noutro\n";
        // Delete the fenced block (bytes 7..29 == "```rust\nfn x() {}\n```\n").
        assert_single_edit_parity(Lang::Markdown, src, 7, 29, "", "delete fenced code block");
    }
}
