//! Syntax highlighting shared across magit's synthetic buffers.
//!
//! `magit-status` already gets diff coloring (`diff_styled_spans`,
//! moved here from `actions.rs`) and section-header/entry styling
//! (`SectionIndex::format_buffer_styled`, `sections.rs`). Every other
//! magit view populated plain, unstyled text — this module extends
//! the same `PendingSyntheticHighlights` pipeline to log, blame,
//! branch, stash, rebase, diff, commit, and revision buffers, reusing
//! `sections.rs`'s established palette (SHA → `Style::Link`, commit
//! subject/message → `Style::Comment`, `stash@{` → `Style::Keyword` +
//! index → `Style::Number`) so the whole surface reads consistently.

use lattice_cells::style::{Style, StyledSpan};

/// MG.21a: what one line of a unified diff *is*. Extracted because the
/// whole-buffer styler and the commit buffer's range-scoped styler each
/// carried their own copy of this prefix ladder, so a rule fixed in one
/// could silently miss the other. One ladder, two callers.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum DiffLineClass {
    /// `+content` — an added line (but not the `+++` file header).
    Added,
    /// `-content` — a removed line (but not the `---` file header).
    Removed,
    /// `@@ … @@` hunk header.
    Hunk,
    /// `diff --git a/… b/…`.
    FileCommand,
    /// `---` / `+++` file path headers.
    FilePath,
    /// Anything else: context lines, `index …`, mode changes, blanks.
    Context,
}

impl DiffLineClass {
    /// The foreground style for this class, or `None` for unstyled.
    fn style(self) -> Option<Style> {
        match self {
            Self::Added => Some(Style::DiffAdd),
            Self::Removed => Some(Style::DiffRemove),
            Self::Hunk => Some(Style::Comment),
            Self::FileCommand => Some(Style::Keyword),
            Self::FilePath => Some(Style::Link),
            Self::Context => None,
        }
    }

}

/// Classify one line of a unified diff. The single prefix ladder.
pub(crate) fn classify_diff_line(line: &str) -> DiffLineClass {
    if line.starts_with('+') && !line.starts_with("+++") {
        DiffLineClass::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        DiffLineClass::Removed
    } else if line.starts_with("@@") {
        DiffLineClass::Hunk
    } else if line.starts_with("diff --git") {
        DiffLineClass::FileCommand
    } else if line.starts_with("---") || line.starts_with("+++") {
        DiffLineClass::FilePath
    } else {
        DiffLineClass::Context
    }
}

/// The styled spans for one classified line — the whole line, or empty.
fn spans_for(class: DiffLineClass, line_len: usize) -> Vec<StyledSpan> {
    match class.style() {
        Some(style) => vec![StyledSpan {
            start: 0,
            end: line_len,
            style,
        }],
        None => Vec::new(),
    }
}


/// Color a unified diff: `+`/`-` content lines, `@@` hunk headers,
/// `diff --git`/`---`/`+++` file headers. Used verbatim by
/// `magit-status`'s inline-expanded diffs (`actions.rs`) and by
/// `magit-diff-mode`'s whole-buffer `git diff` content; `commit_buffer_styled_spans`
/// below reuses it for the staged-diff region of the commit buffer.
pub(crate) fn diff_styled_spans(diff: &str) -> Vec<Vec<StyledSpan>> {
    diff.lines()
        .map(|line| spans_for(classify_diff_line(line), line.len()))
        .collect()
}

/// Color the staged-diff region of the commit buffer
/// (`magit-commit-mode`) without misclassifying the buffer's own
/// `"--- Staged diff ... ---"` header line as a diff `---` file
/// marker (a naive whole-buffer `diff_styled_spans` call would style
/// it `Style::Link`, since it also starts with `---`). Only lines at
/// or after `diff_start_line` get diff coloring; everything before
/// (the header line) and after (the message marker + typed message)
/// stays unstyled.
pub(crate) fn commit_buffer_styled_spans(
    text: &str,
    diff_start_line: usize,
    diff_end_line: usize,
) -> Vec<Vec<StyledSpan>> {
    let mut result: Vec<Vec<StyledSpan>> = text.lines().map(|_| Vec::new()).collect();
    for (i, line) in text.lines().enumerate() {
        if i < diff_start_line || i >= diff_end_line {
            continue;
        }
        if let Some(slot) = result.get_mut(i) {
            *slot = spans_for(classify_diff_line(line), line.len());
        }
    }
    result
}

/// Find the first whitespace-delimited, hex-looking token (≥4 chars)
/// in `line` and return its byte range. Byte-index-safe (ASCII-only
/// check on the candidate token avoids any UTF-8 boundary hazard).
/// Shared by the log/blame/rebase stylers below — each buffer's SHA
/// sits at a different, otherwise-unstructured column (graph
/// characters vary in width, blame's author column is fixed-width
/// but locale-dependent), so scanning for "the first hex token" is
/// more robust than a fixed byte offset.
fn find_sha_span(line: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b' ' {
            i += 1;
        }
        if i > start {
            let tok = &line[start..i];
            if tok.len() >= 4 && tok.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some((start, i));
            }
        }
    }
    None
}

/// Find the first `(...)` span in `line` (git log's ref-decoration
/// list, e.g. `(HEAD -> main, origin/main)`).
fn find_paren_span(line: &str) -> Option<(usize, usize)> {
    let start = line.find('(')?;
    let rel_end = line[start..].find(')')?;
    Some((start, start + rel_end + 1))
}

/// Color `git log --oneline --graph --decorate` output: SHA →
/// `Style::Link` (matches `sections.rs`'s Commit-entry SHA color),
/// `(refs...)` decoration → `Style::MagitRefDecoration`, everything else
/// (graph characters, subject) unstyled — the graph's own ASCII art
/// carries enough visual structure without color, and the subject
/// reads as plain text exactly like `sections.rs`'s Commit entries
/// leave theirs (`Style::Comment` there is for the SUBJECT column
/// specifically, kept plain here since log's subject sits right after
/// the more-visually-busy graph+refs prefix — coloring it too would
/// compete with the SHA/refs highlighting for attention on the same
/// line).
pub(crate) fn log_styled_spans(text: &str) -> Vec<Vec<StyledSpan>> {
    text.lines()
        .map(|line| {
            let mut spans = Vec::new();
            if let Some((start, end)) = find_sha_span(line) {
                spans.push(StyledSpan {
                    start,
                    end,
                    style: Style::MagitSha,
                });
                if let Some((pstart, pend)) = find_paren_span(&line[end..]) {
                    spans.push(StyledSpan {
                        start: end + pstart,
                        end: end + pend,
                        style: Style::MagitRefDecoration,
                    });
                }
            }
            spans
        })
        .collect()
}

/// Color `git blame` output (as formatted by `magit_blame_mode::run_blame`
/// — `<sha> <author padded to 12>  <code>`): SHA → `Style::MagitSha`,
/// author → `Style::MagitAuthor`, code left unstyled (it's the file's own
/// content — this buffer has no language context to highlight it
/// with, and guessing one would be misleading, not helpful).
pub(crate) fn blame_styled_spans(text: &str) -> Vec<Vec<StyledSpan>> {
    text.lines()
        .map(|line| {
            // Matches `run_blame`'s `format!("{} {:>12}  ", sha, author)`
            // exactly: 8-char sha, one space, 12-char right-aligned
            // author, two spaces, then code.
            if line.len() < 8 || !line.as_bytes()[..8].iter().all(u8::is_ascii_hexdigit) {
                return Vec::new();
            }
            let mut spans = vec![StyledSpan {
                start: 0,
                end: 8,
                style: Style::MagitSha,
            }];
            let author_start = 9;
            let author_end = (author_start + 12).min(line.len());
            if author_end > author_start {
                spans.push(StyledSpan {
                    start: author_start,
                    end: author_end,
                    style: Style::MagitAuthor,
                });
            }
            spans
        })
        .collect()
}

/// Color `git branch --format=%(refname:short)`-derived branch list
/// output (`magit_branch_mode::build_branch_list`): the current
/// branch's `* ` marker + name → `Style::MagitBranchCurrent` (the same visual
/// weight `sections.rs` gives commit/stash prefixes), other branches
/// unstyled.
pub(crate) fn branch_styled_spans(text: &str) -> Vec<Vec<StyledSpan>> {
    text.lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("* ") {
                vec![StyledSpan {
                    start: 0,
                    end: 2 + rest.len(),
                    style: Style::MagitBranchCurrent,
                }]
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// Color `magit_stash_mode::build_stash_list` output — `  stash@{N}
/// <message>` per stash. MG.15 gave the list the `stash@{N}` label
/// (fixing the dead-chord bug where the index parser read a label the
/// renderer never wrote), so this styler is no longer the no-op it
/// was: it now applies the SAME split `sections.rs` gives the
/// status-buffer stash entries — `stash@{` → `Style::Keyword`, the
/// index → `Style::Number`, the message → `Style::Comment` — so a
/// stash reads identically in both places.
pub(crate) fn stash_styled_spans(text: &str) -> Vec<Vec<StyledSpan>> {
    text.lines()
        .map(|line| {
            // Byte offsets, matching `magit_stash_mode::list_row`'s
            // `"  stash@{N} message"`. `stash@{` is ASCII and the
            // index is ASCII digits, so the only non-ASCII a row can
            // carry is inside the message — after every boundary
            // computed here.
            const PREFIX: &str = "  stash@{";
            let Some(rest) = line.strip_prefix(PREFIX) else {
                return Vec::new();
            };
            let Some(close) = rest.find('}') else {
                return Vec::new();
            };
            let idx_start = PREFIX.len();
            let idx_end = idx_start + close;
            let mut spans = vec![
                StyledSpan {
                    start: 2,
                    end: idx_start,
                    style: Style::Keyword,
                },
                StyledSpan {
                    start: idx_start,
                    end: idx_end,
                    style: Style::Number,
                },
            ];
            // `}` then a space, then the message — absent for a stash
            // with an empty subject, which git allows.
            let message_start = idx_end + 2;
            if message_start < line.len() {
                spans.push(StyledSpan {
                    start: message_start,
                    end: line.len(),
                    style: Style::Comment,
                });
            }
            spans
        })
        .collect()
}

/// Color the rebase todo buffer (`magit_rebase_mode`): the verb
/// (`pick`/`reword`/`edit`/`squash`/`fixup`/`drop`) → `Style::MagitRebaseVerb`,
/// SHA → `Style::MagitSha`, subject unstyled (matches log's treatment).
/// Comment lines (git leaves instructional `#` lines at the end of a
/// real `git rebase -i` todo — this buffer's own
/// `build_rebase_buffer` doesn't emit any today, but the styler
/// stays defensive since it's user-EDITABLE content) get
/// `Style::Comment`.
pub(crate) fn rebase_styled_spans(text: &str) -> Vec<Vec<StyledSpan>> {
    const VERBS: [&str; 6] = ["pick", "reword", "edit", "squash", "fixup", "drop"];
    text.lines()
        .map(|line| {
            if line.starts_with('#') {
                return vec![StyledSpan {
                    start: 0,
                    end: line.len(),
                    style: Style::Comment,
                }];
            }
            let Some(verb) = VERBS.iter().find(|v| {
                line.strip_prefix(**v)
                    .is_some_and(|rest| rest.starts_with(' '))
            }) else {
                return Vec::new();
            };
            let mut spans = vec![StyledSpan {
                start: 0,
                end: verb.len(),
                style: Style::MagitRebaseVerb,
            }];
            if let Some((start, end)) = find_sha_span(&line[verb.len()..]) {
                spans.push(StyledSpan {
                    start: verb.len() + start,
                    end: verb.len() + end,
                    style: Style::MagitSha,
                });
            }
            spans
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_styled_spans_colors_add_remove_hunk_and_file_headers() {
        let diff =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n+added\n-removed\n context\n";
        let spans = diff_styled_spans(diff);
        assert_eq!(spans.len(), 7);
        assert_eq!(spans[0][0].style, Style::Keyword); // diff --git
        assert_eq!(spans[1][0].style, Style::Link); // ---
        assert_eq!(spans[2][0].style, Style::Link); // +++
        assert_eq!(spans[3][0].style, Style::Comment); // @@
        assert_eq!(spans[4][0].style, Style::DiffAdd); // +added
        assert_eq!(spans[5][0].style, Style::DiffRemove); // -removed
        assert!(spans[6].is_empty()); // plain context line
    }

    #[test]
    fn commit_buffer_styled_spans_scopes_diff_coloring_and_skips_the_header_line() {
        let text = "--- Staged diff (review before committing) ---\n+added\n--- Commit message (edit below) ---\nmy message\n";
        // Line 0 is the header (starts with "---" but must NOT be
        // colored as a diff file marker); line 1 is the real diff
        // content (diff_start_line=1); line 2 is the marker, out of
        // range (diff_end_line=2), so it must also stay unstyled
        // despite ALSO starting with "---".
        let spans = commit_buffer_styled_spans(text, 1, 2);
        assert!(
            spans[0].is_empty(),
            "header line must not be colored as a diff marker"
        );
        assert_eq!(spans[1][0].style, Style::DiffAdd);
        assert!(
            spans[2].is_empty(),
            "message marker line must not be colored despite starting with ---"
        );
        assert!(spans[3].is_empty());
    }

    #[test]
    fn log_styled_spans_colors_sha_and_ref_decoration() {
        let text =
            "* a1b2c3d (HEAD -> main, origin/main) Subject one\n| * b2c3d4e Another commit\n";
        let spans = log_styled_spans(text);
        assert_eq!(spans[0][0].style, Style::MagitSha);
        assert_eq!(
            &text.lines().next().unwrap()[spans[0][0].start..spans[0][0].end],
            "a1b2c3d"
        );
        assert_eq!(spans[0][1].style, Style::MagitRefDecoration);
        assert!(
            text.lines().next().unwrap()[spans[0][1].start..spans[0][1].end].starts_with("(HEAD")
        );
        // Second line has a SHA but no ref decoration.
        assert_eq!(spans[1].len(), 1);
        assert_eq!(spans[1][0].style, Style::MagitSha);
    }

    #[test]
    fn log_styled_spans_graph_only_line_has_no_spans() {
        let text = "|\\  \n";
        let spans = log_styled_spans(text);
        assert!(spans[0].is_empty());
    }

    #[test]
    fn blame_styled_spans_colors_sha_and_author_columns() {
        let line = format!("{} {:>12}  some code here\n", "a1b2c3d8", "Jane Doe");
        let spans = blame_styled_spans(&line);
        assert_eq!(spans[0][0].style, Style::MagitSha);
        assert_eq!(spans[0][0].start, 0);
        assert_eq!(spans[0][0].end, 8);
        assert_eq!(spans[0][1].style, Style::MagitAuthor);
    }

    #[test]
    fn blame_styled_spans_ignores_non_blame_lines() {
        let spans = blame_styled_spans("No file to blame\n");
        assert!(spans[0].is_empty());
    }

    #[test]
    fn branch_styled_spans_colors_only_the_current_branch() {
        let text = "Branches (2)\n* main\n  feature/foo\n";
        let spans = branch_styled_spans(text);
        assert!(spans[0].is_empty());
        assert_eq!(spans[1][0].style, Style::MagitBranchCurrent);
        assert!(spans[2].is_empty());
    }

    /// MG.15: the stash list carries `stash@{N}` now, so this styler
    /// stopped being a no-op. Offsets must land on the label, the
    /// index, and the message — the same split `sections.rs` gives the
    /// status buffer's stash entries, so a stash reads the same in
    /// both views.
    #[test]
    fn stash_styled_spans_colors_the_label_index_and_message() {
        let row = crate::magit_stash_mode::list_row(2, "WIP on main: 1234abc msg");
        let text = format!("Stashes (1)\n{row}\n");
        let spans = stash_styled_spans(&text);
        assert!(spans[0].is_empty(), "the header is not a stash row");
        let row_spans = &spans[1];
        assert_eq!(row_spans[0].style, Style::Keyword);
        assert_eq!(&row[row_spans[0].start..row_spans[0].end], "stash@{");
        assert_eq!(row_spans[1].style, Style::Number);
        assert_eq!(&row[row_spans[1].start..row_spans[1].end], "2");
        assert_eq!(row_spans[2].style, Style::Comment);
        assert_eq!(
            &row[row_spans[2].start..row_spans[2].end],
            "WIP on main: 1234abc msg"
        );
    }

    /// A two-digit index must not shift the message span off by one —
    /// the offsets are computed, not hardcoded.
    #[test]
    fn stash_styled_spans_handles_a_multi_digit_index() {
        let row = crate::magit_stash_mode::list_row(12, "message");
        let spans = stash_styled_spans(&row);
        assert_eq!(&row[spans[0][1].start..spans[0][1].end], "12");
        assert_eq!(&row[spans[0][2].start..spans[0][2].end], "message");
    }

    /// A non-ASCII message must not panic the byte-offset slicing, and
    /// the message span must cover all of it.
    #[test]
    fn stash_styled_spans_is_byte_safe_with_a_non_ascii_message() {
        let row = crate::magit_stash_mode::list_row(0, "WIP — café ☕");
        let spans = stash_styled_spans(&row);
        assert_eq!(&row[spans[0][2].start..spans[0][2].end], "WIP — café ☕");
    }

    #[test]
    fn stash_styled_spans_leaves_non_row_lines_alone() {
        assert!(stash_styled_spans("No stashes.\n")[0].is_empty());
        assert!(stash_styled_spans("Stashes (0)\n")[0].is_empty());
    }

    #[test]
    fn rebase_styled_spans_colors_verb_and_sha() {
        let text =
            "pick a1b2c3d Subject one\nreword b2c3d4e Subject two\n# a comment\nnot-a-verb line\n";
        let spans = rebase_styled_spans(text);
        assert_eq!(spans[0][0].style, Style::MagitRebaseVerb);
        assert_eq!(spans[0][1].style, Style::MagitSha);
        assert_eq!(spans[1][0].style, Style::MagitRebaseVerb);
        assert_eq!(spans[1][1].style, Style::MagitSha);
        assert_eq!(spans[2][0].style, Style::Comment);
        assert!(spans[3].is_empty());
    }
}
