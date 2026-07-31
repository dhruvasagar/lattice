//! MG.2: magit-status buffer refresh.
//!
//! Runs `git status`, `git stash list`, and `git log` on
//! `spawn_blocking`, formats the output through the section
//! index, and applies it to the buffer via `apply_edit_batch`.
//! No diff commands — diffs load on demand via `=`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use lattice_cells::StyledSpan;
use lattice_core::BufferId;
use lattice_mode::PendingSyntheticHighlightsHandle;
use lattice_runtime::Document;
use lattice_vcs::{PathStatus, Repository, Stash, WorkingTree};

use crate::headerline::{self, Field};
use crate::sections::{Section, SectionEntry, SectionIndex, SectionKind};

/// Build the status buffer text (+ styled spans + MG.14 header
/// fields) from live git data. Blocking — call on `spawn_blocking`.
///
/// The header comes out of the SAME [`SectionIndex`] the body is
/// formatted from — branch, ahead/behind, and the per-section counts
/// are all already in hand — so surfacing it costs no extra git call.
/// (Before MG.14 the index's branch/ahead/behind were computed on
/// every refresh and thrown away: `SectionIndex::branch_status_line`
/// was written to render them and never called from anywhere.)
pub fn build_and_format(
    workdir: &PathBuf,
    expanded: &HashSet<String>,
) -> (
    String,
    Vec<Vec<StyledSpan>>,
    Vec<Field>,
    HashMap<String, usize>,
) {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "lattice_magit", "refresh: repo discover failed: {e}");
            return (
                "Not a git repository.\n".to_string(),
                Vec::new(),
                Vec::new(),
                HashMap::new(),
            );
        }
    };

    let index = build_section_index(&repo);
    let header = headerline::status_fields(&index, workdir);
    // MG.18d: re-run each still-open entry's `git diff` so the rebuilt
    // buffer carries its expansion. Blocking, like every other call in
    // this function — it runs on `spawn_blocking`, never the actor.
    // Cost is proportional to what the user had open, which is what an
    // expansion already costs; nothing is fetched for a collapsed entry.
    let (text, spans, reopened) = index.format_buffer_styled_with(|entry, kind| {
        let line = entry_as_status_line(entry, kind)?;
        let key = crate::actions::entry_key(&line);
        if !expanded.contains(&key) {
            return None;
        }
        let diff = crate::actions::run_show(workdir, &line)?;
        (!diff.trim().is_empty()).then_some((key, diff))
    });
    if text.is_empty() {
        (
            "No changes (working tree clean)\n".to_string(),
            Vec::new(),
            header,
            HashMap::new(),
        )
    } else {
        (text, spans, header, reopened.into_iter().collect())
    }
}

/// The [`StatusLine`](crate::actions::StatusLine) a rendered entry
/// classifies as — the same identity `classify_line` derives from the
/// row, resolved here from the index instead of the text.
///
/// Both sides must agree or an expansion would be keyed one way when
/// opened by `=` and another when rebuilt by a refresh, and the
/// entry would silently fail to come back.
fn entry_as_status_line(
    entry: &SectionEntry,
    kind: SectionKind,
) -> Option<crate::actions::StatusLine> {
    use crate::actions::StatusLine;
    Some(match entry {
        SectionEntry::File { path, .. } => StatusLine::File {
            path: path.clone(),
            staged: kind == SectionKind::Staged,
        },
        // An untracked file has no diff to show, but it classifies as a
        // `File` row (`untracked` is one of the entry labels), so it
        // keys the same way `=` would key it.
        SectionEntry::UntrackedFile { path } => StatusLine::File {
            path: path.clone(),
            staged: false,
        },
        SectionEntry::Stash { index, .. } => StatusLine::Stash { index: *index },
        SectionEntry::Commit { sha, .. } => StatusLine::Commit { sha: sha.clone() },
    })
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
            PathStatus::Modified => {
                unstaged.push(SectionEntry::File {
                    path: path.clone(),
                    status,
                });
            }
            PathStatus::Conflicted => {
                // Both staged and unstaged changes — appears in both sections
                staged.push(SectionEntry::File {
                    path: path.clone(),
                    status,
                });
                unstaged.push(SectionEntry::File { path, status });
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

    let push_section = |idx: &mut SectionIndex,
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
            // HEAD is left side → parts[0] = ahead (local commits not on upstream)
            // @{upstream} is right → parts[1] = behind (upstream commits not local)
            index.ahead = parts[0].parse().unwrap_or(0);
            index.behind = parts[1].parse().unwrap_or(0);
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

/// Apply a full buffer replacement, then store highlights and fire the
/// waker so the Editor repaints immediately. Async — call from a tokio
/// task (NOT spawn_blocking). The blocking I/O phase
/// (`build_and_format`) must complete before calling this.
pub async fn apply_and_highlight(
    handle: Arc<dyn Document>,
    text: String,
    spans: Vec<Vec<StyledSpan>>,
    pending_highlights: Option<PendingSyntheticHighlightsHandle>,
    buffer_id: BufferId,
) {
    crate::buffer_io::replace_buffer_text(&handle, text).await;
    if let Some(ref ph) = pending_highlights {
        ph.store_and_wake(buffer_id, spans);
    }
}

/// MG.18d — a refresh no longer throws away what you had open.
///
/// Before this, every refresh replaced the buffer with a collapsed
/// rebuild and cleared the expansion map to match. At file granularity
/// that was tolerable; at hunk granularity it means the diff you were
/// staging out of disappears on the first `s`.
#[cfg(test)]
mod expansion_survives_refresh {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// A repo with one modified, unstaged file.
    fn repo_with_an_unstaged_change() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("a.txt"), &base).unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        let edited: String = (1..=20)
            .map(|i| match i {
                2 => "line 2 EDITED\n".to_string(),
                19 => "line 19 EDITED\n".to_string(),
                _ => format!("line {i}\n"),
            })
            .collect();
        std::fs::write(p.join("a.txt"), &edited).unwrap();
        dir
    }

    fn open_key() -> String {
        crate::actions::entry_key(&crate::actions::StatusLine::File {
            path: PathBuf::from("a.txt"),
            staged: false,
        })
    }

    #[test]
    fn an_open_entrys_diff_comes_back_in_the_rebuilt_text() {
        let dir = repo_with_an_unstaged_change();
        let wd = dir.path().to_path_buf();

        let collapsed = build_and_format(&wd, &HashSet::new());
        assert!(
            !collapsed.0.contains("@@"),
            "nothing was open, so no diff is inlined:\n{}",
            collapsed.0
        );
        assert!(collapsed.3.is_empty());

        let open: HashSet<String> = [open_key()].into_iter().collect();
        let (text, spans, _, reopened) = build_and_format(&wd, &open);
        assert!(
            text.contains("line 2 EDITED"),
            "the open entry's diff is inlined:\n{text}"
        );
        // The recorded count is what a later collapse deletes, so it
        // must equal exactly the rows the rebuild added. Anything else
        // eats a neighbouring entry or leaves orphaned diff rows —
        // the failure `collapse_range`'s own regression test covers
        // from the other side.
        assert_eq!(
            reopened.get(&open_key()).copied(),
            Some(text.lines().count() - collapsed.0.lines().count()),
            "the recomputed count is exactly the rows the expansion added"
        );
        assert_eq!(
            spans.len(),
            text.lines().count(),
            "one span row per text row — a mismatch shifts every highlight below the diff"
        );
    }

    /// The entry key the refresh matches on must be the one `=` writes,
    /// or an expansion would silently fail to come back.
    #[test]
    fn the_rebuilt_key_is_the_one_the_toggle_uses() {
        let dir = repo_with_an_unstaged_change();
        let open: HashSet<String> = [open_key()].into_iter().collect();
        let (_, _, _, reopened) = build_and_format(&dir.path().to_path_buf(), &open);
        assert!(
            reopened.contains_key(&open_key()),
            "keyed as `f:false:a.txt`, the same as `classify_line` derives from the row"
        );
    }

    /// A key for a file that is no longer in the status output must not
    /// resurrect anything — and must not survive into the new map.
    #[test]
    fn a_stale_key_expands_nothing_and_does_not_survive() {
        let dir = repo_with_an_unstaged_change();
        let stale: HashSet<String> = ["f:false:gone.txt".to_string()].into_iter().collect();
        let (text, _, _, reopened) = build_and_format(&dir.path().to_path_buf(), &stale);
        assert!(!text.contains("@@"), "nothing inlined:\n{text}");
        assert!(reopened.is_empty(), "the stale key is dropped, not carried");
    }
}
