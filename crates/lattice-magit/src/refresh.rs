//! MG.2: magit-status buffer refresh.
//!
//! Runs `git status`, `git stash list`, and `git log` on
//! `spawn_blocking`, formats the output through the section
//! index, and applies it to the buffer via `apply_edit_batch`.
//! No diff commands — diffs load on demand via `=`.

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::{Repository, WorkingTree, PathStatus, Stash};

use crate::sections::{Section, SectionEntry, SectionIndex, SectionKind};

/// Run the magit-status refresh and apply formatted output to the
/// buffer. Call on `spawn_blocking` — this does blocking git I/O.
pub async fn refresh_status(
    _buffer_id: BufferId,
    handle: Arc<dyn Document>,
    workdir: std::path::PathBuf,
) {
    let repo = match Repository::discover(&workdir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "lattice_magit", "refresh: repo discover failed: {e}");
            return;
        }
    };

    let index = build_section_index(&repo);

    // Format buffer content
    let text = index.format_buffer();
    if text.is_empty() {
        let empty = "No changes (working tree clean)\n";
        apply_full_replace(&handle, empty.to_string()).await;
    } else {
        apply_full_replace(&handle, text).await;
    }
}

/// Build the section index from live git data.
fn build_section_index(repo: &Repository) -> SectionIndex {
    let mut index = SectionIndex::default();

    // Branch name
    index.branch = current_branch(repo);

    // Statuses
    let statuses = match WorkingTree::statuses(repo) {
        Ok(s) => s,
        Err(_) => return index,
    };

    let mut staged: Vec<SectionEntry> = Vec::new();
    let mut unstaged: Vec<SectionEntry> = Vec::new();
    let mut untracked: Vec<SectionEntry> = Vec::new();

    for (path, status) in statuses {
        match status {
            PathStatus::Untracked => {
                untracked.push(SectionEntry::UntrackedFile { path });
            }
            PathStatus::Added => {
                staged.push(SectionEntry::File { path, status });
            }
            PathStatus::Modified => {
                unstaged.push(SectionEntry::File { path, status });
            }
            PathStatus::Deleted => {
                unstaged.push(SectionEntry::File { path, status });
            }
            PathStatus::Conflicted => {
                // Show in both sections
                staged.push(SectionEntry::File {
                    path: path.clone(),
                    status,
                });
                unstaged.push(SectionEntry::File { path, status });
            }
            _ => {
                unstaged.push(SectionEntry::File { path, status });
            }
        }
    }

    // Stashes
    let stashes: Vec<SectionEntry> = Stash::list(repo)
        .unwrap_or_default()
        .into_iter()
        .map(|s| SectionEntry::Stash {
            index: s.index,
            message: s.message,
        })
        .collect();

    // Recent commits
    let commits: Vec<SectionEntry> = recent_commits(repo)
        .into_iter()
        .map(|(sha, subject)| SectionEntry::Commit { sha, subject })
        .collect();

    let mut line = 0usize;

    // Staged section
    if !staged.is_empty() {
        let body_start = line + 1; // after header
        let body_end = body_start + staged.len();
        index.sections.push(Section {
            kind: SectionKind::Staged,
            header_line: line,
            body_start,
            body_end,
            entries: staged,
        });
        line = body_end + 1; // +1 for blank line after section
    }

    // Unstaged section
    if !unstaged.is_empty() {
        let body_start = line + 1;
        let body_end = body_start + unstaged.len();
        index.sections.push(Section {
            kind: SectionKind::Unstaged,
            header_line: line,
            body_start,
            body_end,
            entries: unstaged,
        });
        line = body_end + 1;
    }

    // Untracked section
    if !untracked.is_empty() {
        let body_start = line + 1;
        let body_end = body_start + untracked.len();
        index.sections.push(Section {
            kind: SectionKind::Untracked,
            header_line: line,
            body_start,
            body_end,
            entries: untracked,
        });
        line = body_end + 1;
    }

    // Stashes section
    if !stashes.is_empty() {
        let body_start = line + 1;
        let body_end = body_start + stashes.len();
        index.sections.push(Section {
            kind: SectionKind::Stashes,
            header_line: line,
            body_start,
            body_end,
            entries: stashes,
        });
        line = body_end + 1;
    }

    // Recent commits section
    if !commits.is_empty() {
        let body_start = line + 1;
        let body_end = body_start + commits.len();
        index.sections.push(Section {
            kind: SectionKind::RecentCommits,
            header_line: line,
            body_start,
            body_end,
            entries: commits,
        });
    }

    index
}

fn current_branch(repo: &Repository) -> String {
    repo.run_git_str(["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "(unknown)".to_string())
}

fn recent_commits(repo: &Repository) -> Vec<(String, String)> {
    let output = repo
        .run_git_str([
            "log",
            "--oneline",
            "-20",
            "--format=%h %s",
        ])
        .unwrap_or_default();

    output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.splitn(2, ' ');
            let sha = parts.next().unwrap_or("").to_string();
            let subject = parts.next().unwrap_or("").to_string();
            (sha, subject)
        })
        .collect()
}

/// Replace the entire buffer content with new text.
async fn apply_full_replace(handle: &Arc<dyn Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let edit = Edit::replace(Range::new(Position::ZERO, end), text);
    let _ = handle.apply_edit_batch(vec![edit]).await;
}
