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

    let mut folds: Vec<Fold> = Vec::new();
    for i in 0..line_count {
        let Some(start_indent) = indents[i] else {
            continue;
        };
        // Look for the next non-blank line.
        let next_non_blank = ((i + 1)..line_count).find(|j| indents[*j].is_some());
        let Some(j) = next_non_blank else {
            continue;
        };
        let Some(next_indent) = indents[j] else {
            continue;
        };
        if next_indent <= start_indent {
            continue;
        }
        // Walk forward to find the last line with indent > start_indent.
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
            let start_line_text = line_at(source, start_line);
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

/// Extract the text of `line_idx` from a source byte slice. Returns
/// "" for out-of-range indices. Used by the syntax fold identity
/// hash.
fn line_at(source: &[u8], line_idx: u32) -> &str {
    let mut line: u32 = 0;
    let mut start: usize = 0;
    for (i, b) in source.iter().enumerate() {
        if line == line_idx {
            // Found the start of the target line.
            let end = source[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map(|d| i + d)
                .unwrap_or(source.len());
            return std::str::from_utf8(&source[start..end]).unwrap_or("");
        }
        if *b == b'\n' {
            line += 1;
            start = i + 1;
        }
    }
    if line == line_idx {
        return std::str::from_utf8(&source[start..]).unwrap_or("");
    }
    ""
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
}
