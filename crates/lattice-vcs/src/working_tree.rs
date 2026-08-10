use std::path::{Path, PathBuf};

use crate::{Repository, Result, VcsError};

/// One axis of a path's change, as git reports it.
///
/// Deliberately does NOT include a "clean" or "both staged and
/// unstaged" value: those are properties of the pair, not of one
/// axis, and [`PathChange`] expresses them (`None` and `Some` on both
/// sides respectively). Encoding them here is what produced the
/// original bug — a staged modification had to claim to be `Added`
/// so the consumer would file it in the right section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathStatus {
    /// Content changed. On the index axis: staged for commit. On the
    /// worktree axis: not yet staged.
    Modified,
    /// Path exists on this side but not the one it is compared against
    /// — a new file.
    Added,
    /// Path was removed.
    Deleted,
    /// Path moved from somewhere else; the origin is
    /// [`PathChange::original_path`]. Index axis only — git does not
    /// detect worktree renames.
    Renamed,
    /// Path was copied from somewhere else; origin as for
    /// [`Self::Renamed`]. Only produced when rename detection is
    /// configured to find copies (`status.renames=copies`).
    Copied,
    /// The path's TYPE changed — regular file ⇄ symlink ⇄ submodule —
    /// with or without a content change. Git spells this `T`, and
    /// dropping it (as this parser used to) makes the file vanish from
    /// the status view entirely: a change you can neither see nor
    /// stage.
    TypeChanged,
    /// File is not tracked by git. A whole-path state, not an axis.
    Untracked,
    /// File is ignored via `.gitignore`. A whole-path state.
    Ignored,
    /// File has unmerged entries — a real merge conflict, carrying
    /// WHICH of git's seven combinations it is. A whole-path state:
    /// the resolution work is in the worktree, so it is reported
    /// there.
    Unmerged(UnmergedKind),
}

/// Which unmerged combination git reported.
///
/// "Us" is the branch being merged INTO (`HEAD` / the branch you are
/// rebasing onto); "them" is the incoming side. During a rebase or
/// cherry-pick the two are inverted relative to what people expect —
/// git replays your commits ONTO the upstream, so "us" is the
/// upstream and "them" is your own commit. Naming the side is the
/// whole value of distinguishing these: "both modified" and "deleted
/// by them" call for completely different resolutions, and a generic
/// "unmerged" tells the user neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnmergedKind {
    /// `DD` — deleted on both sides.
    BothDeleted,
    /// `AU` — added by us, unmerged on their side.
    AddedByUs,
    /// `UD` — deleted by them.
    DeletedByThem,
    /// `UA` — added by them.
    AddedByThem,
    /// `DU` — deleted by us.
    DeletedByUs,
    /// `AA` — added on both sides.
    BothAdded,
    /// `UU` — modified on both sides. The ordinary conflict.
    BothModified,
    /// A `U` combination git documents no name for. Kept rather than
    /// guessed at: reporting it as one of the seven would be a lie,
    /// and dropping it would make the path invisible in the status —
    /// the same silent omission `T` used to have.
    Other,
}

impl UnmergedKind {
    /// Git's own wording for this combination, as `git status` prints
    /// it in the long format.
    pub fn label(self) -> &'static str {
        match self {
            Self::BothDeleted => "both deleted",
            Self::AddedByUs => "added by us",
            Self::DeletedByThem => "deleted by them",
            Self::AddedByThem => "added by them",
            Self::DeletedByUs => "deleted by us",
            Self::BothAdded => "both added",
            Self::BothModified => "both modified",
            Self::Other => "unmerged",
        }
    }

    /// Every named combination, for exhaustive tests and label tables.
    pub const ALL: [Self; 8] = [
        Self::BothDeleted,
        Self::AddedByUs,
        Self::DeletedByThem,
        Self::AddedByThem,
        Self::DeletedByUs,
        Self::BothAdded,
        Self::BothModified,
        Self::Other,
    ];
}

/// A path's porcelain `XY` status, kept as the TWO independent axes git
/// reports rather than collapsed into one value.
///
/// `X` is the index vs HEAD ("what a commit would record") and `Y` is
/// the working tree vs the index ("what staging would add"). They vary
/// independently: `MM` means a file has staged changes *and* further
/// unstaged ones, and it belongs in both of magit's sections with
/// "modified" on each row.
///
/// Collapsing them into a single [`PathStatus`] is what produced the
/// reported bug — a staged modification (`M `) had to be reported as
/// `Added` to make it land in the staged section, so it rendered as
/// "new file", and `MM` had to be reported as `Conflicted` (which means
/// a merge conflict) purely to make it appear in both. One value cannot
/// answer both "which section" and "what label" without lying about one
/// of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathChange {
    /// Index vs HEAD. `None` when nothing is staged for this path.
    pub staged: Option<PathStatus>,
    /// Working tree vs index. `None` when the worktree matches the
    /// index. Also carries [`PathStatus::Untracked`] / `Ignored` /
    /// `Unmerged`, which are whole-path states rather than one axis.
    pub unstaged: Option<PathStatus>,
    /// Where a [`PathStatus::Renamed`] / [`PathStatus::Copied`] path
    /// came from. `None` for every other status.
    pub original_path: Option<PathBuf>,
}

impl PathChange {
    /// Nothing staged, nothing modified.
    pub const CLEAN: Self = Self {
        staged: None,
        unstaged: None,
        original_path: None,
    };

    /// `true` when git reports no change on either axis.
    pub fn is_clean(&self) -> bool {
        self.staged.is_none() && self.unstaged.is_none()
    }
}

/// Working tree and index status operations.
///
/// Uses `git status --porcelain=v1` for status classification.
pub struct WorkingTree;

impl WorkingTree {
    /// Return the status of a single path in the repository.
    ///
    /// Uses `git status --porcelain=v1 -- <path>`.
    pub fn path_status(repo: &Repository, path: impl AsRef<Path>) -> Result<PathChange> {
        let path = path.as_ref();
        let output =
            // `-z` for the same reason as `statuses` — a non-ASCII
            // path would otherwise come back quoted and escaped.
            repo.run_git_str(["status", "--porcelain=v1", "-z", "--", &path.to_string_lossy()])?;

        if output.trim().is_empty() {
            // File might be tracked but clean, or nonexistent.
            // Check if it's tracked at all.
            match repo.run_git_str(["ls-files", "--error-unmatch", "--", &path.to_string_lossy()]) {
                Ok(_) => Ok(PathChange::CLEAN),
                Err(_) => Err(VcsError::StatusParse(format!(
                    "path not in repository: {}",
                    path.display()
                ))),
            }
        } else {
            let record = output.split('\0').find(|r| !r.is_empty()).unwrap_or("");
            Ok(parse_porcelain_line(record))
        }
    }

    /// Return the status of every file in the working tree that has
    /// changed relative to the index or HEAD.
    ///
    /// Includes untracked files. Uses `git status --porcelain=v1`.
    pub fn statuses(repo: &Repository) -> Result<Vec<(PathBuf, PathChange)>> {
        // `-z` is not an optimisation, it is the only correct form.
        //
        // Without it git QUOTES any path that needs escaping, and
        // `core.quotepath` defaults to on — so every non-ASCII filename
        // comes back as `"\303\251t\303\251.txt"`, quotes and octal
        // escapes included, and the parsed `PathBuf` names a file that
        // does not exist. Renames also arrive as `old -> new` in one
        // field, which cannot be split unambiguously: ` -> ` is legal in
        // a filename.
        //
        // With `-z`, records are NUL-terminated, paths are verbatim, and
        // a rename/copy is TWO records — the new path, then the original
        // (note the order is the reverse of the ` -> ` form).
        let output = repo.run_git_str(["status", "--porcelain=v1", "-z"])?;
        let mut results = Vec::new();
        let mut records = output.split('\0').filter(|r| !r.is_empty());

        while let Some(record) = records.next() {
            // "XY " plus at least one byte of path.
            if record.len() < 4 {
                continue;
            }
            let change = parse_porcelain_line(record);
            // Byte 3 is always a char boundary: `XY` and the separator
            // are ASCII.
            let path = PathBuf::from(&record[3..]);
            // Only the INDEX axis carries rename/copy detection in
            // porcelain v1, so only an `R`/`C` in column X means git
            // emitted an extra origin record. Keying off column Y as
            // well would swallow the next file's record whenever git
            // did not emit one.
            let original_path = match record.chars().next() {
                Some('R' | 'C') => records.next().map(PathBuf::from),
                _ => None,
            };
            results.push((
                path,
                PathChange {
                    original_path,
                    ..change
                },
            ));
        }

        Ok(results)
    }
}

/// Parse a single `git status --porcelain=v1` line into a [`PathChange`].
///
/// Porcelain format: `XY PATH` where X is the index status and Y is the
/// working-tree status. The two are decoded SEPARATELY — see
/// [`PathChange`] for why collapsing them is a bug rather than a
/// simplification.
/// An unmerged path, on the worktree axis.
fn unmerged(kind: UnmergedKind) -> PathChange {
    PathChange {
        unstaged: Some(PathStatus::Unmerged(kind)),
        ..PathChange::CLEAN
    }
}

fn parse_porcelain_line(line: &str) -> PathChange {
    let chars: Vec<char> = line.chars().take(2).collect();
    if chars.len() < 2 {
        return PathChange::CLEAN;
    }
    let x = chars[0]; // index (staging area) status
    let y = chars[1]; // working tree status

    // Whole-path states first: these are not per-axis, and git spells
    // them with the same char in both columns.
    match (x, y) {
        ('?', '?') => {
            return PathChange {
                unstaged: Some(PathStatus::Untracked),
                ..PathChange::CLEAN
            };
        }
        ('!', '!') => {
            return PathChange {
                unstaged: Some(PathStatus::Ignored),
                ..PathChange::CLEAN
            };
        }
        // Unmerged. Git documents exactly seven combinations; each is
        // decoded by name because they call for different resolutions
        // ("both modified" and "deleted by them" are not the same
        // problem). Reported on the unstaged axis because the
        // resolution work is in the worktree.
        ('D', 'D') => return unmerged(UnmergedKind::BothDeleted),
        ('A', 'U') => return unmerged(UnmergedKind::AddedByUs),
        ('U', 'D') => return unmerged(UnmergedKind::DeletedByThem),
        ('U', 'A') => return unmerged(UnmergedKind::AddedByThem),
        ('D', 'U') => return unmerged(UnmergedKind::DeletedByUs),
        ('A', 'A') => return unmerged(UnmergedKind::BothAdded),
        ('U', 'U') => return unmerged(UnmergedKind::BothModified),
        // Any other `U` pairing: still unmerged, still visible, but not
        // claimed to be one of the seven.
        ('U', _) | (_, 'U') => return unmerged(UnmergedKind::Other),
        _ => {}
    }

    // Index axis (vs HEAD).
    let staged = match x {
        'M' => Some(PathStatus::Modified),
        'A' => Some(PathStatus::Added),
        'D' => Some(PathStatus::Deleted),
        'R' => Some(PathStatus::Renamed),
        'C' => Some(PathStatus::Copied),
        'T' => Some(PathStatus::TypeChanged),
        _ => None,
    };

    // Worktree axis (vs index). Git does not detect worktree renames,
    // so `R`/`C` cannot appear here.
    let unstaged = match y {
        'M' => Some(PathStatus::Modified),
        'D' => Some(PathStatus::Deleted),
        'A' => Some(PathStatus::Added),
        'T' => Some(PathStatus::TypeChanged),
        _ => None,
    };

    PathChange {
        staged,
        unstaged,
        original_path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xy: &str) -> PathChange {
        parse_porcelain_line(&format!("{xy} some/path.txt"))
    }

    /// The porcelain table, pinned per axis.
    ///
    /// Reported 2026-08-10: a staged modification rendered as "new
    /// file" in magit's Staged-changes section. `('M', ' ')` was mapped
    /// to `PathStatus::Added` — not a typo, but the only way to make a
    /// single collapsed value land the row in the staged section, since
    /// magit's refresh chose the section FROM that value. The label was
    /// the price. `MM` had the same shape: reported as `Conflicted`
    /// (which means a merge conflict) purely to appear in both
    /// sections.
    ///
    /// Both axes are decoded separately now, so "which section" and
    /// "what label" stop fighting over one value.
    #[test]
    fn porcelain_xy_decodes_both_axes_independently() {
        // The reported case: staged modification. Staged, and MODIFIED
        // — not added.
        assert_eq!(
            parse("M "),
            PathChange {
                staged: Some(PathStatus::Modified),
                unstaged: None,
                ..PathChange::CLEAN
            }
        );
        // Unstaged modification.
        assert_eq!(
            parse(" M"),
            PathChange {
                staged: None,
                unstaged: Some(PathStatus::Modified),
                ..PathChange::CLEAN
            }
        );
        // Both axes at once — two modifications, not a conflict.
        assert_eq!(
            parse("MM"),
            PathChange {
                staged: Some(PathStatus::Modified),
                unstaged: Some(PathStatus::Modified),
                ..PathChange::CLEAN
            }
        );
        // A genuinely new file, staged.
        assert_eq!(
            parse("A "),
            PathChange {
                staged: Some(PathStatus::Added),
                unstaged: None,
                ..PathChange::CLEAN
            }
        );
        // Staged new file with further unstaged edits.
        assert_eq!(
            parse("AM"),
            PathChange {
                staged: Some(PathStatus::Added),
                unstaged: Some(PathStatus::Modified),
                ..PathChange::CLEAN
            }
        );
        // Deletions, each side.
        assert_eq!(
            parse("D "),
            PathChange {
                staged: Some(PathStatus::Deleted),
                unstaged: None,
                ..PathChange::CLEAN
            }
        );
        assert_eq!(
            parse(" D"),
            PathChange {
                staged: None,
                unstaged: Some(PathStatus::Deleted),
                ..PathChange::CLEAN
            }
        );
        // Rename / copy are their own statuses now — they used to be
        // reported as `Added`, so a staged rename read as "new file".
        assert_eq!(parse("R ").staged, Some(PathStatus::Renamed));
        assert_eq!(parse("C ").staged, Some(PathStatus::Copied));
        // Renamed in the index AND edited in the worktree.
        assert_eq!(
            parse("RM"),
            PathChange {
                staged: Some(PathStatus::Renamed),
                unstaged: Some(PathStatus::Modified),
                ..PathChange::CLEAN
            }
        );
        // Type change (file ⇄ symlink ⇄ submodule), each axis. These
        // used to decode to `None` on both sides, which silently
        // dropped the path from the status view — a change the user
        // could neither see nor stage.
        assert_eq!(parse("T ").staged, Some(PathStatus::TypeChanged));
        assert_eq!(parse(" T").unstaged, Some(PathStatus::TypeChanged));
        assert_eq!(
            parse("TT"),
            PathChange {
                staged: Some(PathStatus::TypeChanged),
                unstaged: Some(PathStatus::TypeChanged),
                ..PathChange::CLEAN
            }
        );
        // Git never reports a worktree rename/copy, so those columns
        // stay unclaimed rather than guessing.
        assert_eq!(parse(" R").unstaged, None);
        assert_eq!(parse(" C").unstaged, None);
    }

    /// Whole-path states are not per-axis: git spells them in both
    /// columns and they describe the path, not one side of it.
    #[test]
    fn porcelain_whole_path_states() {
        assert_eq!(parse("??").unstaged, Some(PathStatus::Untracked));
        assert_eq!(parse("??").staged, None);
        assert_eq!(parse("!!").unstaged, Some(PathStatus::Ignored));
        // All SEVEN unmerged combinations, each decoded by name. They
        // call for different resolutions — "both modified" and
        // "deleted by them" are not the same problem — so collapsing
        // them into one "unmerged" tells the user nothing about what
        // to do.
        for (xy, kind) in [
            ("DD", UnmergedKind::BothDeleted),
            ("AU", UnmergedKind::AddedByUs),
            ("UD", UnmergedKind::DeletedByThem),
            ("UA", UnmergedKind::AddedByThem),
            ("DU", UnmergedKind::DeletedByUs),
            ("AA", UnmergedKind::BothAdded),
            ("UU", UnmergedKind::BothModified),
        ] {
            assert_eq!(
                parse(xy).unstaged,
                Some(PathStatus::Unmerged(kind)),
                "{xy} is {}",
                kind.label()
            );
            // Never mistaken for a staged change: the resolution work
            // is in the worktree.
            assert_eq!(parse(xy).staged, None, "{xy} claims nothing staged");
        }
        // A `U` pairing git documents no name for stays visible and is
        // not claimed to be one of the seven.
        assert_eq!(
            parse("UM").unstaged,
            Some(PathStatus::Unmerged(UnmergedKind::Other))
        );
        // Every named kind has git's own wording, and no two collide.
        let labels: std::collections::HashSet<&str> =
            UnmergedKind::ALL.iter().map(|k| k.label()).collect();
        assert_eq!(
            labels.len(),
            UnmergedKind::ALL.len(),
            "labels must be distinct"
        );
        // Too short to carry a status ⇒ nothing claimed.
        assert!(parse_porcelain_line("").is_clean());
    }
}
