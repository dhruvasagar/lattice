//! MG.2: Magit status section index and content formatting.
//!
//! Lazy by default: stores file paths + status labels only.
//! No diffs are pre-computed — diffs load on demand via `=`.

use std::path::PathBuf;

use lattice_vcs::PathStatus;

#[derive(Debug, Clone)]
pub enum SectionEntry {
    File {
        path: PathBuf,
        status: PathStatus,
        /// Where a renamed / copied path came from, so the row can
        /// render `old -> new` the way git and magit do — and so
        /// unstaging can reset BOTH paths. Resetting only the new one
        /// leaves the old path staged-DELETED, a deletion the user
        /// never asked for and the next commit would record.
        original_path: Option<PathBuf>,
    },
    Stash {
        index: usize,
        message: String,
    },
    Commit {
        sha: String,
        subject: String,
    },
    UntrackedFile {
        path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct Section {
    pub kind: SectionKind,
    pub header_line: usize,
    pub body_start: usize,
    pub body_end: usize,
    pub entries: Vec<SectionEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    Staged,
    Unstaged,
    Untracked,
    Stashes,
    RecentCommits,
}

/// The fixed prefix `format_buffer_styled` renders for each
/// [`SectionKind`]'s header line, before the `" (N)"` count suffix.
/// Single source of truth for "is this line a section header" —
/// previously hand-duplicated as three independent string-prefix
/// lists (here implicitly, `actions.rs::section_header_above`, and
/// `magit_core_mode.rs::section_headers`), free to drift out of sync
/// with each other and with this list.
pub const SECTION_HEADER_PREFIXES: [&str; 5] = [
    "Staged changes",
    "Unstaged changes",
    "Untracked files",
    "Stashes",
    "Recent commits",
];

/// True if `text` (already trimmed of buffer indentation) is a
/// section header line.
pub fn is_section_header(text: &str) -> bool {
    SECTION_HEADER_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

#[derive(Debug, Clone, Default)]
pub struct SectionIndex {
    pub sections: Vec<Section>,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    /// MG.21f: the bisect in progress, if any.
    ///
    /// Repo state rather than a section — this struct already carries
    /// `branch` / `ahead` / `behind` for the same reason, and the
    /// headerline is where lattice answers "what state is this repo
    /// in" (it already carries `REBASE IN PROGRESS`). A `SectionKind`
    /// would have been the wrong home: every `SectionEntry` variant
    /// carries diff-bearing-file invariants — a path, a stage
    /// operation, an expandable patch — and a bisect has none of them.
    pub bisect: Option<lattice_vcs::BisectState>,
}

impl SectionIndex {
    pub fn format_buffer(&self) -> String {
        self.format_buffer_styled().0
    }

    pub fn format_buffer_styled(&self) -> (String, Vec<Vec<lattice_cells::style::StyledSpan>>) {
        let (text, spans, _) = self.format_buffer_styled_with(|_, _| None);
        (text, spans)
    }

    /// MG.18d: format the buffer with each entry's diff **inlined**
    /// where `inline` supplies one.
    ///
    /// A refresh replaces the whole buffer, so any expansion the user
    /// had open dies with it — which at hunk granularity means losing
    /// your place after every single `s`. Rebuilding *with* the
    /// expansions is what lets the entry survive: one edit, one span
    /// vector, no splice arithmetic against a buffer that is being
    /// replaced underneath it, and no collapse-then-expand flicker.
    ///
    /// `inline(entry, kind)` returns `(key, diff_text)` for an entry
    /// that should come back expanded. The returned
    /// `Vec<(key, line_count)>` is the rebuilt expansion bookkeeping —
    /// counts are recomputed rather than carried, because staging a
    /// hunk makes the diff shorter.
    pub fn format_buffer_styled_with(
        &self,
        inline: impl Fn(&SectionEntry, SectionKind) -> Option<(String, String)>,
    ) -> (
        String,
        Vec<Vec<lattice_cells::style::StyledSpan>>,
        Vec<(String, usize)>,
    ) {
        let mut out = String::new();
        let mut spans: Vec<Vec<lattice_cells::style::StyledSpan>> = Vec::new();
        let mut expanded: Vec<(String, usize)> = Vec::new();

        for section in &self.sections {
            if section.entries.is_empty() {
                continue;
            }
            let header = match section.kind {
                SectionKind::Staged => format!("Staged changes ({})", section.entries.len()),
                SectionKind::Unstaged => format!("Unstaged changes ({})", section.entries.len()),
                SectionKind::Untracked => format!("Untracked files ({})", section.entries.len()),
                SectionKind::Stashes => format!("Stashes ({})", section.entries.len()),
                SectionKind::RecentCommits => format!("Recent commits ({})", section.entries.len()),
            };
            let line_idx = out.matches('\n').count();
            while spans.len() <= line_idx {
                spans.push(Vec::new());
            }
            out.push_str(&header);
            out.push('\n');
            spans[line_idx] = vec![lattice_cells::style::StyledSpan {
                start: 0,
                end: header.len(),
                style: lattice_cells::style::Style::Heading2,
            }];

            for entry in &section.entries {
                let line_idx = out.matches('\n').count();
                while spans.len() <= line_idx {
                    spans.push(Vec::new());
                }
                match entry {
                    SectionEntry::File {
                        path,
                        status,
                        original_path,
                    } => {
                        let label = status_label(*status);
                        // A rename / copy renders `old -> new`, as git
                        // and magit both do: the origin is half the
                        // information, and a row saying only `new` is
                        // indistinguishable from a fresh file.
                        let path_s = match original_path {
                            Some(from) => std::borrow::Cow::Owned(format!(
                                "{} -> {}",
                                from.to_string_lossy(),
                                path.to_string_lossy()
                            )),
                            None => path.to_string_lossy(),
                        };
                        let path_text = format!("  {label:<LABEL_WIDTH$} {path_s}");
                        out.push_str(&path_text);
                        out.push('\n');
                        let path_start = 2 + LABEL_WIDTH + 1;
                        let label_end = 2 + label.len();
                        spans[line_idx] = match status {
                            PathStatus::Deleted => vec![
                                lattice_cells::style::StyledSpan {
                                    start: 2,
                                    end: label_end,
                                    style: lattice_cells::style::Style::DiagnosticError,
                                },
                                lattice_cells::style::StyledSpan {
                                    start: path_start,
                                    end: path_text.len(),
                                    style: lattice_cells::style::Style::String,
                                },
                            ],
                            PathStatus::Added => vec![
                                lattice_cells::style::StyledSpan {
                                    start: 2,
                                    end: label_end,
                                    style: lattice_cells::style::Style::String,
                                },
                                lattice_cells::style::StyledSpan {
                                    start: path_start,
                                    end: path_text.len(),
                                    style: lattice_cells::style::Style::String,
                                },
                            ],
                            PathStatus::Unmerged(_) => vec![
                                lattice_cells::style::StyledSpan {
                                    start: 2,
                                    end: label_end,
                                    style: lattice_cells::style::Style::DiagnosticWarning,
                                },
                                lattice_cells::style::StyledSpan {
                                    start: path_start,
                                    end: path_text.len(),
                                    style: lattice_cells::style::Style::String,
                                },
                            ],
                            _ => vec![
                                lattice_cells::style::StyledSpan {
                                    start: 2,
                                    end: label_end,
                                    style: lattice_cells::style::Style::Keyword,
                                },
                                lattice_cells::style::StyledSpan {
                                    start: path_start,
                                    end: path_text.len(),
                                    style: lattice_cells::style::Style::String,
                                },
                            ],
                        };
                    }
                    SectionEntry::Stash { index, message } => {
                        let idx_str = format!("{}", index);
                        // ONE writer for the stash row, shared with the
                        // stash-list buffer. It was duplicated here, and
                        // that is the same writer/reader split that made
                        // every chord in the stash buffer dead in MG.15
                        // — now both views also *read* it back through
                        // `parse_index` to resolve the stash under the
                        // cursor, so a drift would break the chords in
                        // whichever buffer lost the coin toss.
                        let line_text = crate::magit_stash_mode::list_row(*index, message);
                        out.push_str(&line_text);
                        out.push('\n');
                        spans[line_idx] = vec![
                            lattice_cells::style::StyledSpan {
                                start: 2,
                                end: 9,
                                style: lattice_cells::style::Style::Keyword,
                            },
                            lattice_cells::style::StyledSpan {
                                start: 9,
                                end: 9 + idx_str.len(),
                                style: lattice_cells::style::Style::Number,
                            },
                            lattice_cells::style::StyledSpan {
                                start: 11 + idx_str.len(),
                                end: line_text.len(),
                                style: lattice_cells::style::Style::Comment,
                            },
                        ];
                    }
                    SectionEntry::Commit { sha, subject } => {
                        let sha_len = sha.len();
                        let line_text = format!("  {} {}", sha, subject);
                        out.push_str(&line_text);
                        out.push('\n');
                        spans[line_idx] = vec![
                            lattice_cells::style::StyledSpan {
                                start: 2,
                                end: 2 + sha_len,
                                style: lattice_cells::style::Style::Link,
                            },
                            lattice_cells::style::StyledSpan {
                                start: 2 + sha_len + 1,
                                end: line_text.len(),
                                style: lattice_cells::style::Style::Comment,
                            },
                        ];
                    }
                    SectionEntry::UntrackedFile { path } => {
                        let label = "untracked";
                        let path_s = path.to_string_lossy();
                        let line_text = format!("  {label:<LABEL_WIDTH$} {path_s}");
                        out.push_str(&line_text);
                        out.push('\n');
                        let path_start = 2 + LABEL_WIDTH + 1;
                        spans[line_idx] = vec![
                            lattice_cells::style::StyledSpan {
                                start: 2,
                                end: 2 + label.len(),
                                style: lattice_cells::style::Style::Comment,
                            },
                            lattice_cells::style::StyledSpan {
                                start: path_start,
                                end: line_text.len(),
                                style: lattice_cells::style::Style::Comment,
                            },
                        ];
                    }
                }
                // MG.18d: the entry's own diff, if it was open before
                // the refresh. Highlighted through the same
                // `diff_styled_spans` the `=` toggle uses, so an
                // expansion looks identical however it got there.
                if let Some((key, diff)) = inline(entry, section.kind) {
                    let diff = diff.trim_end();
                    if !diff.is_empty() {
                        let diff_spans = crate::highlight::diff_styled_spans(diff);
                        let mut count = 0usize;
                        for (i, line) in diff.lines().enumerate() {
                            let line_idx = out.matches('\n').count();
                            while spans.len() <= line_idx {
                                spans.push(Vec::new());
                            }
                            out.push_str(line);
                            out.push('\n');
                            if let Some(row) = diff_spans.get(i) {
                                spans[line_idx] = row.clone();
                            }
                            count += 1;
                        }
                        expanded.push((key, count));
                    }
                }
            }
            // blank separator line
            let line_idx = out.matches('\n').count();
            while spans.len() <= line_idx {
                spans.push(Vec::new());
            }
            out.push('\n');
        }

        (out, spans, expanded)
    }

    // MG.14 removed `branch_status_line()`. It formatted branch +
    // ahead/behind into a `String` and had no callers in any revision
    // of this crate — the header it was written for did not exist.
    // `headerline::status_fields` renders the same data now, as
    // per-role coloured fields rather than one flat string, and is
    // reached from the one place a status refresh runs.
}

fn status_label(status: PathStatus) -> &'static str {
    match status {
        PathStatus::Modified => "modified",
        PathStatus::Added => "new file",
        PathStatus::Deleted => "deleted",
        // Git's own wording for each. `renamed` / `copied` used to
        // render as "new file" because the parser collapsed them into
        // `Added`; the origin path travels on
        // `PathChange::original_path` but is not shown yet — see the
        // note in `refresh`.
        PathStatus::Renamed => "renamed",
        PathStatus::Copied => "copied",
        PathStatus::TypeChanged => "typechange",
        PathStatus::Untracked => "untracked",
        PathStatus::Ignored => "ignored",
        // Git's own wording per combination — "both modified" and
        // "deleted by them" are different problems and a generic
        // "unmerged" tells the user neither.
        PathStatus::Unmerged(kind) => kind.label(),
    }
}

/// Width of the label column in a rendered file row.
///
/// Was a bare `12` written out at three sites. The unmerged labels
/// ("deleted by them" is 15 columns) overflow that, and an overflowing
/// `{:<12}` silently stops padding — which would have left the styled
/// spans' hardcoded `path_start` pointing INTO the label. Derived from
/// the labels themselves, with `label_column_fits_every_label` holding
/// it honest.
pub(crate) const LABEL_WIDTH: usize = 15;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_section_header_matches_every_rendered_header_prefix() {
        for prefix in SECTION_HEADER_PREFIXES {
            let rendered = format!("{prefix} (3)");
            assert!(is_section_header(&rendered), "{rendered:?} should match");
        }
    }

    #[test]
    fn is_section_header_rejects_entry_lines() {
        assert!(!is_section_header("modified     src/lib.rs"));
        assert!(!is_section_header("stash@{0} WIP on main"));
        assert!(!is_section_header(""));
    }

    /// `LABEL_WIDTH` must fit every label, or `{:<WIDTH$}` stops
    /// padding and the styled spans' `path_start` lands inside the
    /// label text instead of on the path.
    #[test]
    fn label_column_fits_every_label() {
        for label in crate::actions::FILE_LABELS {
            assert!(
                label.len() <= LABEL_WIDTH,
                "{label:?} is {} columns, wider than LABEL_WIDTH ({LABEL_WIDTH})",
                label.len()
            );
        }
    }

    #[test]
    fn status_label_is_a_subset_of_actions_file_labels() {
        // actions::FILE_LABELS must stay in sync with every label this
        // renders — the comment there documents the pairing; this
        // test catches drift mechanically instead of by inspection.
        let mut all = vec![
            PathStatus::Modified,
            PathStatus::Added,
            PathStatus::Deleted,
            PathStatus::Renamed,
            PathStatus::Copied,
            PathStatus::TypeChanged,
            PathStatus::Untracked,
            PathStatus::Ignored,
        ];
        // Every unmerged combination renders its own label, so every
        // one has to be reachable from `classify_line`.
        all.extend(
            lattice_vcs::UnmergedKind::ALL
                .into_iter()
                .map(PathStatus::Unmerged),
        );
        for status in all {
            let label = status_label(status);
            assert!(
                crate::actions::FILE_LABELS.contains(&label),
                "status_label({status:?}) = {label:?} not in actions::FILE_LABELS"
            );
        }
    }
}
