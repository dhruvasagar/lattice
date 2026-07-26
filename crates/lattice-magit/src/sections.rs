//! MG.2: Magit status section index and content formatting.
//!
//! Lazy by default: stores file paths + status labels only.
//! No diffs are pre-computed — diffs load on demand via `=`.

use std::path::PathBuf;

use lattice_vcs::PathStatus;

#[derive(Debug, Clone)]
pub enum SectionEntry {
    File { path: PathBuf, status: PathStatus },
    Stash { index: usize, message: String },
    Commit { sha: String, subject: String },
    UntrackedFile { path: PathBuf },
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

#[derive(Debug, Clone, Default)]
pub struct SectionIndex {
    pub sections: Vec<Section>,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
}

impl SectionIndex {
    pub fn format_buffer(&self) -> String {
        self.format_buffer_styled().0
    }

    pub fn format_buffer_styled(&self) -> (String, Vec<Vec<lattice_cells::style::StyledSpan>>) {
        let mut out = String::new();
        let mut spans: Vec<Vec<lattice_cells::style::StyledSpan>> = Vec::new();

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
                    SectionEntry::File { path, status } => {
                        let label = status_label(*status);
                        let path_s = path.to_string_lossy();
                        let path_text = format!("  {:<12} {}", label, path_s);
                        out.push_str(&path_text);
                        out.push('\n');
                        let path_start = 2 + 12 + 1;
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
                            PathStatus::Conflicted => vec![
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
                        let line_text = format!("  stash@{{{}}} {}", index, message);
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
                        let line_text = format!("  {:<12} {}", label, path_s);
                        out.push_str(&line_text);
                        out.push('\n');
                        let path_start = 2 + 12 + 1;
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
            }
            // blank separator line
            let line_idx = out.matches('\n').count();
            while spans.len() <= line_idx {
                spans.push(Vec::new());
            }
            out.push('\n');
        }

        (out, spans)
    }

    pub fn branch_status_line(&self) -> String {
        let mut s = self.branch.clone();
        if self.ahead > 0 {
            s.push_str(&format!(" {}↑", self.ahead));
        }
        if self.behind > 0 {
            s.push_str(&format!(" {}↓", self.behind));
        }
        s
    }
}

fn status_label(status: PathStatus) -> &'static str {
    match status {
        PathStatus::Clean => "clean",
        PathStatus::Modified => "modified",
        PathStatus::Added => "new file",
        PathStatus::Deleted => "deleted",
        PathStatus::Untracked => "untracked",
        PathStatus::Ignored => "ignored",
        PathStatus::Unmerged => "unmerged",
        PathStatus::Conflicted => "modified",
    }
}
