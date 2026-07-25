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
        let mut out = String::new();

        for section in &self.sections {
            if section.entries.is_empty() {
                continue;
            }
            let header = match section.kind {
                SectionKind::Staged => format!("Staged changes ({})", section.entries.len()),
                SectionKind::Unstaged => format!("Unstaged changes ({})", section.entries.len()),
                SectionKind::Untracked => format!("Untracked files ({})", section.entries.len()),
                SectionKind::Stashes => format!("Stashes ({})", section.entries.len()),
                SectionKind::RecentCommits => format!(
                    "Recent commits ({})",
                    section.entries.len()
                ),
            };
            out.push_str(&header);
            out.push('\n');

            for entry in &section.entries {
                match entry {
                    SectionEntry::File { path, status } => {
                        out.push_str(&format!(
                            "  {:<12} {}\n",
                            status_label(*status),
                            path.display()
                        ));
                    }
                    SectionEntry::Stash { index, message } => {
                        out.push_str(&format!("  stash@{{{}}} {}\n", index, message));
                    }
                    SectionEntry::Commit { sha, subject } => {
                        out.push_str(&format!("  {} {}\n", sha, subject));
                    }
                    SectionEntry::UntrackedFile { path } => {
                        out.push_str(&format!("  untracked    {}\n", path.display()));
                    }
                }
            }
            out.push('\n');
        }

        out
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
