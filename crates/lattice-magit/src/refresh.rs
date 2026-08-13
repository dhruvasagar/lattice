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
    context: i64,
    // DS-fix (2026-08-12): the grammar registry the reopened
    // expansions highlight through. Without it a refresh rebuilt every
    // open diff with the flat classifier and the syntax layer vanished.
    lang_registry: Option<&std::sync::Arc<lattice_syntax::LangRegistry>>,
) -> (
    String,
    Vec<Vec<StyledSpan>>,
    Vec<Field>,
    HashMap<String, usize>,
    // DR.3: intra-line refinement, line-aligned with the spans above.
    Vec<Vec<lattice_cells::RefineSpan>>,
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
                Vec::new(),
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
    let (text, spans, reopened, refine) = index.format_buffer_styled_with(
        |entry, kind| {
            let line = entry_as_status_line(entry, kind)?;
            let key = crate::actions::entry_key(&line);
            if !expanded.contains(&key) {
                return None;
            }
            let diff = crate::actions::run_show(workdir, &line, context)?;
            (!diff.trim().is_empty()).then_some((key, diff))
        },
        lang_registry,
    );
    if text.is_empty() {
        (
            "No changes (working tree clean)\n".to_string(),
            Vec::new(),
            header,
            HashMap::new(),
            Vec::new(),
        )
    } else {
        (text, spans, header, reopened.into_iter().collect(), refine)
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
        SectionEntry::File {
            path,
            status,
            original_path,
        } => StatusLine::File {
            path: path.clone(),
            staged: kind == SectionKind::Staged,
            untracked: *status == lattice_vcs::PathStatus::Untracked,
            original_path: original_path.clone(),
        },
        // An untracked file has no diff to show, but it classifies as a
        // `File` row (`untracked` is one of the entry labels), so it
        // keys the same way `=` would key it.
        SectionEntry::UntrackedFile { path } => StatusLine::File {
            original_path: None,
            path: path.clone(),
            staged: false,
            untracked: true,
        },
        SectionEntry::Stash { index, .. } => StatusLine::Stash { index: *index },
        SectionEntry::Commit { sha, .. } => StatusLine::Commit { sha: sha.clone() },
    })
}

fn build_section_index(repo: &Repository) -> SectionIndex {
    let mut index = SectionIndex {
        branch: current_branch(repo),
        ..Default::default()
    };
    populate_ahead_behind(repo, &mut index);
    // MG.21f. Costs a stat in the overwhelmingly common case — the
    // git calls behind the progress numbers run only once a bisect is
    // actually in flight, which is why `in_progress` gates `state`.
    // Which multi-commit operation is stopped mid-flight, if any.
    // Marker files in the gitdir, so it cannot go stale behind a git
    // command the user ran in a terminal.
    index.in_flight = lattice_vcs::InFlightOp::detect(repo);
    index.bisect = match lattice_vcs::Bisect::state(repo) {
        Ok(state) => state,
        Err(e) => {
            tracing::debug!("magit-status: bisect state unreadable: {e}");
            None
        }
    };

    let statuses = match WorkingTree::statuses(repo) {
        Ok(s) => s,
        Err(_) => return index,
    };

    let mut staged: Vec<SectionEntry> = Vec::new();
    let mut unstaged: Vec<SectionEntry> = Vec::new();
    let mut untracked: Vec<SectionEntry> = Vec::new();

    // Porcelain reports TWO independent axes per path (see
    // `lattice_vcs::PathChange`): what the index has staged, and what
    // the worktree has beyond it. Place each on its own axis, so a file
    // carrying both appears in both sections with the correct label on
    // each row — and a staged MODIFICATION stays "modified" instead of
    // being reported as `Added` purely to make it land in the staged
    // section, which is what rendered it as "new file".
    for (path, change) in statuses {
        if let Some(staged_status) = change.staged {
            staged.push(SectionEntry::File {
                path: path.clone(),
                status: staged_status,
                // Only the index axis carries rename/copy detection.
                original_path: change.original_path.clone(),
            });
        }
        match change.unstaged {
            Some(PathStatus::Untracked) => {
                untracked.push(SectionEntry::UntrackedFile { path });
            }
            // Ignored files never appear in the status view; `None` is
            // a worktree that matches the index.
            Some(PathStatus::Ignored) | None => {}
            Some(unstaged_status) => {
                unstaged.push(SectionEntry::File {
                    path,
                    status: unstaged_status,
                    original_path: None,
                });
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

    // Commits this branch has that its upstream does not. Magit shows
    // these INSTEAD of recent commits when there are any — the question
    // "what have I not pushed yet" answers "what have I done lately"
    // whenever the answer is non-empty, and showing both would list the
    // same commits twice under two headings.
    index.upstream = upstream_ref(repo);
    let unmerged: Vec<SectionEntry> = unmerged_commits(repo)
        .into_iter()
        .map(|(sha, subject)| SectionEntry::Commit { sha, subject })
        .collect();

    // Only computed when it will be shown — `git log` is a process
    // spawn, and the common case on a branch with work on it is that
    // this list is never rendered.
    let commits: Vec<SectionEntry> = if unmerged.is_empty() {
        recent_commits(repo)
            .into_iter()
            .map(|(sha, subject)| SectionEntry::Commit { sha, subject })
            .collect()
    } else {
        Vec::new()
    };

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
    push_section(&mut index, unmerged, SectionKind::Unmerged, &mut line);
    // `push_section` skips an empty list, so the either/or above needs
    // no branch here: exactly one of these two ever has entries.
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

/// The upstream ref this branch tracks, as `origin/main`.
///
/// `None` for a branch with no upstream, a detached HEAD, or a repo
/// with no remote — all ordinary states, not errors, and each one
/// simply means there is no "unmerged into" question to ask.
fn upstream_ref(repo: &Repository) -> Option<String> {
    let out = repo
        .run_git_str(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .ok()?;
    let name = out.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Commits on HEAD that `@{upstream}` does not have.
///
/// Empty when there is no upstream — `git rev-parse` fails and the
/// whole section is skipped, which is the right rendering for a branch
/// that tracks nothing.
///
/// No `-N` cap, unlike [`recent_commits`]: "how much have I not pushed"
/// is a number the user wants exactly right, and truncating it to a
/// screenful would make the header's count a lie.
fn unmerged_commits(repo: &Repository) -> Vec<(String, String)> {
    let output = repo
        .run_git_str(["log", "--format=%h %s", "@{upstream}..HEAD"])
        .unwrap_or_default();
    parse_oneline_log(&output)
}

fn recent_commits(repo: &Repository) -> Vec<(String, String)> {
    let output = repo
        .run_git_str(["log", "--oneline", "-20", "--format=%h %s"])
        .unwrap_or_default();
    parse_oneline_log(&output)
}

/// `<sha> <subject>` per line. Shared by both commit sections so the
/// two cannot drift into parsing the same `git log` output differently.
fn parse_oneline_log(output: &str) -> Vec<(String, String)> {
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
    apply_and_highlight_refined(
        handle,
        text,
        spans,
        Vec::new(),
        pending_highlights,
        buffer_id,
    )
    .await
}

/// DR.3: as [`apply_and_highlight`], publishing intra-line refinement
/// with the spans — one update, so the two cannot drift when an inline
/// expansion shifts lines.
pub async fn apply_and_highlight_refined(
    handle: Arc<dyn Document>,
    text: String,
    spans: Vec<Vec<StyledSpan>>,
    refine: Vec<Vec<lattice_cells::RefineSpan>>,
    pending_highlights: Option<PendingSyntheticHighlightsHandle>,
    buffer_id: BufferId,
) {
    crate::buffer_io::replace_buffer_text(&handle, text).await;
    if let Some(ref ph) = pending_highlights {
        ph.store_refined_and_wake(buffer_id, spans, refine);
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
            untracked: false,
            original_path: None,
        })
    }

    /// A repo whose modified file is Rust, so the diff has something
    /// for a grammar to colour.
    fn repo_with_an_unstaged_rust_change() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("a.rs"), "fn main() {\n    let old = 1;\n}\n").unwrap();
        git_ok(p, &["add", "a.rs"]);
        git_ok(p, &["commit", "-m", "base"]);
        std::fs::write(p.join("a.rs"), "fn main() {\n    let new = 2;\n}\n").unwrap();
        dir
    }

    fn rust_open_key() -> String {
        crate::actions::entry_key(&crate::actions::StatusLine::File {
            path: PathBuf::from("a.rs"),
            staged: false,
            untracked: false,
            original_path: None,
        })
    }

    /// DS-fix (2026-08-12): THE regression. A refresh rebuilt every open
    /// expansion with the FLAT classifier, so the syntax layer DS.1–DS.5
    /// added silently vanished on `gr` — diff colouring stayed, token
    /// colour did not.
    ///
    /// The absent assertion is what let it through: `sections.rs` called
    /// `highlight::diff_styled_spans` directly while the `=` toggle went
    /// through `hunk_syntax::diff_spans`, and nothing compared the two
    /// routes. This does.
    #[test]
    fn a_refreshed_expansion_keeps_its_syntax_highlighting() {
        let dir = repo_with_an_unstaged_rust_change();
        let wd = dir.path().to_path_buf();
        let open: HashSet<String> = [rust_open_key()].into_iter().collect();
        let registry = lattice_syntax::LangRegistry::standard().expect("standard registry");

        let (_text, with_syntax, _h, _r, _) = build_and_format(&wd, &open, 3, Some(&registry));
        let (_text, flat, _h, _r, _) = build_and_format(&wd, &open, 3, None);

        // The flat route is what the bug shipped; it must still be
        // reachable (a harness without grammars), just not the default.
        let count = |rows: &Vec<Vec<StyledSpan>>| rows.iter().map(|r| r.len()).sum::<usize>();
        assert!(
            count(&with_syntax) > count(&flat),
            "a registry must add the syntax layer: {} spans with, {} without",
            count(&with_syntax),
            count(&flat),
        );
    }

    /// The invariant `sections.rs`'s own comment claimed and the code
    /// stopped honouring: an expansion looks the same however it got
    /// there. Compares the refresh route against the `=`-toggle route
    /// on the same diff text.
    #[test]
    fn refresh_and_toggle_produce_the_same_spans_for_one_diff() {
        let dir = repo_with_an_unstaged_rust_change();
        let wd = dir.path().to_path_buf();
        let registry = lattice_syntax::LangRegistry::standard().expect("standard registry");
        let line = crate::actions::StatusLine::File {
            path: PathBuf::from("a.rs"),
            staged: false,
            untracked: false,
            original_path: None,
        };
        let diff = crate::actions::run_show(&wd, &line, 3).expect("diff");
        let diff = diff.trim_end();

        // The `=` toggle's route.
        let toggle = crate::hunk_syntax::diff_spans(diff, Some(&registry));

        // The refresh route, sliced back out of the rebuilt buffer.
        let open: HashSet<String> = [rust_open_key()].into_iter().collect();
        let (text, refreshed, _h, _r, _) = build_and_format(&wd, &open, 3, Some(&registry));
        let start = text
            .lines()
            .position(|l| l.starts_with("diff --git"))
            .expect("the inlined diff is in the buffer");
        let slice: Vec<Vec<StyledSpan>> = refreshed
            .into_iter()
            .skip(start)
            .take(toggle.len())
            .collect();

        assert_eq!(
            slice, toggle,
            "an expansion must look identical however it got there"
        );
    }

    #[test]
    fn an_open_entrys_diff_comes_back_in_the_rebuilt_text() {
        let dir = repo_with_an_unstaged_change();
        let wd = dir.path().to_path_buf();

        let collapsed = build_and_format(&wd, &HashSet::new(), 3, None);
        assert!(
            !collapsed.0.contains("@@"),
            "nothing was open, so no diff is inlined:\n{}",
            collapsed.0
        );
        assert!(collapsed.3.is_empty());

        let open: HashSet<String> = [open_key()].into_iter().collect();
        let (text, spans, _, reopened, _) = build_and_format(&wd, &open, 3, None);
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

    /// Reported from real use (2026-08-03): `<Tab>` on a file with no
    /// diff hid that file AND every file below it in the section.
    ///
    /// `fold_to_close_at` picks by **containment**, so a row with no
    /// fold of its own closes whichever fold currently spans it. That is
    /// correct behaviour given a correct fold list — the bug was that
    /// the list was stale: `=` edits the buffer through the document
    /// handle, which never goes through the Editor's edit path, so
    /// nothing recomputed folds and the ranges stayed pinned to the text
    /// as it was before the diff was spliced in. A leftover range then
    /// spanned half the buffer.
    ///
    /// This pins the half that must hold for the fix to mean anything:
    /// the ranges the source emits describe the text as it is NOW, and
    /// an entry with nothing expanded emits NO fold — so once the list
    /// is fresh there is nothing for `<Tab>` on such a row to close, and
    /// it says "No fold found" instead of eating the section.
    ///
    /// (The trigger half — recomputing when the buffer changes out of
    /// band — is `Editor::refresh_overlay_folds`, wired into the tick
    /// beside `refresh_diff_folds`.)
    #[test]
    fn an_entry_with_nothing_expanded_contributes_no_fold_range() {
        let dir = repo_with_an_unstaged_change();
        let wd = dir.path().to_path_buf();

        // Nothing expanded: the source has no ranges to offer at all,
        // which is what leaves `<Tab>` on such a row with nothing to
        // close.
        let (collapsed_text, _, _, collapsed_expanded, _) =
            build_and_format(&wd, &HashSet::new(), 3, None);
        assert!(collapsed_expanded.is_empty(), "nothing is expanded");

        // Expanded: exactly one entry gains a range, and it must stop at
        // that entry's own diff rather than running on into the rows
        // below — the stale-range shape the report describes.
        let open: HashSet<String> = [open_key()].into_iter().collect();
        let (text, _, _, expanded, _) = build_and_format(&wd, &open, 3, None);
        let count = expanded
            .get(&open_key())
            .copied()
            .expect("the open entry records its row count");
        let added = text.lines().count() - collapsed_text.lines().count();
        assert_eq!(
            count, added,
            "the fold body is exactly the rows the expansion added — a \
             count larger than that is precisely the range that swallows \
             the entries below it"
        );

        // And the body really is this file's diff, not the next entry.
        let header = text
            .lines()
            .position(|l| l.contains("a.txt"))
            .expect("the entry row");
        let body: Vec<&str> = text.lines().skip(header + 1).take(count).collect();
        assert!(
            body.iter().any(|l| l.starts_with("@@")),
            "the folded body is the diff:\n{body:#?}"
        );
        assert!(
            !body
                .iter()
                .any(|l| l.trim_start().starts_with("Untracked") || l.contains("Recent commits")),
            "and it stops before the next section:\n{body:#?}"
        );
    }

    /// The entry key the refresh matches on must be the one `=` writes,
    /// or an expansion would silently fail to come back.
    #[test]
    fn the_rebuilt_key_is_the_one_the_toggle_uses() {
        let dir = repo_with_an_unstaged_change();
        let open: HashSet<String> = [open_key()].into_iter().collect();
        let (_, _, _, reopened, _) = build_and_format(&dir.path().to_path_buf(), &open, 3, None);
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
        let (text, _, _, reopened, _) =
            build_and_format(&dir.path().to_path_buf(), &stale, 3, None);
        assert!(!text.contains("@@"), "nothing inlined:\n{text}");
        assert!(reopened.is_empty(), "the stale key is dropped, not carried");
    }
}

#[cfg(test)]
mod unmerged_into_upstream {
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

    /// A repo whose branch tracks a bare "remote", with `ahead`
    /// commits made after the last push.
    ///
    /// A real bare remote rather than a stub: `@{upstream}` is resolved
    /// by git itself, and a fake would prove only that our own fake
    /// works.
    fn repo_tracking_upstream(ahead: usize) -> (tempfile::TempDir, tempfile::TempDir) {
        let remote = tempfile::tempdir().expect("tempdir");
        git_ok(remote.path(), &["init", "--bare"]);

        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("a.txt"), "base\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        git_ok(p, &["branch", "-M", "main"]);
        let remote_url = remote.path().to_string_lossy().into_owned();
        git_ok(p, &["remote", "add", "origin", &remote_url]);
        git_ok(p, &["push", "-u", "origin", "main"]);

        for i in 0..ahead {
            std::fs::write(p.join("a.txt"), format!("change {i}\n")).unwrap();
            git_ok(p, &["add", "a.txt"]);
            git_ok(p, &["commit", "-m", &format!("local change {i}")]);
        }
        (dir, remote)
    }

    fn kinds(index: &SectionIndex) -> Vec<SectionKind> {
        index.sections.iter().map(|s| s.kind).collect()
    }

    /// The reported behaviour: commits the upstream does not have get
    /// their own section, named for the upstream.
    #[test]
    fn unpushed_commits_appear_under_the_upstream_name() {
        let (dir, _remote) = repo_tracking_upstream(3);
        let repo = Repository::discover(dir.path()).expect("repo");
        let index = build_section_index(&repo);

        assert_eq!(index.upstream.as_deref(), Some("origin/main"));
        let unmerged = index
            .sections
            .iter()
            .find(|s| s.kind == SectionKind::Unmerged)
            .expect("an unmerged section");
        assert_eq!(unmerged.entries.len(), 3, "one row per unpushed commit");

        let rendered = index.format_buffer();
        assert!(
            rendered.contains("Unmerged into origin/main (3)"),
            "header names the upstream and counts: {rendered}"
        );
    }

    /// The either/or rule: recent commits are shown ONLY when there is
    /// nothing unpushed. Listing both would repeat the same commits
    /// under two headings.
    #[test]
    fn recent_commits_are_hidden_while_anything_is_unmerged() {
        let (dir, _remote) = repo_tracking_upstream(2);
        let repo = Repository::discover(dir.path()).expect("repo");
        let k = kinds(&build_section_index(&repo));
        assert!(k.contains(&SectionKind::Unmerged), "got {k:?}");
        assert!(
            !k.contains(&SectionKind::RecentCommits),
            "recent commits must yield to unmerged: {k:?}"
        );
    }

    /// And they come back once everything is pushed — the transition
    /// the user described, and the half that a one-way test would miss.
    #[test]
    fn recent_commits_return_once_everything_is_pushed() {
        let (dir, _remote) = repo_tracking_upstream(2);
        git_ok(dir.path(), &["push", "origin", "main"]);
        let repo = Repository::discover(dir.path()).expect("repo");
        let k = kinds(&build_section_index(&repo));
        assert!(
            !k.contains(&SectionKind::Unmerged),
            "nothing is unpushed now: {k:?}"
        );
        assert!(
            k.contains(&SectionKind::RecentCommits),
            "recent commits come back: {k:?}"
        );
    }

    /// A branch with no upstream is an ordinary state, not an error:
    /// no unmerged section, and recent commits render as they always
    /// did.
    #[test]
    fn a_branch_without_an_upstream_shows_recent_commits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("a.txt"), "base\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);

        let repo = Repository::discover(p).expect("repo");
        let index = build_section_index(&repo);
        assert_eq!(index.upstream, None);
        let k = kinds(&index);
        assert!(!k.contains(&SectionKind::Unmerged), "got {k:?}");
        assert!(k.contains(&SectionKind::RecentCommits), "got {k:?}");
    }

    #[test]
    fn the_log_parser_splits_sha_from_subject() {
        let parsed = parse_oneline_log("abc1234 fix the thing\ndef5678 another\n");
        assert_eq!(
            parsed,
            vec![
                ("abc1234".to_string(), "fix the thing".to_string()),
                ("def5678".to_string(), "another".to_string()),
            ]
        );
    }
}
