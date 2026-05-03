//! Computed folds (DESIGN.md §5.1, §15:18; user-facing reference
//! at `docs/help/folding.md`).
//!
//! Folds derived automatically from the buffer's structure. v1
//! providers:
//!
//! 1. **Indent** -- fold spans any line whose successor indents
//!    deeper, ending at the last line whose indent is strictly
//!    greater than the start. Universal across languages.
//! 2. **Markdown** -- `^#+ ` headings define the fold tree.
//!    A `# H1` folds until the next `# H1`; a `## H2` folds until
//!    the next same-or-higher heading. Triggers automatically on
//!    `*.md` buffers when `foldmethod = syntax` cascades.
//! 3. **Syntax (cascade)** -- tree-sitter scope queries are queued
//!    as a follow-up; until that lands, the `Syntax` foldmethod
//!    cascades to `Markdown` for `.md` buffers and `Indent`
//!    otherwise.
//!
//! Manual folds (created via `zf` from a Visual selection) and
//! computed folds coexist in [`crate::app::App::folds`] with no
//! distinction at the storage layer; the `:set foldmethod` option
//! decides which side feeds in.
//!
//! Fold identity (`Fold::identity`) is the SHA-style hash of the
//! trimmed start-line text plus indent depth. When the buffer
//! changes and folds recompute, we match new folds to old ones by
//! identity and transfer the closed-state -- so adding a line to
//! one section doesn't reopen the closed section above. Manual
//! folds carry `identity = None` (their stable identity is the
//! line range itself).

use std::hash::{DefaultHasher, Hash, Hasher};

use lattice_core::Buffer;

use crate::app::Fold;

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
    indents.iter().enumerate().skip(from).find_map(|(i, ind)| {
        if ind.is_some() { Some(i) } else { None }
    })
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
/// `docs/help/folding.md`). Walks the buffer for ATX headings
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
        let src = "fn outer() {\n    let x = 1;\n    if x > 0 {\n        println!(\"yes\");\n    }\n}\n";
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
}
