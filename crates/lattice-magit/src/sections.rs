//! MG.2: Magit status section index and content formatting.
//!
//! Lazy by default: stores file paths + status labels only.
//! No diffs are pre-computed — diffs load on demand via `=`.
//! See `docs/dev/architecture/magit.md` §6-7.

use std::path::PathBuf;

use lattice_vcs::PathStatus;

/// One entry in a magit-status section.
#[derive(Debug, Clone)]
pub enum SectionEntry {
    File {
        path: PathBuf,
        status: PathStatus,
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

/// A named section in the magit-status buffer.
#[derive(Debug, Clone)]
pub struct Section {
    pub kind: SectionKind,
    pub header_line: usize,
    pub body_start: usize,
    pub body_end: usize,
    pub entries: Vec<SectionEntry>,
}

/// Top-level section categories in magit-status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    Staged,
    Unstaged,
    Untracked,
    Stashes,
    RecentCommits,
}

/// The full section index for a magit-status buffer.
#[derive(Debug, Clone, Default)]
pub struct SectionIndex {
    pub sections: Vec<Section>,
    /// Branch name for the headerline.
    pub branch: String,
    /// Ahead/behind counts.
    pub ahead: usize,
    pub behind: usize,
}

impl SectionIndex {
    /// Build formatted buffer content from the section index.
    /// Returns lines of text suitable for setting as the buffer content.
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
                        let label = status_label(*status);
                        out.push_str(&format!(
                            "  {:<12} {}\n",
                            label,
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
                        out.push_str(&format!("  {:<12} {}\n", "untracked", path.display()));
                    }
                }
            }

            out.push('\n');
        }

        out
    }

    /// Return the current branch name as a human-readable string,
    /// including ahead/behind indicators.
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

/// Human-readable status label matching `git status --porcelain`.
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
