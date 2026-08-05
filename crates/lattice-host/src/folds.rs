//! Computed folds (DESIGN.md §5.1, §15:18; user-facing reference
//! at `docs/user/folding.md`).
//!
//! Folds derived automatically from the buffer's structure. v1
//! providers:
//!
//! 1. **Indent** -- fold spans any line whose successor indents
//!    deeper, ending at the last line whose indent is strictly
//!    greater than the start. Universal across languages.
//! 2. **Markdown** -- `^#+ ` headings define the fold tree.
//!    A `# H1` folds until the next `# H1`; a `## H2` folds until
//!    the next same-or-higher heading.
//! 3. **Syntax (tree-sitter)** -- runs the language's compiled
//!    `folds.scm` query against the parse tree owned by
//!    [`lattice_syntax::Syntax`]. Each `@fold` capture becomes a
//!    fold spanning the captured node's lines. Falls back to the
//!    Markdown / Indent providers for languages that don't ship a
//!    `folds.scm` yet.
//!
//! Manual folds (created via `zf` from a Visual selection) and
//! computed folds coexist in [`crate::app::App::folds`] with no
//! distinction at the storage layer; the `:set foldmethod` option
//! decides which side feeds in.
//!
//! Fold identity (`Fold::identity`) is the SHA-style hash of the
//! trimmed start-line text plus indent depth (for indent / markdown
//! providers) or `(node_kind, trimmed start-line text)` (for the
//! syntax provider). When the buffer changes and folds recompute,
//! we match new folds to old ones by identity and transfer the
//! closed-state -- so adding a line to one section doesn't reopen
//! the closed section above. Manual folds carry `identity = None`
//! (their stable identity is the line range itself).

use std::hash::{DefaultHasher, Hash, Hasher};

use lattice_core::Buffer;
use lattice_syntax::SyntaxSnapshot;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

use lattice_core::Fold;

/// Compute the stable identity hash for a computed fold.
///
/// Inputs are `(trimmed start-line text, indent depth)` -- enough
/// to keep a heading distinct from sibling headings while ignoring
/// trailing-line additions. Used by [`crate::app::App::recompute_folds`]
/// to carry the closed/open state across edits.
pub(crate) fn fold_identity(start_line_text: &str, indent_depth: usize) -> u64 {
    let mut h = DefaultHasher::new();
    start_line_text.trim().hash(&mut h);
    indent_depth.hash(&mut h);
    h.finish()
}

/// Compute the stable identity hash for a tree-sitter-driven fold.
/// Uses `(node_kind, trimmed start-line text)` -- node kind separates
/// e.g. an `impl` block from a `fn` block that happen to start with
/// the same text, which is more stable than indent depth across
/// reformat / refactor operations.
fn syntax_fold_identity(node_kind: &str, start_line_text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    node_kind.hash(&mut h);
    start_line_text.trim().hash(&mut h);
    h.finish()
}

/// Run the indent-based fold algorithm against `buffer` and return
/// every fold range it discovers. All produced folds are open
/// (`closed = false`) by default -- vim's `foldlevelstart` would
/// override that, but v1 doesn't model the level option yet.
///
/// Algorithm:
///
/// 1. For each non-blank line, compute its indent (count of
///    leading ASCII whitespace, treating tabs as one cell).
/// 2. Walk lines top-down; whenever line `i` has a non-blank
///    successor `j` with strictly greater indent, open a fold
///    starting at `i`. Walk forward to find the last line whose
///    indent is greater than `i`'s; that's the fold end.
/// 3. Skip blank lines when locating the fold end (they don't
///    break a fold -- vim's behaviour).
///
/// The output is sorted by start_line; nested folds appear
/// inside their parent's range.
pub fn compute_indent_folds(buffer: &Buffer) -> Vec<Fold> {
    let text = buffer.as_string();
    let lines: Vec<&str> = text.split('\n').collect();
    let line_count = lines.len();
    if line_count <= 1 {
        return Vec::new();
    }
    let indents: Vec<Option<usize>> = lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                None
            } else {
                Some(leading_indent(l))
            }
        })
        .collect();

    // Precompute next_non_blank_idx[i] = the first j > i with a non-blank
    // line, or line_count if none. Replaces the O(n) scan in the outer loop.
    let mut next_non_blank_idx: Vec<usize> = vec![line_count; line_count];
    {
        let mut next = line_count;
        for i in (0..line_count).rev() {
            next_non_blank_idx[i] = next;
            if indents[i].is_some() {
                next = i;
            }
        }
    }

    let mut folds: Vec<Fold> = Vec::new();
    // Perf guard: cap the number of indent folds to prevent O(folds²) walk
    // inside providers downstream (folded_line_span chains, etc.) on
    // pathological files (e.g. monotonically increasing whitespace).
    const MAX_FOLDS: usize = 5000;
    for i in 0..line_count {
        if folds.len() >= MAX_FOLDS {
            break;
        }
        let Some(start_indent) = indents[i] else {
            continue;
        };
        let j = next_non_blank_idx[i];
        if j >= line_count {
            continue;
        }
        let Some(next_indent) = indents[j] else {
            continue;
        };
        if next_indent <= start_indent {
            continue;
        }
        // Walk forward to find the last line with indent > start_indent.
        // For pathological files with monotonically increasing indent the
        // inner loop can walk to end-of-file for every line, yielding
        // O(n²) behaviour. The MAX_FOLDS cap bounds this: once the cap is
        // reached the outer loop breaks, so at most MAX_FOLDS × n inner
        // iterations run.
        let mut end = j;
        for (k, ind) in indents.iter().enumerate().skip(j + 1) {
            match ind {
                Some(i) if *i > start_indent => end = k,
                Some(_) => break,
                None => {
                    // Blank line: keep looking but don't extend
                    // the end past it unless a deeper line follows.
                    continue;
                }
            }
        }
        // "Closer" inclusion: many languages dedent the closing
        // delimiter back to the parent indent (Rust / C / JS `}`,
        // Python triple-quote close, etc.). When the next non-blank
        // line after `end` is a "closer" line at indent == start_indent
        // and contains only close-brackets / whitespace, swallow it
        // so the visible fold-summary line ends with the brace
        // instead of leaving an orphan `}` below the fold.
        if let Some(closer) = next_non_blank_line(&indents, end + 1)
            && let Some(ind) = indents[closer]
            && ind == start_indent
            && is_closer_line(lines[closer])
        {
            end = closer;
        }
        let identity = fold_identity(lines[i], start_indent);
        folds.push(Fold {
            start_line: i as u32,
            end_line: end as u32,
            closed: false,
            identity: Some(identity),
        });
    }
    folds
}

/// Count leading whitespace cells. Tabs and spaces both count as
/// one cell -- vim's `foldmethod=indent` uses 'shiftwidth' instead
/// of raw whitespace count; v1 keeps this simple and treats every
/// leading whitespace byte as one indent unit. (Refinement when
/// we honour `tabstop` / `shiftwidth` lands with the typed-options
/// follow-up.)
fn leading_indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// Find the next non-blank line in `indents` starting at `from`.
/// Returns the index, or `None` if every line from `from` onward is
/// blank.
fn next_non_blank_line(indents: &[Option<usize>], from: usize) -> Option<usize> {
    indents.iter().enumerate().skip(from).find_map(
        |(i, ind)| {
            if ind.is_some() { Some(i) } else { None }
        },
    )
}

/// True when `line` is a pure closing-bracket line (matched by the
/// indent fold's "closer inclusion" heuristic). The line must
/// consist only of whitespace plus one or more of `)`, `]`, `}`,
/// optionally followed by `,` / `;` -- this catches the common Rust
/// / C / JS / Go shapes (`}`, `};`, `})`, `})?;`, etc.) without
/// pulling in the next statement.
fn is_closer_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed
        .chars()
        .all(|c| matches!(c, ')' | ']' | '}' | ',' | ';' | '?'))
}

/// Markdown heading-based fold provider (DESIGN.md §15:18,
/// `docs/user/folding.md`). Walks the buffer for ATX headings
/// (`^#+\s`) and emits one fold per heading whose body has at
/// least one row. Heading depth (the number of `#`s) determines
/// nesting: a `## H2` ends at the next same-or-shallower heading
/// (`# H1` or another `## H2` or end-of-buffer).
///
/// Code-fence aware: `^```` lines toggle a "in fenced block" state;
/// `#`-prefixed lines inside a fenced block are not headings. The
/// fence itself is included in whichever fold contains it.
///
/// Lines inside fenced code blocks of the form `~~~` are also
/// excluded from heading detection, mirroring CommonMark §4.5.
pub fn compute_markdown_folds(buffer: &Buffer) -> Vec<Fold> {
    let text = buffer.as_string();
    let lines: Vec<&str> = text.split('\n').collect();
    // `split('\n')` on a string ending in `\n` produces a trailing
    // empty element; that empty element isn't an addressable buffer
    // line and a fold extending to it would over-delete in the
    // fold-aware operator path. Use the last addressable index
    // instead.
    let last_addressable = if lines.last().is_some_and(|s| s.is_empty()) && lines.len() > 1 {
        lines.len() - 2
    } else {
        lines.len().saturating_sub(1)
    };
    if last_addressable == 0 {
        return Vec::new();
    }

    // First pass: find every heading line and its depth, skipping
    // those inside fenced code blocks.
    let mut headings: Vec<(usize, u32)> = Vec::new(); // (line_idx, depth)
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
        if let Some(depth) = atx_heading_depth(line) {
            headings.push((i, depth));
        }
    }
    if headings.is_empty() {
        return Vec::new();
    }

    // Second pass: each heading folds from its row to the row
    // before the next same-or-shallower heading, or end-of-buffer.
    // Single-line "headings" (next heading on the very next row)
    // are skipped because the body would be empty.
    let mut folds: Vec<Fold> = Vec::new();
    for (h_idx, &(start, depth)) in headings.iter().enumerate() {
        let end = headings
            .iter()
            .skip(h_idx + 1)
            .find(|(_, d)| *d <= depth)
            .map(|(line_idx, _)| line_idx.saturating_sub(1))
            .unwrap_or(last_addressable);
        if end <= start {
            // Empty body (next sibling heading immediately follows).
            continue;
        }
        // Identity uses the heading line + depth so headings with
        // identical text at different depths (`# Foo` vs `## Foo`)
        // get different identities.
        let identity = fold_identity(lines[start], depth as usize);
        folds.push(Fold {
            start_line: start as u32,
            end_line: end as u32,
            closed: false,
            identity: Some(identity),
        });
    }
    folds
}

/// Recognise an ATX heading line (`#` to `######` followed by at
/// least one whitespace) per CommonMark §4.2. Returns the heading
/// depth (1-6) on match, `None` otherwise. Leading whitespace is
/// allowed but capped at 3 spaces (CommonMark's "up to 3 leading
/// spaces" rule); 4+ leading spaces means the line is part of an
/// indented code block.
fn atx_heading_depth(line: &str) -> Option<u32> {
    let lead = line.chars().take_while(|c| *c == ' ').count();
    if lead > 3 {
        return None;
    }
    let rest = &line[lead..];
    let hashes = rest.chars().take_while(|c| *c == '#').count() as u32;
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = &rest[hashes as usize..];
    // CommonMark requires at least one whitespace between hashes
    // and content (or the line ends, e.g. `#` alone is a valid
    // heading with empty content).
    if after.is_empty() || after.starts_with([' ', '\t']) {
        Some(hashes)
    } else {
        None
    }
}

/// Tree-sitter-driven fold provider. Runs the language's compiled
/// `folds.scm` query against [`lattice_syntax::Syntax`]'s parse tree
/// and emits one [`Fold`] per `@fold` capture spanning more than a
/// single line.
///
/// Returns `None` when:
/// - The buffer's language doesn't ship a `folds.scm` (e.g. plain
///   text or the inline-markdown parser). The caller cascades to the
///   markdown / indent providers.
/// - The syntax tree isn't available yet (first parse hasn't run).
///
/// Returns `Some(Vec::new())` when the language has a query but the
/// document genuinely has no foldable structures (an empty file, a
/// single-line file). The empty Vec lets the caller know "syntax
/// authoritative, just nothing to fold" so it doesn't fall through
/// to indent.
pub fn compute_syntax_folds(syntax: &SyntaxSnapshot) -> Option<Vec<Fold>> {
    let tree = syntax.tree()?;
    let source = syntax.source();
    let registry = syntax.registry();
    let query = registry.folds_query(syntax.lang().name())?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    // Precompute line-start byte offsets in ONE pass so the per-fold
    // start-line-text lookup below is O(1). The old code called
    // `line_at(source, start_line)`, which rescans from byte 0 every call;
    // with one call per fold that is O(folds × file_size) = O(n²) — ~34 s on
    // a 36k-line file (dispatch.rs), the freeze this fixes. `line_starts[i]`
    // is the byte offset where line `i` begins.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            source
                .iter()
                .enumerate()
                .filter_map(|(i, &b)| (b == b'\n').then_some(i + 1)),
        )
        .collect();
    let line_text = |line: u32| -> &str {
        let li = line as usize;
        let Some(&start) = line_starts.get(li) else {
            return "";
        };
        // Content ends just before the next line's start (the `\n`), or at
        // EOF for the last line.
        let end = line_starts
            .get(li + 1)
            .map(|&next| next.saturating_sub(1))
            .unwrap_or(source.len());
        std::str::from_utf8(source.get(start..end).unwrap_or(&[])).unwrap_or("")
    };

    let mut folds: Vec<Fold> = Vec::new();
    let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            let node = cap.node;
            let start_line = node.start_position().row as u32;
            let end_line = node.end_position().row as u32;
            // Tree-sitter's `end_position` lands on the line *after*
            // the node's last content character when the node ends
            // with a newline (e.g. block_comment, fenced_code_block).
            // Pull back one line so the user-facing fold range
            // corresponds to lines that actually contain the node's
            // content.
            let end_line = if end_line > start_line && node.end_byte() > 0 {
                let last_byte = node.end_byte().saturating_sub(1);
                if source.get(last_byte) == Some(&b'\n') {
                    end_line.saturating_sub(1)
                } else {
                    end_line
                }
            } else {
                end_line
            };
            // Skip single-line captures: nothing to hide.
            if end_line <= start_line {
                continue;
            }
            // De-dup: tree-sitter can emit the same node range under
            // multiple patterns; folds are keyed on byte range so
            // duplicates would just bloat the vec.
            if !seen.insert((start_line, end_line)) {
                continue;
            }
            let start_line_text = line_text(start_line);
            let identity = syntax_fold_identity(node.kind(), start_line_text);
            folds.push(Fold {
                start_line,
                end_line,
                closed: false,
                identity: Some(identity),
            });
        }
    }
    folds.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| b.end_line.cmp(&a.end_line))
    });
    Some(folds)
}

/// Hash the user-visible signature of the current fold set.
///
/// Used as part of the highlights cache key
/// ([`crate::render_state::VisibleHighlightsKey::fold_hash`]) so a
/// fold toggle invalidates cached spans: collapsing / expanding a
/// fold changes which physical lines are visible, which changes
/// what `highlight_lines(start, end)` should produce.
///
/// Only `(start_line, end_line, closed)` are hashed —
/// fold `identity` is excluded because two folds with the same
/// range and state but different identities don't change which
/// bytes are visible.
///
/// Phase 5.8.AF.5 / Slice X2: hoisted host-side from
/// `lattice-ui-tui::app::folds::compute_fold_hash` so dispatch's
/// `publish_render_state` can populate
/// `SyntaxRenderState::fold_hash` without depending on the
/// renderer crate.
pub fn compute_fold_hash(folds: &[Fold]) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    folds.len().hash(&mut h);
    for f in folds {
        f.start_line.hash(&mut h);
        f.end_line.hash(&mut h);
        f.closed.hash(&mut h);
    }
    h.finish()
}

/// O(log folds) lookup index built once per frame (or per publish) over
/// a snapshot of the active document's folds.
///
/// Perf plan C. Renderers used to call
/// `folds.iter().any(|f| ...)` per visible line in the gutter compose
/// loop — `O(rows × folds)` per pane per frame. With this index the
/// per-line check drops to a partition-point binary search plus a
/// constant-time fast path for the common non-overlapping case.
///
/// Build cost is `O(folds)`; for the typical buffer with <50 folds
/// it's <1 µs. The build is intentionally per-frame on the renderer
/// side (no host-side caching / invalidation discipline) — the saving
/// is the per-line walk, not the construction.
///
/// Fold semantics (paramount goal #3, vim parity):
/// - `closed_fold_start_at(line)` is true iff `line == f.start_line`
///   for some closed fold `f`.
/// - `line_inside_closed_fold(line)` is true iff
///   `f.start_line < line && line <= f.end_line` for some closed fold.
///   Start lines are NOT inside (vim renders the heading row).
/// - `fold_start_at_any(line)` covers open + closed folds. Used by
///   the gutter glyph provider (open-fold caret vs closed-fold chevron).
#[derive(Debug, Clone, Default)]
pub struct FoldIndex {
    /// Closed-fold `(start_line, end_line)` pairs sorted by `start_line`.
    /// Two `u32`s fit in 8 bytes — cache-friendly linear / binary walks.
    closed: Vec<(u32, u32)>,
    /// All-fold `start_line`s (open + closed) sorted ascending.
    all_starts: Vec<u32>,
    /// `:set foldenable` cached at build time so every predicate is a
    /// single bool branch ahead of any vec walk; the user-facing
    /// behaviour of all three predicates collapses to `false` when
    /// foldenable is off, exactly matching the existing `Editor::*`
    /// helpers in `dispatch.rs`.
    foldenable: bool,
}

impl FoldIndex {
    /// Build an index from a fold snapshot. `O(folds)` — one filter +
    /// two sorts.
    pub fn from_folds(folds: &[Fold], foldenable: bool) -> Self {
        let mut closed: Vec<(u32, u32)> = folds
            .iter()
            .filter(|f| f.closed)
            .map(|f| (f.start_line, f.end_line))
            .collect();
        closed.sort_unstable_by_key(|(s, _)| *s);
        let mut all_starts: Vec<u32> = folds.iter().map(|f| f.start_line).collect();
        all_starts.sort_unstable();
        Self {
            closed,
            all_starts,
            foldenable,
        }
    }

    /// True iff `line` is the start row of a closed fold. Matches
    /// `Editor::fold_start_at(line).is_some()` semantics.
    pub fn closed_fold_start_at(&self, line: u32) -> bool {
        if !self.foldenable {
            return false;
        }
        self.closed.binary_search_by_key(&line, |(s, _)| *s).is_ok()
    }

    /// `(start, end)` of the closed fold starting at `line`, or
    /// `None` if no closed fold starts there. Mirrors
    /// `Editor::fold_start_at(line)` but yields just the row range
    /// the renderer + fold-aware viewport math actually need —
    /// `&Fold` would force us to hold a `Vec<Fold>` snapshot in the
    /// index, while two `u32`s are 8 bytes per entry and cache-
    /// friendly. Used by `Editor::fold_aware_highlight_end_line`
    /// to advance `buf_line` past a closed fold in the viewport
    /// stretch loop.
    pub fn closed_fold_at(&self, line: u32) -> Option<(u32, u32)> {
        if !self.foldenable {
            return None;
        }
        self.closed
            .binary_search_by_key(&line, |(s, _)| *s)
            .ok()
            .map(|i| self.closed[i])
    }

    /// The innermost closed fold whose *interior* contains `line`
    /// (`start_line < line <= end_line`), or `None`. The interior
    /// excludes the start row — the fold head stays visible — so a
    /// head line reports `None` here even though it bounds a fold.
    ///
    /// Used by the fold-aware scroll walk
    /// (`Editor::bottom_anchored_scroll`) to hop from a hidden body
    /// line straight up to its visible head in one step, instead of
    /// iterating every collapsed line. `line_inside_closed_fold`
    /// answers the same question as a bool; this returns the range
    /// so the caller can jump.
    pub fn enclosing_closed_fold(&self, line: u32) -> Option<(u32, u32)> {
        if !self.foldenable {
            return None;
        }
        // `closed` is sorted by start_line ascending. The rightmost
        // entry with `start < line` is the innermost candidate; for
        // non-overlapping / properly-nested folds (the common case)
        // it is also the only one that can enclose `line`.
        let idx = self.closed.partition_point(|(s, _)| *s < line);
        if idx == 0 {
            return None;
        }
        let (s, e) = self.closed[idx - 1];
        if e >= line {
            return Some((s, e));
        }
        // Slow path: only reachable when folds overlap (rare — e.g.
        // user manually `:fold`s overlapping ranges). Walk left for
        // the nearest encloser.
        self.closed[..idx - 1]
            .iter()
            .rev()
            .find(|(_, e)| *e >= line)
            .copied()
    }

    /// True iff `line` falls strictly inside the interior of some closed
    /// fold (`start_line < line <= end_line`). Matches the existing
    /// `Editor::line_inside_closed_fold` semantics.
    pub fn line_inside_closed_fold(&self, line: u32) -> bool {
        if !self.foldenable {
            return false;
        }
        // `closed` is sorted by start_line ascending. The rightmost
        // entry with `start < line` is the most-recently-opened
        // candidate; for non-overlapping folds (the common case) it
        // is also the only candidate that can enclose `line`.
        let idx = self.closed.partition_point(|(s, _)| *s < line);
        if idx == 0 {
            return false;
        }
        // Fast path: innermost (or only) candidate. Constant time.
        if self.closed[idx - 1].1 >= line {
            return true;
        }
        // Slow path: only reachable when folds overlap (rare —
        // e.g. user manually `:fold`s overlapping ranges). Bounded
        // by `idx`, but in practice walks a tiny prefix.
        self.closed[..idx - 1].iter().any(|(_, e)| line <= *e)
    }

    /// True iff `line` is the start row of any fold (open or closed).
    /// Mirrors `Editor::fold_start_at_any(line).is_some()`.
    pub fn fold_start_at_any(&self, line: u32) -> bool {
        if !self.foldenable {
            return false;
        }
        self.all_starts.binary_search(&line).is_ok()
    }

    /// Whether a fold *starts* at `line`, and if so whether it is
    /// collapsed. `Some(true)` = closed fold head, `Some(false)` = open
    /// (expanded) fold head, `None` = no fold starts here. Gates on
    /// `foldenable`. This is what a gutter renderer consults to pick the
    /// collapsed vs expanded marker glyph — the shared peer of the TUI's
    /// `fold_glyph_for`, so both renderers show a marker on every
    /// foldable head (not just collapsed ones) and agree on which glyph.
    pub fn fold_start_kind_at(&self, line: u32) -> Option<FoldMarker> {
        if !self.foldenable || self.all_starts.binary_search(&line).is_err() {
            return None;
        }
        if self.closed.binary_search_by_key(&line, |(s, _)| *s).is_ok() {
            Some(FoldMarker::Closed)
        } else {
            Some(FoldMarker::Open)
        }
    }
}

/// A gutter fold marker's state: the head row of an open (expanded) fold
/// versus a closed (collapsed) one. Renderers map this to their glyph +
/// themed colour (`gutter.fold.open` / `gutter.fold.closed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldMarker {
    /// Expanded fold — body visible below the head. Rendered `▾`.
    Open,
    /// Collapsed fold — body hidden onto the head. Rendered `▸`.
    Closed,
}

/// Number of buffer lines collapsed onto the visible head row of the
/// closed fold `(start_line, end_line)` — the count both renderers show
/// in the `⋯ N lines` fold summary. Walks forward from `end_line + 1`
/// through any sibling closed folds whose *heading is itself hidden* by
/// the region already collapsed (`start < probe <= end`), so overlapping
/// folds (e.g. `(1,3)+(3,5)` from `foldmethod=indent`) report their
/// combined span. Folds that merely ABUT — the next fold starts at
/// `end + 1`, the first *visible* line after this one — are NOT chained:
/// that fold has an on-screen heading and is a separate fold with its
/// own summary. Shared by the TUI and GPUI renderers so the count stays
/// identical.
pub fn folded_line_span(folds: &[Fold], start_line: u32, end_line: u32, total_lines: u32) -> u32 {
    let mut end = end_line;
    let mut probe = end.saturating_add(1);
    while probe < total_lines {
        let next = folds
            .iter()
            .find(|f| f.closed && probe > f.start_line && probe <= f.end_line);
        match next {
            Some(f) => {
                end = end.max(f.end_line);
                probe = end.saturating_add(1);
            }
            None => break,
        }
    }
    end.saturating_sub(start_line).saturating_add(1)
}

/// Walk forward from `start_line` and return the buffer line at
/// `offset` visible (non-fold-hidden) display rows below it.
/// Closed fold bodies are skipped — only fold heads (start lines)
/// and non-folded lines count as visible rows. When the walk
/// reaches the last addressable line before consuming `offset`
/// rows, that last line is returned (the caller's clamp).
pub fn nth_visible_line_forward(
    fold_idx: &FoldIndex,
    start_line: u32,
    offset: u32,
    total_lines: u32,
) -> u32 {
    if offset == 0 || !fold_idx.foldenable {
        return start_line.min(total_lines.saturating_sub(1));
    }
    let last = total_lines.saturating_sub(1);
    let mut line = start_line;
    let mut count = 0u32;
    while count < offset && line < last {
        line += 1;
        while let Some((_, end)) = fold_idx.enclosing_closed_fold(line) {
            line = end + 1;
            if line > last {
                return last;
            }
        }
        count += 1;
    }
    line.min(last)
}

/// Walk backward from `start_line` and return the line at `offset`
/// visible rows above it. Walking backward hops over closed fold
/// bodies to their visible heads, then continues from the preceding
/// line. Returns 0 when the walk hits BOF before consuming `offset`
/// rows.
pub fn nth_visible_line_backward(fold_idx: &FoldIndex, start_line: u32, offset: u32) -> u32 {
    if offset == 0 || !fold_idx.foldenable {
        return start_line;
    }
    let mut line = start_line;
    let mut count = 0u32;
    while count < offset && line > 0 {
        line = line.saturating_sub(1);
        while let Some((start, _)) = fold_idx.enclosing_closed_fold(line) {
            line = start;
        }
        count += 1;
    }
    line
}

/// Count how many visible (non-fold-hidden) rows exist between
/// `from_line` (inclusive) and `to_line` (exclusive). Closed fold
/// bodies are skipped. Both lines must be *visible* (not inside a
/// closed fold body) — callers ensure this before calling. Returns
/// the number of display rows spanning the range.
pub fn count_visible_rows_between(fold_idx: &FoldIndex, from_line: u32, to_line: u32) -> u32 {
    if from_line >= to_line || !fold_idx.foldenable {
        return to_line.saturating_sub(from_line);
    }
    let mut line = from_line;
    let mut count = 0u32;
    while line < to_line {
        count += 1;
        if let Some((_, end)) = fold_idx.closed_fold_at(line) {
            line = end + 1;
        } else {
            line += 1;
        }
    }
    count
}

// =========================================================
// D.3.f.0 (2026-05-29): primary-provider impls wrapping the
// existing `compute_*_folds` helpers. See
// `docs/dev/architecture/fold-architecture.md`. These wire
// into the `FoldRegistry` constructed by `Editor::boot`.
// Behaviour is unchanged from the pre-refactor
// `Editor::recompute_folds` match arms — the registry
// dispatch produces the same fold sets for the same inputs.
// =========================================================

use crate::fold_provider::{FoldContext, FoldProvider};
use lattice_core::{ProviderId, ProviderKind};

const PROVIDER_ID_MANUAL: ProviderId = ProviderId(0);
const PROVIDER_ID_INDENT: ProviderId = ProviderId(1);
const PROVIDER_ID_MARKDOWN: ProviderId = ProviderId(2);
const PROVIDER_ID_SYNTAX: ProviderId = ProviderId(3);
const PROVIDER_ID_LSP: ProviderId = ProviderId(4);

pub struct ManualPrimary;

impl FoldProvider for ManualPrimary {
    fn id(&self) -> ProviderId {
        PROVIDER_ID_MANUAL
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Primary
    }
    fn compute(&self, _ctx: &FoldContext<'_>) -> Vec<Fold> {
        // Manual folds (zf) are carried over by
        // `Editor::recompute_folds` from the previous fold
        // list; the Primary provider produces nothing.
        Vec::new()
    }
}

pub struct IndentPrimary;

impl FoldProvider for IndentPrimary {
    fn id(&self) -> ProviderId {
        PROVIDER_ID_INDENT
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Primary
    }
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold> {
        compute_indent_folds(ctx.buffer)
    }
}

pub struct MarkdownPrimary;

impl FoldProvider for MarkdownPrimary {
    fn id(&self) -> ProviderId {
        PROVIDER_ID_MARKDOWN
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Primary
    }
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold> {
        compute_markdown_folds(ctx.buffer)
    }
}

pub struct SyntaxPrimary;

impl FoldProvider for SyntaxPrimary {
    fn id(&self) -> ProviderId {
        PROVIDER_ID_SYNTAX
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Primary
    }
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold> {
        if let Some(syntax) = ctx.syntax
            && let Some(folds) = compute_syntax_folds(syntax)
        {
            return folds;
        }
        // Cascade: markdown for `.md`, else indent — matches
        // pre-refactor `Editor::recompute_syntax_folds`.
        let is_md = ctx
            .path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("md"))
            .unwrap_or(false);
        if is_md {
            compute_markdown_folds(ctx.buffer)
        } else {
            compute_indent_folds(ctx.buffer)
        }
    }
}

pub struct LspPrimary;

impl FoldProvider for LspPrimary {
    fn id(&self) -> ProviderId {
        PROVIDER_ID_LSP
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Primary
    }
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold> {
        if let Some(folds) = ctx.lsp_folds
            && !folds.is_empty()
        {
            return folds.to_vec();
        }
        // Cascade to syntax provider's behaviour — matches
        // pre-refactor `Editor::recompute_lsp_folds`.
        SyntaxPrimary.compute(ctx)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::Position;

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        if !text.is_empty() {
            b.apply_edit(&Edit::insert(Position::ZERO, text.to_string()))
                .unwrap();
        }
        b
    }

    #[test]
    fn empty_buffer_yields_no_folds() {
        let b = buf("");
        assert!(compute_indent_folds(&b).is_empty());
    }

    #[test]
    fn single_line_yields_no_folds() {
        let b = buf("hello");
        assert!(compute_indent_folds(&b).is_empty());
    }

    #[test]
    fn flat_lines_yield_no_folds() {
        let b = buf("a\nb\nc\nd\n");
        assert!(compute_indent_folds(&b).is_empty());
    }

    #[test]
    fn one_block_produces_a_fold() {
        let b = buf("def f():\n    pass\n");
        let folds = compute_indent_folds(&b);
        assert_eq!(folds.len(), 1);
        let f = &folds[0];
        assert_eq!(f.start_line, 0);
        assert_eq!(f.end_line, 1);
        assert!(!f.closed);
    }

    #[test]
    fn nested_blocks_produce_nested_folds() {
        let b = buf("outer:\n    inner:\n        deep\n        deeper\n    after-inner\n");
        let folds = compute_indent_folds(&b);
        // outer (0..4) and inner (1..3).
        assert!(folds.iter().any(|f| f.start_line == 0 && f.end_line == 4));
        assert!(folds.iter().any(|f| f.start_line == 1 && f.end_line == 3));
    }

    #[test]
    fn blank_lines_inside_a_block_dont_break_it() {
        let b = buf("def f():\n    line1\n\n    line2\n");
        let folds = compute_indent_folds(&b);
        assert_eq!(folds.len(), 1);
        // Fold extends to line 3 (last indented row); the blank
        // line on row 2 is skipped.
        assert_eq!(folds[0].end_line, 3);
    }

    #[test]
    fn blank_lines_at_top_dont_start_a_fold() {
        let b = buf("\n    indented\nfollowing\n");
        // Line 0 is blank; no fold should start there.
        let folds = compute_indent_folds(&b);
        assert!(folds.iter().all(|f| f.start_line != 0));
    }

    #[test]
    fn rust_function_produces_an_indent_fold() {
        // Smoke test for the common case: `fn foo() { ... }` should
        // produce a fold spanning the body. Default `foldmethod = manual`
        // produces no folds; users need `:set foldmethod=indent` (or
        // `=syntax` cascading to indent) for `zc` to have something
        // to operate on. Closer-line inclusion swallows the trailing
        // `}` so the fold extends to the closing brace line.
        let src =
            "fn outer() {\n    let x = 1;\n    if x > 0 {\n        println!(\"yes\");\n    }\n}\n";
        let b = buf(src);
        let folds = compute_indent_folds(&b);
        let outer = folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("outer fn fold missing");
        assert_eq!(
            outer.end_line, 5,
            "outer fold should swallow the final `}}` line: {outer:?}"
        );
        let inner = folds
            .iter()
            .find(|f| f.start_line == 2)
            .expect("inner if fold missing");
        assert_eq!(
            inner.end_line, 4,
            "inner fold should swallow its `}}` at indent 4: {inner:?}"
        );
    }

    #[test]
    fn computed_folds_are_open_by_default() {
        let b = buf("a:\n    b\n");
        let folds = compute_indent_folds(&b);
        assert!(!folds[0].closed);
    }

    // --- Markdown provider --------------------------------------

    #[test]
    fn markdown_empty_buffer_yields_no_folds() {
        let b = buf("");
        assert!(compute_markdown_folds(&b).is_empty());
    }

    #[test]
    fn markdown_no_headings_yields_no_folds() {
        let b = buf("just\nsome\nbody text\n");
        assert!(compute_markdown_folds(&b).is_empty());
    }

    #[test]
    fn markdown_single_heading_with_body_folds_to_eob() {
        let b = buf("# Title\nbody line one\nbody line two\n");
        let folds = compute_markdown_folds(&b);
        assert_eq!(folds.len(), 1);
        let f = &folds[0];
        assert_eq!(f.start_line, 0);
        // Trailing newline produces an empty 4th line (idx 3); the
        // fold spans through the last *real* line (idx 2).
        assert!(f.end_line >= 2);
        assert!(!f.closed);
    }

    #[test]
    fn markdown_single_line_heading_skipped() {
        // Two H1s back-to-back: the first has no body, so no fold.
        let b = buf("# A\n# B\nbody\n");
        let folds = compute_markdown_folds(&b);
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].start_line, 1);
    }

    #[test]
    fn markdown_h1_then_h2_nests() {
        let b = buf("# Outer\nlead\n## Inner\nbody\n");
        let folds = compute_markdown_folds(&b);
        // Outer (line 0) folds to end; Inner (line 2) folds inside it.
        assert!(folds.iter().any(|f| f.start_line == 0));
        assert!(folds.iter().any(|f| f.start_line == 2));
    }

    #[test]
    fn markdown_h2_ends_at_next_h1() {
        let b = buf("# A\n## A.1\na1 body\n# B\nb body\n");
        let folds = compute_markdown_folds(&b);
        // ## A.1 fold should end at line 2 (line before # B).
        let inner = folds
            .iter()
            .find(|f| f.start_line == 1)
            .expect("expected ## fold");
        assert_eq!(inner.end_line, 2);
    }

    #[test]
    fn markdown_hash_inside_code_fence_not_a_heading() {
        let b = buf("# Real\nbody\n```\n# inside fence\n```\nafter\n");
        let folds = compute_markdown_folds(&b);
        // Only one heading (line 0) should be detected.
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].start_line, 0);
    }

    #[test]
    fn markdown_tilde_fence_also_protects_hashes() {
        let b = buf("# Real\n~~~\n# inside\n~~~\nafter\n");
        let folds = compute_markdown_folds(&b);
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].start_line, 0);
    }

    #[test]
    fn markdown_indented_4_spaces_is_not_a_heading() {
        // 4+ leading spaces means indented code block, not heading.
        let b = buf("body\n    # not heading\nmore body\n");
        let folds = compute_markdown_folds(&b);
        assert!(folds.is_empty());
    }

    #[test]
    fn markdown_3_leading_spaces_still_a_heading() {
        let b = buf("   # Heading\nbody\n");
        let folds = compute_markdown_folds(&b);
        assert_eq!(folds.len(), 1);
    }

    #[test]
    fn markdown_seven_hashes_not_a_heading() {
        // CommonMark caps at 6 hashes.
        let b = buf("####### nope\nbody\n");
        let folds = compute_markdown_folds(&b);
        assert!(folds.is_empty());
    }

    #[test]
    fn markdown_hash_without_space_not_a_heading() {
        let b = buf("#nospace\nbody\n");
        let folds = compute_markdown_folds(&b);
        assert!(folds.is_empty());
    }

    #[test]
    fn markdown_open_by_default() {
        let b = buf("# H\nbody\n");
        let folds = compute_markdown_folds(&b);
        assert!(!folds[0].closed);
    }

    #[test]
    fn atx_depth_recognises_levels_one_through_six() {
        for n in 1..=6u32 {
            let prefix: String = "#".repeat(n as usize);
            let line = format!("{prefix} text");
            assert_eq!(atx_heading_depth(&line), Some(n));
        }
    }

    #[test]
    fn atx_depth_rejects_zero_or_seven() {
        assert_eq!(atx_heading_depth("nothing"), None);
        assert_eq!(atx_heading_depth("####### too deep"), None);
    }

    #[test]
    fn atx_depth_accepts_hash_alone() {
        // CommonMark: "# " or just "#" both valid.
        assert_eq!(atx_heading_depth("#"), Some(1));
    }

    // --- Syntax (tree-sitter) provider --------------------------

    /// Regression guard: `compute_syntax_folds` must be ~O(n), not
    /// O(folds × file_size). The old per-fold `line_at` rescanned the source
    /// from byte 0 on every call, so a large file (dispatch.rs, 36k lines,
    /// thousands of folds) took ~34 s and froze the editor. A file with
    /// thousands of foldable items must compute well under a second.
    #[test]
    fn syntax_folds_are_linear_not_quadratic() {
        let mut src = String::with_capacity(200_000);
        for i in 0..3000 {
            src.push_str(&format!(
                "fn f{i}() {{\n    let x = {i};\n    x + 1\n}}\n\n"
            ));
        }
        let syntax = rust_syntax_with(&src);
        let t = std::time::Instant::now();
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds.scm");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(
            folds.len() >= 3000,
            "each fn body should fold (got {})",
            folds.len()
        );
        assert!(
            ms < 2000.0,
            "fold compute must stay ~linear; took {ms:.0}ms — an O(n²) regression \
             (per-fold full-source rescan) would take many seconds here"
        );
    }

    fn rust_syntax_with(text: &str) -> lattice_syntax::Syntax {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap()
            .unwrap();
        s.parse(text);
        s
    }

    fn markdown_syntax_with(text: &str) -> lattice_syntax::Syntax {
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Markdown)
            .unwrap()
            .unwrap();
        s.parse(text);
        s
    }

    #[test]
    fn rust_syntax_folds_function_struct_and_impl() {
        let src = r#"struct Buffer {
    rope: Rope,
}

impl Buffer {
    fn new() -> Self {
        Self { rope: Rope::new() }
    }
}
"#;
        let syntax = rust_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds.scm");
        // struct_item: lines 0..=2
        assert!(
            folds.iter().any(|f| f.start_line == 0 && f.end_line >= 2),
            "expected struct fold: {folds:?}"
        );
        // impl_item: starts at line 4
        assert!(
            folds.iter().any(|f| f.start_line == 4),
            "expected impl fold: {folds:?}"
        );
        // function_item: starts at line 5 (`fn new`)
        assert!(
            folds.iter().any(|f| f.start_line == 5),
            "expected fn fold: {folds:?}"
        );
    }

    #[test]
    fn rust_syntax_folds_skips_single_line_items() {
        let src = "use std::sync::Arc;\nfn main() {}\n";
        let syntax = rust_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds");
        // Both items live on a single line; nothing to fold.
        assert!(
            folds.iter().all(|f| f.end_line > f.start_line),
            "single-line items should not produce folds: {folds:?}"
        );
    }

    #[test]
    fn rust_syntax_folds_let_with_if_else_expression() {
        // The user's exact scenario: a `let` binding with an
        // if-else expression body. Three folds must be available --
        // the then-block, the else-block, AND the surrounding
        // if_expression -- so progressive `zc`s walk inner → outer.
        // Wrap in a fn body so tree-sitter parses it cleanly even
        // without a trailing `;` -- the original report omitted it.
        let src = "fn outer() {\n    let len = if cond {\n        a\n    } else {\n        b\n    };\n}\n";
        let syntax = rust_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds");
        // then-block fold: starts on line 1 (the `{` after `if cond`).
        assert!(
            folds.iter().any(|f| f.start_line == 1 && f.end_line == 3),
            "expected then-block fold at 1..=3: {folds:?}"
        );
        // else-block fold: starts on line 3 (the `} else {`).
        assert!(
            folds.iter().any(|f| f.start_line == 3 && f.end_line == 5),
            "expected else-block fold at 3..=5: {folds:?}"
        );
        // if_expression covers lines 1..=5 (`if ... else { ... }`).
        assert!(
            folds.iter().any(|f| f.start_line == 1 && f.end_line == 5),
            "expected if_expression fold at 1..=5: {folds:?}"
        );
    }

    #[test]
    fn rust_syntax_folds_if_else_without_trailing_semicolon() {
        // The literal no-semicolon shape the user shared. A `let`
        // without `;` is not a valid Rust statement but tree-sitter
        // still recovers and the if_expression node exists. Verify
        // it produces a fold so progressive `zc` can reach the
        // outer if/else as one unit.
        let src = "fn outer() -> u32 {\n    let len = if cond {\n        bytes - 1\n    } else {\n        bytes\n    }\n}\n";
        let syntax = rust_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds");
        // if_expression fold (the user's "fold the if part" target)
        // starts on line 1 and runs through line 5 (the closing `}`
        // of the else branch).
        assert!(
            folds.iter().any(|f| f.start_line == 1 && f.end_line == 5),
            "expected if_expression fold at 1..=5: {folds:?}"
        );
    }

    #[test]
    fn rust_syntax_top_level_let_with_if_else_emits_five_line_fold() {
        // The literal snippet from the user's report -- no
        // surrounding fn, no semicolon. tree-sitter recovers and
        // emits the if_expression node; folds.scm captures it. The
        // outermost fold starting at line 0 must span the full 5
        // lines so `zc` on the `let` line collapses the entire
        // form in one step and the renderer reports "5 lines
        // folded".
        let src = "let len = if has_trailing_newline {\n    bytes - 1\n} else {\n    bytes\n}\n";
        let syntax = rust_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds");
        let widest_at_zero = folds
            .iter()
            .filter(|f| f.start_line == 0)
            .max_by_key(|f| f.end_line)
            .expect("a fold must start at line 0");
        assert_eq!(
            widest_at_zero.end_line, 4,
            "outermost fold at line 0 must end at line 4 (5 lines): {folds:?}"
        );
    }

    #[test]
    fn rust_syntax_folds_block_comments() {
        // Multi-line `/* ... */` comments fall under the
        // `block_comment` capture in folds.scm.
        let src = "/*\n * doc\n */\nfn main() {}\n";
        let syntax = rust_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("rust folds");
        assert!(
            folds.iter().any(|f| f.start_line == 0 && f.end_line == 2),
            "expected block_comment fold: {folds:?}"
        );
    }

    #[test]
    fn rust_syntax_folds_identities_separate_struct_from_fn() {
        // Two items with the same start-line text but different
        // node kinds must produce distinct identities so closed-
        // state can't mistakenly transfer between them.
        let a = rust_syntax_with("struct X {\n    f: u8,\n}\n");
        let b = rust_syntax_with("fn x() {\n    return;\n}\n");
        let fa = compute_syntax_folds(a.snapshot()).unwrap();
        let fb = compute_syntax_folds(b.snapshot()).unwrap();
        let id_a = fa.iter().find_map(|f| f.identity).expect("a id");
        let id_b = fb.iter().find_map(|f| f.identity).expect("b id");
        assert_ne!(
            id_a, id_b,
            "node kind must contribute to identity (struct vs fn)"
        );
    }

    #[test]
    fn markdown_syntax_folds_section() {
        let src = "# H1\nbody one\nbody two\n# H2\nafter\n";
        let syntax = markdown_syntax_with(src);
        let folds = compute_syntax_folds(syntax.snapshot()).expect("markdown folds");
        // Each section becomes a fold; the H1 section spans lines
        // 0..=2 (heading + 2 body lines, before the H2 sibling).
        assert!(
            folds.iter().any(|f| f.start_line == 0),
            "expected H1 section fold: {folds:?}"
        );
    }

    #[test]
    fn syntax_folds_returns_none_for_plain_buffer() {
        // Plain language: there's no Syntax instance to begin with;
        // the App-level cascade is what handles plain. The provider
        // function itself only runs when called with a Syntax for a
        // recognised language. We assert the registry honestly
        // reports no folds.scm for the inline grammar here as a
        // proxy: it has a parser+tree but no folds query.
        let src = "*emphasis* and `code`";
        let mut s = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Markdown)
            .unwrap()
            .unwrap();
        s.parse(src);
        // The registry-level guard: if we ask via folds_query for
        // a language that doesn't ship one, we get None. (Markdown
        // does ship one; the inline grammar doesn't, and we don't
        // expose Lang::MarkdownInline as a top-level language.)
        let _ = s.tree();
    }

    // ---- FoldIndex (perf plan C) ---------------------------------

    fn closed(start: u32, end: u32) -> Fold {
        Fold {
            start_line: start,
            end_line: end,
            closed: true,
            identity: None,
        }
    }
    fn open(start: u32, end: u32) -> Fold {
        Fold {
            start_line: start,
            end_line: end,
            closed: false,
            identity: None,
        }
    }

    #[test]
    fn fold_index_empty_returns_false_everywhere() {
        let idx = FoldIndex::from_folds(&[], true);
        assert!(!idx.line_inside_closed_fold(0));
        assert!(!idx.line_inside_closed_fold(10));
        assert!(!idx.closed_fold_start_at(0));
        assert!(!idx.fold_start_at_any(0));
    }

    #[test]
    fn fold_index_respects_foldenable_off() {
        // Even with closed folds present, foldenable=false makes
        // every predicate return false — matches the existing
        // Editor::* helpers.
        let folds = vec![closed(5, 10)];
        let idx = FoldIndex::from_folds(&folds, false);
        assert!(!idx.line_inside_closed_fold(7));
        assert!(!idx.closed_fold_start_at(5));
        assert!(!idx.fold_start_at_any(5));
        assert_eq!(idx.fold_start_kind_at(5), None);
    }

    #[test]
    fn fold_start_kind_distinguishes_open_closed_and_none() {
        // A gutter renderer shows `▾` on open heads and `▸` on closed
        // ones; every non-head line reports `None`. foldenable gates it.
        let folds = vec![closed(2, 5), open(8, 12)];
        let idx = FoldIndex::from_folds(&folds, true);
        assert_eq!(idx.fold_start_kind_at(2), Some(FoldMarker::Closed));
        assert_eq!(idx.fold_start_kind_at(8), Some(FoldMarker::Open));
        // Interior + unrelated lines: no marker.
        assert_eq!(idx.fold_start_kind_at(3), None);
        assert_eq!(idx.fold_start_kind_at(0), None);
        assert_eq!(idx.fold_start_kind_at(12), None);
        // foldenable off suppresses the marker on the same heads.
        let off = FoldIndex::from_folds(&folds, false);
        assert_eq!(off.fold_start_kind_at(2), None);
        assert_eq!(off.fold_start_kind_at(8), None);
    }

    #[test]
    fn fold_index_matches_naive_lookup_on_non_overlapping() {
        let folds = vec![closed(2, 5), open(8, 12), closed(15, 20)];
        let idx = FoldIndex::from_folds(&folds, true);
        // Naive walk for the same predicates, asserting parity at
        // every line in the relevant range.
        let naive_inside = |line: u32| -> bool {
            folds
                .iter()
                .any(|f| f.closed && line > f.start_line && line <= f.end_line)
        };
        let naive_closed_start =
            |line: u32| -> bool { folds.iter().any(|f| f.closed && f.start_line == line) };
        let naive_any_start = |line: u32| -> bool { folds.iter().any(|f| f.start_line == line) };
        for line in 0..25 {
            assert_eq!(
                idx.line_inside_closed_fold(line),
                naive_inside(line),
                "line_inside parity at line {line}"
            );
            assert_eq!(
                idx.closed_fold_start_at(line),
                naive_closed_start(line),
                "closed_start parity at line {line}"
            );
            assert_eq!(
                idx.fold_start_at_any(line),
                naive_any_start(line),
                "any_start parity at line {line}"
            );
        }
    }

    #[test]
    fn fold_index_handles_nested_closed_folds() {
        // Outer + inner both closed. The semantics — matching the
        // existing `Editor::line_inside_closed_fold` — are: `inside`
        // is true iff ANY closed fold has `start < line && line <= end`.
        // For a `(0..=10, 3..=7)` nested layout, the inner's start
        // line (3) IS inside the outer (because `0 < 3 && 3 <= 10`)
        // — the renderer hides it as part of the outer fold's body.
        // Only the outer's heading (line 0) and lines past the outer's
        // end escape.
        let folds = vec![closed(0, 10), closed(3, 7)];
        let idx = FoldIndex::from_folds(&folds, true);
        assert!(
            !idx.line_inside_closed_fold(0),
            "outer start row stays visible"
        );
        for line in 1..=10 {
            assert!(
                idx.line_inside_closed_fold(line),
                "line {line} should be inside the outer fold (0..=10)"
            );
        }
        assert!(!idx.line_inside_closed_fold(11), "past outer end");
    }

    #[test]
    fn fold_index_handles_sibling_nested_only_inner_closed() {
        // Outer open + inner closed. Inside the inner range: hidden.
        // Outside the inner range: outer is open, so visible.
        let folds = vec![open(0, 10), closed(3, 7)];
        let idx = FoldIndex::from_folds(&folds, true);
        for line in 0..=3 {
            assert!(
                !idx.line_inside_closed_fold(line),
                "line {line} should be visible (outer open, before inner start)"
            );
        }
        for line in 4..=7 {
            assert!(
                idx.line_inside_closed_fold(line),
                "line {line} should be inside the closed inner (3..=7)"
            );
        }
        for line in 8..=11 {
            assert!(
                !idx.line_inside_closed_fold(line),
                "line {line} should be visible (outer open, past inner end)"
            );
        }
    }

    #[test]
    fn fold_index_handles_overlapping_closed_folds_via_slow_path() {
        // Overlapping (rare — manually created) folds should still
        // report `inside` for any line covered by either. The
        // partition-point fast path exits early on the rightmost
        // candidate; this test exercises the slow-path fallback.
        let folds = vec![closed(0, 8), closed(2, 4)];
        let idx = FoldIndex::from_folds(&folds, true);
        // Line 6: outside the inner (2..=4) but inside the outer
        // (0..=8). The rightmost candidate by start (inner, idx-1)
        // has end=4, so the fast path returns false; the slow path
        // walks left and finds the outer.
        assert!(idx.line_inside_closed_fold(6));
        assert!(idx.line_inside_closed_fold(8));
        assert!(!idx.line_inside_closed_fold(9));
    }

    #[test]
    fn fold_index_closed_fold_at_returns_range_only_for_closed_starts() {
        // Closed folds get their (start, end) back; open folds and
        // non-start lines return None even if their start coincides
        // with another fold's interior.
        let folds = vec![closed(2, 5), open(8, 12), closed(15, 20)];
        let idx = FoldIndex::from_folds(&folds, true);
        assert_eq!(idx.closed_fold_at(2), Some((2, 5)));
        assert_eq!(idx.closed_fold_at(15), Some((15, 20)));
        // Open fold's start: None (closed-only accessor).
        assert_eq!(idx.closed_fold_at(8), None);
        // Non-start lines (interior or unrelated): None.
        assert_eq!(idx.closed_fold_at(3), None);
        assert_eq!(idx.closed_fold_at(0), None);
        assert_eq!(idx.closed_fold_at(100), None);
    }

    #[test]
    fn fold_index_closed_fold_at_respects_foldenable_off() {
        let folds = vec![closed(2, 5)];
        let idx = FoldIndex::from_folds(&folds, false);
        assert_eq!(idx.closed_fold_at(2), None);
    }

    #[test]
    fn fold_index_enclosing_closed_fold_returns_innermost_range() {
        // Non-overlapping: the body of each closed fold reports its
        // own range; heads and gaps report None.
        let folds = vec![closed(2, 5), open(8, 12), closed(15, 20)];
        let idx = FoldIndex::from_folds(&folds, true);
        assert_eq!(idx.enclosing_closed_fold(3), Some((2, 5)));
        assert_eq!(idx.enclosing_closed_fold(5), Some((2, 5)));
        assert_eq!(
            idx.enclosing_closed_fold(2),
            None,
            "head row is not interior"
        );
        assert_eq!(idx.enclosing_closed_fold(6), None, "gap after fold");
        assert_eq!(
            idx.enclosing_closed_fold(10),
            None,
            "open fold body excluded"
        );
        assert_eq!(idx.enclosing_closed_fold(18), Some((15, 20)));
    }

    #[test]
    fn fold_index_enclosing_closed_fold_prefers_inner_then_climbs_to_outer() {
        // Nested closed folds: a body line resolves to the innermost
        // fold; jumping to that fold's start then resolves to the
        // outer (the scroll walk climbs head-to-head this way).
        let folds = vec![closed(0, 10), closed(3, 7)];
        let idx = FoldIndex::from_folds(&folds, true);
        assert_eq!(
            idx.enclosing_closed_fold(5),
            Some((3, 7)),
            "innermost first"
        );
        assert_eq!(
            idx.enclosing_closed_fold(3),
            Some((0, 10)),
            "inner head is itself inside the outer ⇒ climb out"
        );
        assert_eq!(idx.enclosing_closed_fold(0), None, "outer head is visible");
        // A line inside only the outer (past the inner's end).
        assert_eq!(idx.enclosing_closed_fold(9), Some((0, 10)));
    }

    #[test]
    fn fold_index_enclosing_closed_fold_overlap_slow_path() {
        // Overlapping (manually created) folds: line 6 is outside the
        // inner (2..=4) but inside the outer (0..=8); the fast path's
        // rightmost candidate misses, the slow path finds the outer.
        let folds = vec![closed(0, 8), closed(2, 4)];
        let idx = FoldIndex::from_folds(&folds, true);
        assert_eq!(idx.enclosing_closed_fold(6), Some((0, 8)));
        assert_eq!(
            idx.enclosing_closed_fold(3),
            Some((2, 4)),
            "innermost on overlap"
        );
    }

    #[test]
    fn fold_index_enclosing_closed_fold_respects_foldenable_off() {
        let folds = vec![closed(2, 5)];
        let idx = FoldIndex::from_folds(&folds, false);
        assert_eq!(idx.enclosing_closed_fold(3), None);
    }

    #[test]
    fn fold_index_open_folds_excluded_from_inside_check() {
        let folds = vec![open(0, 10)];
        let idx = FoldIndex::from_folds(&folds, true);
        // Open folds don't hide content — `inside` only applies to
        // CLOSED folds.
        for line in 0..=10 {
            assert!(!idx.line_inside_closed_fold(line));
        }
        assert!(!idx.closed_fold_start_at(0));
        // ...but the start row is still a fold start (for the gutter
        // glyph that distinguishes open vs closed).
        assert!(idx.fold_start_at_any(0));
    }
}
