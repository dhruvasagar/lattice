//! The text-reflow engine — re-break a range of lines to `textwidth`.
//!
//! Design: `docs/dev/architecture/text-reflow.md` §4.
//! Sequencing: `docs/dev/operations/slice-plans/text-reflow.md` (this is
//! RF.1).
//!
//! A pure function of `(lines, textwidth, comment leader, indent)`. No
//! I/O, no syntax tree, no config lookup — the host resolves the options
//! and hands the values down, exactly as it does for the indent unit.
//!
//! ## Why it lives in `lattice-grammar`
//!
//! Heuristic #6: it carves out no dependency surface, so it earns no
//! crate. The operator that drives it ([`crate::builtins`]) reads
//! [`crate::GrammarEnv`], which already carries `comment_syntax` (N.1.6)
//! and `indent` (IN.0); the host's insert path (RF.3) calls the same
//! module and already depends on this crate.
//!
//! `lattice-format` was the other candidate and is the wrong one: that
//! crate is process spawning, timeouts and diff-derived edits — a
//! different mechanism that happens to share the word "format".
//!
//! ## What it is not
//!
//! Not a reformatter. It moves line breaks and normalises interior
//! whitespace within a paragraph, and touches nothing else — no
//! reindentation of code, no reordering, no syntax awareness.

use unicode_width::UnicodeWidthStr;

/// Display width of `s` in terminal columns.
///
/// Columns, not bytes and not chars: bytes mis-measure every non-ASCII
/// line, and chars mis-measure CJK (2 columns) and combining marks (0).
/// The renderer already measures this way, so a reflowed line lands
/// where the user was told it would.
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// The fixed prefix every line of a paragraph carries: indentation plus
/// any comment leader, e.g. `"    /// "` or `"# "` or just `"  "`.
///
/// [`Self::first`] and [`Self::rest`] differ only for a list item, whose
/// continuation lines align to the text column rather than repeating the
/// marker (§4.3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Prefix {
    /// What the paragraph's first output line starts with.
    pub first: String,
    /// What every subsequent output line starts with.
    pub rest: String,
}

impl Prefix {
    fn uniform(p: String) -> Self {
        Prefix {
            first: p.clone(),
            rest: p,
        }
    }
}

/// One paragraph found inside a reflow range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    /// Index of the paragraph's first line, relative to the range start.
    pub start: usize,
    /// One past the paragraph's last line, relative to the range start.
    pub end: usize,
    /// The prefix its output lines carry.
    pub prefix: Prefix,
    /// Whether this run is fillable at all. Blank lines and fenced code
    /// are carried through verbatim; the whole paragraph model stays one
    /// list so a consumer cannot forget to re-emit the gaps.
    pub fillable: bool,
}

/// Everything the engine needs that it cannot derive from the text.
#[derive(Debug, Clone, Copy)]
pub struct ReflowConfig<'a> {
    /// Target column. Always a real column — `autowrap=off` is what
    /// turns wrapping off, so this never carries a "disabled" sentinel.
    pub textwidth: usize,
    /// The language's line-comment leader (`//`, `#`, `--`), if it has
    /// one. `None` for markdown, plain text and any language whose
    /// comment syntax is undeclared — reflow then treats indentation
    /// alone as the prefix, which is the right answer for prose.
    pub line_comment: Option<&'a str>,
}

/// Split `lines` into paragraphs (§4.1).
///
/// A new paragraph begins at a blank line, a leader-only line, a change
/// of prefix, or a change of indent. Blank and leader-only runs come
/// back as `fillable: false` so they survive the round trip verbatim —
/// carrying them in the same list as the fillable runs is what makes it
/// impossible to drop them by forgetting a branch.
pub fn paragraphs(lines: &[&str], cfg: ReflowConfig<'_>) -> Vec<Paragraph> {
    let mut out: Vec<Paragraph> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let start = i;
        let sep = is_separator(lines[i], cfg);
        if sep {
            // A run of separators is carried through untouched.
            while i < lines.len() && is_separator(lines[i], cfg) {
                i += 1;
            }
            out.push(Paragraph {
                start,
                end: i,
                prefix: Prefix::default(),
                fillable: false,
            });
            continue;
        }
        // A fillable run: extend while the next line is not a separator
        // and agrees about its prefix.
        let head = line_prefix(lines[i], cfg);
        i += 1;
        while i < lines.len()
            && !is_separator(lines[i], cfg)
            && continues_paragraph(&head, lines[i], cfg)
        {
            i += 1;
        }
        out.push(Paragraph {
            start,
            end: i,
            prefix: paragraph_prefix(&lines[start..i], cfg),
            fillable: true,
        });
    }
    out
}

/// Reflow one already-identified paragraph into output lines.
///
/// Greedy fill: words are appended while the result still fits. A word
/// that cannot fit on a line of its own **overflows rather than being
/// split** — vim, Emacs `fill-paragraph` and Rewrap all agree, and it is
/// what keeps a long URL in a comment intact.
pub fn fill(lines: &[&str], prefix: &Prefix, textwidth: usize) -> Vec<String> {
    let mut words: Vec<&str> = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let body = strip_known_prefix(line, if n == 0 { &prefix.first } else { &prefix.rest });
        words.extend(body.split_whitespace());
    }
    if words.is_empty() {
        // A paragraph of nothing but prefix: emit it back unchanged
        // rather than an empty line, which would delete the leader.
        return lines.iter().map(|l| l.trim_end().to_string()).collect();
    }

    let mut out: Vec<String> = Vec::new();
    let mut cur = prefix.first.clone();
    let mut cur_has_word = false;
    for w in words {
        if !cur_has_word {
            cur.push_str(w);
            cur_has_word = true;
            continue;
        }
        // `+ 1` for the space that would join them.
        if display_width(&cur) + 1 + display_width(w) <= textwidth {
            cur.push(' ');
            cur.push_str(w);
        } else {
            out.push(cur);
            cur = prefix.rest.clone();
            cur.push_str(w);
        }
    }
    out.push(cur);
    out
}

/// Reflow a whole range: paragraphs found, fillable ones filled,
/// everything else carried through.
///
/// Returns the replacement lines for the range. A range that reflows to
/// itself returns an equal `Vec`, which is what lets the operator skip
/// the edit entirely (RF.2) rather than pushing a no-op undo step.
pub fn reflow_range(lines: &[&str], cfg: ReflowConfig<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for p in paragraphs(lines, cfg) {
        if !p.fillable {
            out.extend(lines[p.start..p.end].iter().map(|l| l.to_string()));
            continue;
        }
        // §10: a prefix at or past the margin leaves no room for even
        // one word, and greedy filling would emit one word per line
        // forever. Leaving the paragraph alone is the honest answer.
        if display_width(&p.prefix.rest) >= cfg.textwidth {
            out.extend(lines[p.start..p.end].iter().map(|l| l.to_string()));
            continue;
        }
        out.extend(fill(&lines[p.start..p.end], &p.prefix, cfg.textwidth));
    }
    out
}

/// Where auto-wrap should break the line being typed on, and what the
/// carried remainder needs in front of it.
///
/// Byte offsets into the line, not the buffer — the host adds the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoWrapBreak {
    /// Start of the whitespace run being replaced by the newline.
    pub start: usize,
    /// End of that run — the first byte of the word moving down.
    pub end: usize,
    /// Text to splice in: a newline plus the continuation prefix.
    pub replacement: String,
}

/// Decide whether the line the cursor sits on should break, and where
/// (§9).
///
/// Called on the **keystroke path**, once per inserted character, so it
/// is a single scan of the current line and nothing else. No tree, no
/// buffer walk, no allocation beyond the replacement string on the rare
/// frame that actually breaks.
///
/// Returns `None` — leave the line alone — when:
///
/// - the cursor has not passed `textwidth` yet;
/// - there is no whitespace to break at after the prefix, i.e. the
///   overlong thing is one word. Vim, Emacs and Rewrap all agree that a
///   long URL overflows rather than being split;
/// - the only break points are past the margin, so breaking would not
///   help.
pub fn auto_wrap_break(
    line: &str,
    cursor_byte: usize,
    cfg: ReflowConfig<'_>,
) -> Option<AutoWrapBreak> {
    let cursor_byte = cursor_byte.min(line.len());
    if display_width(&line[..cursor_byte]) <= cfg.textwidth {
        return None;
    }
    // Everything up to and including the comment marker is structure and
    // is never a break point — breaking inside `///` would produce `//`
    // and a stray `/`.
    let indent = indent_of(line);
    let marker = marker_of(&line[indent.len()..], cfg.line_comment);
    let head_len = indent.len() + marker.len();

    // The continuation the carried words land after. Same rule the
    // operator uses, so a line broken by typing and the same line broken
    // by `gq` agree.
    let continuation = paragraph_prefix(&[line], cfg).rest;

    // Candidate break points: the start of each whitespace run that has
    // real content before it on this line. Scanning forward and keeping
    // the LAST one that still fits is the greedy fill, one line at a
    // time.
    let mut best: Option<(usize, usize)> = None;
    let mut seen_word = false;
    let mut i = head_len;
    let bytes = line.as_bytes();
    while i < cursor_byte {
        let c = bytes[i];
        if c == b' ' || c == b'\t' {
            if seen_word {
                let run_start = i;
                let mut j = i;
                while j < line.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if display_width(&line[..run_start]) <= cfg.textwidth {
                    best = Some((run_start, j));
                } else {
                    // Past the margin already; later runs are worse.
                    break;
                }
                i = j;
                continue;
            }
        } else {
            seen_word = true;
        }
        i += 1;
    }

    let (start, end) = best?;
    Some(AutoWrapBreak {
        start,
        end,
        replacement: format!("\n{continuation}"),
    })
}

/// Whether `line` reads as a comment, by its leading marker alone.
///
/// **Lexical on purpose** (§9). A tree-sitter query would also know that
/// a `//` inside a string literal is not a comment, and would put a
/// parse on the typing path — which paramount #1 does not allow for
/// accuracy that costs a frame. The inaccuracy is a comment marker
/// inside a string, which is rare, and its consequence is one wrapped
/// line the user can undo.
pub fn line_is_comment(line: &str, cfg: ReflowConfig<'_>) -> bool {
    let indent = indent_of(line);
    !marker_of(&line[indent.len()..], cfg.line_comment).is_empty()
}

// ---- prefix analysis (§4.2) ----

/// The leading whitespace of `line`.
fn indent_of(line: &str) -> &str {
    let end = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    &line[..end]
}

/// The comment-marker run at the start of `line`'s content, if any.
///
/// **Read from the line, not from the language table.** `CommentSyntax`
/// gives `//` for Rust, but Rust comments are written `///` and `//!`;
/// reflowing a `//!` block with `//` as the leader would rewrite
/// continuation lines as `// text`, silently turning an inner doc
/// comment into an outer one.
///
/// So the marker is the longest prefix built from the leader's own
/// characters — `///`, `//!` (`!` is admitted because `/` and `!` are
/// both leader-ish for the doc forms every C-family language uses), `##`
/// for `#` languages, `---` for Lua. A block whose lines disagree
/// degrades through [`paragraph_prefix`]'s longest-common-prefix rule.
fn marker_of<'a>(content: &'a str, leader: Option<&str>) -> &'a str {
    let Some(leader) = leader else { return "" };
    if !content.starts_with(leader) {
        return "";
    }
    let first = leader.chars().next().unwrap_or('\0');
    let end = content
        .find(|c: char| c != first && c != '!')
        .unwrap_or(content.len());
    &content[..end]
}

/// `indent + marker` for one line, without the space that follows.
fn line_prefix(line: &str, cfg: ReflowConfig<'_>) -> String {
    let indent = indent_of(line);
    let marker = marker_of(&line[indent.len()..], cfg.line_comment);
    format!("{indent}{marker}")
}

/// A line is a paragraph separator when it is blank, or when it is a
/// comment leader with no text after it.
///
/// The second case is what makes a multi-paragraph doc comment survive
/// `gqaC`: a bare `///` between two prose runs is the comment
/// equivalent of a blank line, and treating it as content would weld the
/// paragraphs together.
fn is_separator(line: &str, cfg: ReflowConfig<'_>) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return true;
    }
    let marker = marker_of(t, cfg.line_comment);
    !marker.is_empty() && t[marker.len()..].trim().is_empty()
}

/// Whether `line` continues a paragraph whose first line had prefix
/// `head`.
///
/// Requires the same indent-plus-marker. A change of either starts a new
/// paragraph — a `///` line after a `//` line is a different comment, and
/// a differently-indented line is a different block.
fn continues_paragraph(head: &str, line: &str, cfg: ReflowConfig<'_>) -> bool {
    line_prefix(line, cfg) == head
}

/// The prefix a paragraph's output lines carry.
///
/// The **longest common prefix** of its lines' `indent + marker`, which
/// is the rule that makes `///` and `//!` blocks come back as
/// themselves, handles `#` / `##` with no special case, and degrades a
/// mixed block to the shared part rather than to a guess.
///
/// A single trailing space is appended when the marker is non-empty, so
/// `///` becomes `/// ` and text does not weld to the slashes.
fn paragraph_prefix(lines: &[&str], cfg: ReflowConfig<'_>) -> Prefix {
    let mut common: Option<String> = None;
    for l in lines {
        let p = line_prefix(l, cfg);
        common = Some(match common {
            None => p,
            Some(c) => longest_common_prefix(&c, &p).to_string(),
        });
    }
    let mut base = common.unwrap_or_default();
    let indent = lines.first().map(|l| indent_of(l)).unwrap_or("");
    if base.len() > indent.len() {
        // There is a marker; separate it from the text.
        base.push(' ');
    }
    Prefix::uniform(base)
}

fn longest_common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let n = a
        .char_indices()
        .zip(b.chars())
        .take_while(|((_, ca), cb)| ca == cb)
        .map(|((i, ca), _)| i + ca.len_utf8())
        .last()
        .unwrap_or(0);
    &a[..n]
}

/// Strip `prefix` from `line` when present, else strip whatever leading
/// whitespace and marker the line does have.
///
/// The fallback matters for the degraded case: a `//` paragraph prefix
/// computed over a block containing one `///` line must not leave the
/// third slash in the body text.
fn strip_known_prefix<'a>(line: &'a str, prefix: &str) -> &'a str {
    if let Some(rest) = line.strip_prefix(prefix) {
        return rest;
    }
    let t = line.trim_start();
    let p = prefix.trim();
    if !p.is_empty()
        && let Some(rest) = t.strip_prefix(p)
    {
        return rest;
    }
    t
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cfg(textwidth: usize, leader: Option<&str>) -> ReflowConfig<'_> {
        ReflowConfig {
            textwidth,
            line_comment: leader,
        }
    }

    fn reflow(lines: &[&str], width: usize, leader: Option<&str>) -> Vec<String> {
        reflow_range(lines, cfg(width, leader))
    }

    #[test]
    fn prose_fills_greedily_to_the_margin() {
        let out = reflow(&["aaa bbb ccc ddd eee fff"], 11, None);
        assert_eq!(out, vec!["aaa bbb ccc", "ddd eee fff"]);
        for l in &out {
            assert!(display_width(l) <= 11, "{l:?} overflows");
        }
    }

    #[test]
    fn short_input_is_joined_not_only_split() {
        let out = reflow(&["aaa", "bbb", "ccc"], 20, None);
        assert_eq!(out, vec!["aaa bbb ccc"]);
    }

    #[test]
    fn a_blank_line_separates_and_survives() {
        let out = reflow(&["aaa bbb", "", "ccc ddd"], 20, None);
        assert_eq!(out, vec!["aaa bbb", "", "ccc ddd"]);
    }

    #[test]
    fn indentation_is_preserved_on_every_output_line() {
        let out = reflow(&["    aaa bbb ccc ddd"], 12, None);
        assert_eq!(out, vec!["    aaa bbb", "    ccc ddd"]);
    }

    /// The case the longest-common-prefix rule exists for. Deriving the
    /// leader from `CommentSyntax` (`//`) would rewrite these as `//`
    /// lines and silently turn an inner doc comment into an outer one.
    #[test]
    fn inner_and_outer_doc_comments_keep_their_own_marker() {
        let inner = reflow(&["//! aaa bbb ccc ddd"], 12, Some("//"));
        assert_eq!(inner, vec!["//! aaa bbb", "//! ccc ddd"]);

        let outer = reflow(&["/// aaa bbb ccc ddd"], 12, Some("//"));
        assert_eq!(outer, vec!["/// aaa bbb", "/// ccc ddd"]);

        let plain = reflow(&["// aaa bbb ccc ddd"], 11, Some("//"));
        assert_eq!(plain, vec!["// aaa bbb", "// ccc ddd"]);
    }

    #[test]
    fn hash_languages_keep_their_marker_run() {
        assert_eq!(
            reflow(&["## aaa bbb ccc ddd"], 11, Some("#")),
            vec!["## aaa bbb", "## ccc ddd"]
        );
        assert_eq!(
            reflow(&["# aaa bbb ccc ddd"], 10, Some("#")),
            vec!["# aaa bbb", "# ccc ddd"]
        );
    }

    /// A bare `///` between two prose runs is the comment equivalent of
    /// a blank line. Treating it as content welds the paragraphs, which
    /// is what `gqaC` on a real doc comment would do wrong.
    #[test]
    fn a_leader_only_line_separates_paragraphs_inside_a_comment() {
        let out = reflow(
            &["/// aaa bbb ccc", "///", "/// ddd eee fff"],
            11,
            Some("//"),
        );
        assert_eq!(
            out,
            vec!["/// aaa bbb", "/// ccc", "///", "/// ddd eee", "/// fff"]
        );
    }

    #[test]
    fn a_change_of_marker_starts_a_new_paragraph() {
        let out = reflow(&["/// aaa", "// bbb"], 40, Some("//"));
        assert_eq!(out, vec!["/// aaa", "// bbb"]);
    }

    #[test]
    fn a_change_of_indent_starts_a_new_paragraph() {
        let out = reflow(&["aaa", "    bbb"], 40, None);
        assert_eq!(out, vec!["aaa", "    bbb"]);
    }

    /// Never hard-split a word. A long URL in a comment stays one token
    /// and overflows, which every editor in the field agrees on.
    #[test]
    fn a_word_longer_than_the_margin_overflows_rather_than_splitting() {
        let long = "https://example.com/a/very/long/path/that/exceeds/the/margin";
        let out = reflow(&[&format!("aa {long} bb")], 20, None);
        assert_eq!(out, vec!["aa", long, "bb"]);
        assert!(out.iter().any(|l| display_width(l) > 20));
    }

    /// Columns, not chars: a CJK glyph is two columns wide, so a line of
    /// them fits half as many.
    #[test]
    fn width_is_measured_in_columns_not_characters() {
        assert_eq!(display_width("日本語"), 6);
        let out = reflow(&["日本 語学 練習"], 5, None);
        assert_eq!(out, vec!["日本", "語学", "練習"]);
    }

    /// §10: no break point can exist, so greedy filling would emit one
    /// word per line indefinitely. Leaving it alone beats mangling it.
    #[test]
    fn a_prefix_wider_than_the_margin_leaves_the_paragraph_untouched() {
        let input = ["        //// aaa bbb ccc"];
        let out = reflow(&input, 4, Some("//"));
        assert_eq!(out, vec![input[0].to_string()]);
    }

    #[test]
    fn an_empty_range_is_an_empty_result() {
        assert!(reflow(&[], 80, None).is_empty());
    }

    #[test]
    fn a_paragraph_that_already_fits_comes_back_unchanged() {
        let input = ["aaa bbb ccc"];
        assert_eq!(reflow(&input, 80, None), vec!["aaa bbb ccc".to_string()]);
    }

    /// Trailing whitespace goes and interior runs collapse — the
    /// normalisation vim's `gq` also does. Asserted because it is the
    /// difference between "reflow is a no-op here" and "reflow made an
    /// invisible edit".
    #[test]
    fn interior_and_trailing_whitespace_are_normalised() {
        assert_eq!(
            reflow(&["aaa   bbb  ", "ccc"], 40, None),
            vec!["aaa bbb ccc"]
        );
    }

    #[test]
    fn a_line_of_only_a_leader_is_not_emptied() {
        assert_eq!(reflow(&["///"], 40, Some("//")), vec!["///"]);
        assert_eq!(reflow(&["   "], 40, None), vec!["   "]);
    }

    /// Reflow moves line breaks and nothing else. A `//` inside a string
    /// literal is not a comment, but the engine has no tree and cannot
    /// know that — so this pins the blast radius: the operator only ever
    /// sees the range the user selected (RF.2), and within it the worst
    /// case is a prefix guessed from a lexical marker.
    #[test]
    fn only_line_breaks_and_interior_spacing_change() {
        let out = reflow(&["let x = 1; let y = 2;"], 12, None);
        assert_eq!(out, vec!["let x = 1;", "let y = 2;"]);
        // Every word survives, in order.
        let words: Vec<&str> = out.iter().flat_map(|l| l.split_whitespace()).collect();
        assert_eq!(words, vec!["let", "x", "=", "1;", "let", "y", "=", "2;"]);
    }

    // ---- RF.3: the auto-wrap break point ----

    fn brk(line: &str, width: usize, leader: Option<&str>) -> Option<AutoWrapBreak> {
        auto_wrap_break(line, line.len(), cfg(width, leader))
    }

    /// The break replaces the whitespace run, so the space does not
    /// become trailing whitespace on the line above.
    #[test]
    fn auto_wrap_breaks_at_the_last_space_that_fits() {
        let b = brk("aaa bbb ccc", 7, None).expect("must break");
        assert_eq!(&"aaa bbb ccc"[b.start..b.end], " ");
        assert_eq!(b.start, 7, "the space after `bbb` is the last that fits");
        assert_eq!(b.replacement, "\n");
    }

    #[test]
    fn no_break_until_the_cursor_passes_the_margin() {
        assert!(brk("aaa bbb", 80, None).is_none());
        assert!(brk("", 80, None).is_none());
    }

    /// One long word overflows rather than being split — the same rule
    /// the operator follows, so typing and `gq` cannot disagree.
    #[test]
    fn a_single_long_word_does_not_break() {
        assert!(brk("aaaaaaaaaaaaaaaaaaaa", 5, None).is_none());
        // …and neither does a leader followed by one long word.
        assert!(brk("// aaaaaaaaaaaaaaaaaaaa", 5, Some("//")).is_none());
    }

    /// The carried remainder gets the comment leader, or a doc comment
    /// silently turns into code on the next line.
    #[test]
    fn the_continuation_carries_the_comment_leader() {
        let line = "/// aaa bbb ccc";
        let b = brk(line, 11, Some("//")).expect("must break");
        assert_eq!(b.replacement, "\n/// ");
        assert_eq!(&line[b.start..b.end], " ");
    }

    #[test]
    fn the_continuation_carries_indentation() {
        let line = "    aaa bbb ccc";
        let b = brk(line, 11, None).expect("must break");
        assert_eq!(b.replacement, "\n    ");
    }

    /// Never break inside the marker itself — `///` split across a
    /// newline would leave `//` and a stray `/`.
    #[test]
    fn the_marker_is_never_a_break_point() {
        let line = "///aaa bbb";
        let b = brk(line, 6, Some("//")).expect("must break");
        assert!(
            b.start >= 3,
            "break at {} is inside the `///` marker",
            b.start
        );
    }

    #[test]
    fn line_is_comment_reads_the_leading_marker_only() {
        let c = cfg(80, Some("//"));
        assert!(line_is_comment("  // hi", c));
        assert!(line_is_comment("/// hi", c));
        assert!(!line_is_comment("let x = 1; // hi", c));
        assert!(!line_is_comment("plain", c));
        // No leader declared (markdown, plain text): nothing is a comment.
        assert!(!line_is_comment("// hi", cfg(80, None)));
    }

    #[test]
    fn paragraphs_reports_fillable_and_verbatim_runs_in_order() {
        let lines = ["aaa", "", "bbb"];
        let ps = paragraphs(&lines, cfg(40, None));
        assert_eq!(ps.len(), 3);
        assert!(ps[0].fillable && ps[0].start == 0 && ps[0].end == 1);
        assert!(!ps[1].fillable, "the blank run is carried, not filled");
        assert!(ps[2].fillable && ps[2].start == 2 && ps[2].end == 3);
    }
}
