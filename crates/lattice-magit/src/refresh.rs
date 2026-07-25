//! MG.2: magit-status buffer refresh.
//!
//! Runs `git status`, `git stash list`, and `git log` on
//! `spawn_blocking`, formats the output through the section
//! index, and applies it to the buffer via `apply_edit_batch`.
//! No diff commands — diffs load on demand via `=`.

use std::path::PathBuf;
use std::sync::Arc;

use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::{PathStatus, Repository, Stash, WorkingTree};

use crate::sections::{Section, SectionEntry, SectionIndex, SectionKind};

/// Run the magit-status refresh and apply formatted output.
/// Called from inside `spawn_blocking` — this does blocking I/O
/// (git commands), formats, then sends edits to the actor thread.
pub async fn refresh_and_apply(
    handle: Arc<dyn Document>,
    workdir: PathBuf,
) {
    let text = build_status_text(&workdir);
    apply_full_replace(&handle, text).await;
}

/// Build the status buffer text from live git data.
/// Blocking — call on `spawn_blocking`.
fn build_status_text(workdir: &PathBuf) -> String {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "lattice_magit", "refresh: repo discover failed: {e}");
            return "Not a git repository.\n".to_string();
        }
    };

    let index = build_section_index(&repo);
    let text = index.format_buffer();
    if text.is_empty() {
        "No changes (working tree clean)\n".to_string()
    } else {
        text
    }
}

fn build_section_index(repo: &Repository) -> SectionIndex {
    let mut index = SectionIndex::default();
    index.branch = current_branch(repo);
    populate_ahead_behind(repo, &mut index);

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
            PathStatus::Modified | PathStatus::Conflicted => {
                // Modified in worktree → unstaged.
                // Conflicted (both staged + unstaged changes) appears in both.
                unstaged.push(SectionEntry::File {
                    path: path.clone(),
                    status,
                });
            }
            PathStatus::Deleted => {
                unstaged.push(SectionEntry::File { path, status });
            }
            PathStatus::Unmerged => {
                // Show unmerged in unstaged with a distinct label
                unstaged.push(SectionEntry::File {
                    path: path.clone(),
                    status,
                });
            }
            PathStatus::Ignored | PathStatus::Clean => {
                // Skip — ignored files don't appear in status
            }
        }
    }

    let stashes: Vec<SectionEntry> = Stash::list(repo)
        .unwrap_or_default()
        .into_iter()
        .map(|s| SectionEntry::Stash {
            index: s.index,
            message: s.message,
        })
        .collect();

    let commits: Vec<SectionEntry> = recent_commits(repo)
        .into_iter()
        .map(|(sha, subject)| SectionEntry::Commit { sha, subject })
        .collect();

    let mut line = 0usize;

    let mut push_section =
        |idx: &mut SectionIndex,
         entries: Vec<SectionEntry>,
         kind: SectionKind,
         line: &mut usize| {
            if entries.is_empty() {
                return;
            }
            let body_start = *line + 1;
            let body_end = body_start + entries.len();
            idx.sections.push(Section {
                kind,
                header_line: *line,
                body_start,
                body_end,
                entries,
            });
            *line = body_end + 1; // +1 for blank separator
        };

    push_section(&mut index, staged, SectionKind::Staged, &mut line);
    push_section(&mut index, unstaged, SectionKind::Unstaged, &mut line);
    push_section(&mut index, untracked, SectionKind::Untracked, &mut line);
    push_section(&mut index, stashes, SectionKind::Stashes, &mut line);
    push_section(&mut index, commits, SectionKind::RecentCommits, &mut line);

    index
}

fn current_branch(repo: &Repository) -> String {
    repo.run_git_str(["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "(detached)".to_string())
}

fn populate_ahead_behind(repo: &Repository, index: &mut SectionIndex) {
    if let Ok(output) =
        repo.run_git_str(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
    {
        let parts: Vec<&str> = output.split_whitespace().collect();
        if parts.len() == 2 {
            index.behind = parts[0].parse().unwrap_or(0);
            index.ahead = parts[1].parse().unwrap_or(0);
        }
    }
}

fn recent_commits(repo: &Repository) -> Vec<(String, String)> {
    let output = repo
        .run_git_str(["log", "--oneline", "-20", "--format=%h %s"])
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

async fn apply_full_replace(handle: &Arc<dyn Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let edit = Edit::replace(Range::new(Position::ZERO, end), text);
    let _ = handle.apply_edit_batch(vec![edit]).await;
}
