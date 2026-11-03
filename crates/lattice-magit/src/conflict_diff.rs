//! 3W.4: diffing a conflicted file opens it for resolution.
//!
//! `dd` and `dv` on a file git stopped a merge on would otherwise show
//! git's combined `diff --cc` (`dd`) or fail to read the index copy
//! (`dv`: an unmerged path has no stage 0). What the user wants there is
//! the vim-fugitive `:Gdiffsplit!` view: ours | working | theirs, the
//! working file between git's copies of the two sides, resolved with
//! `d2o` / `d3o` / `dB`. `lattice-diff` owns that session
//! (`ConflictLayout::Resolve`) and the host lays it out
//! (`Effect::DiffResolve`); this module decides, from what the status
//! row says, which copies exist and so which view to open.

use std::path::Path;

use lattice_grammar::{EchoLevel, Effect};
use lattice_vcs::UnmergedKind;

use crate::magit_file_revision_mode::{ConflictStage, MagitFileRevisionMode, blob_buffer_name};

/// The conflict a magit buffer shows for `path`, if any.
///
/// A status row's label is git's own wording for the conflict ("both
/// modified", "deleted by them"), the same fact `untracked` is read from.
/// A diff buffer has no labels, but git prints a conflicted path's diff
/// as `diff --cc <path>`, which says there is a conflict without saying
/// which kind, so it reads as [`UnmergedKind::Other`]: both copies are
/// tried, and a missing one says so in its pane.
pub(crate) fn conflict_of<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    path: &Path,
) -> Option<UnmergedKind> {
    let path = path.to_string_lossy();
    let combined = format!("diff --cc {path}");
    lines.into_iter().find_map(|line| {
        if line == combined {
            return Some(UnmergedKind::Other);
        }
        let rest = line.strip_prefix("  ")?;
        UnmergedKind::ALL.into_iter().find(|kind| {
            rest.strip_prefix(kind.label())
                .filter(|r| r.starts_with(char::is_whitespace))
                .is_some_and(|r| r.trim_start() == path)
        })
    })
}

/// Which of git's copies a conflict of `kind` has: `(ours, theirs)`.
fn sides(kind: UnmergedKind) -> (bool, bool) {
    match kind {
        UnmergedKind::BothModified | UnmergedKind::BothAdded | UnmergedKind::Other => (true, true),
        UnmergedKind::AddedByUs | UnmergedKind::DeletedByThem => (true, false),
        UnmergedKind::AddedByThem | UnmergedKind::DeletedByUs => (false, true),
        UnmergedKind::BothDeleted => (false, false),
    }
}

/// The effect that opens `path` for resolution.
///
/// - **Both sides** (`both modified`, `both added`): ours | working |
///   theirs. The copies are opened first, because `Effect::DiffResolve`
///   names buffers rather than files; magit-file-revision-mode fills each
///   from git's index.
/// - **One side** (`added by us`, `deleted by them`, and the mirror
///   pair): the two-pane diff `dv` gives anywhere else, against the one
///   copy there is. The other side is not missing information, it is
///   the conflict: one branch has the file and the other does not.
/// - **Neither** (`both deleted`), or no working file: an echo, since
///   there is nothing to lay beside anything.
pub(crate) fn conflict_diff_effect(
    repo: &str,
    workdir: &Path,
    path: &Path,
    kind: UnmergedKind,
) -> Effect {
    let working = workdir.join(path);
    let name = |stage: ConflictStage| blob_buffer_name(repo, stage.token(), path);
    let open = |stage: ConflictStage| Effect::OpenSyntheticBuffer {
        name: name(stage),
        mode_id: MagitFileRevisionMode::mode_id().to_string(),
        content: None,
        cursor: None,
        activate_minor: None,
    };
    match sides(kind) {
        (false, false) => Effect::Echo {
            level: EchoLevel::Info,
            text: format!(
                "{} was deleted on both sides; there is nothing to diff",
                path.display()
            ),
        },
        _ if !working.exists() => Effect::Echo {
            level: EchoLevel::Warn,
            text: format!("{} has no working-tree copy to resolve", path.display()),
        },
        // Theirs first, so ours is what the pane shows when the layout
        // starts from it.
        (true, true) => Effect::Many(vec![
            open(ConflictStage::Theirs),
            open(ConflictStage::Ours),
            Effect::DiffResolve {
                ours: name(ConflictStage::Ours),
                working,
                theirs: name(ConflictStage::Theirs),
            },
        ]),
        (true, false) => crate::magit_hunk_mode::side_by_side_effects(
            repo,
            ConflictStage::Ours.token(),
            path,
            working,
        ),
        (false, true) => crate::magit_hunk_mode::side_by_side_effects(
            repo,
            ConflictStage::Theirs.token(),
            path,
            working,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_status_row_names_its_conflict() {
        let rows = [
            "Unstaged changes (2)",
            "  modified        src/lib.rs",
            "  both modified   src/main.rs",
            "  deleted by them src/gone.rs",
        ];
        assert_eq!(
            conflict_of(rows, Path::new("src/main.rs")),
            Some(UnmergedKind::BothModified)
        );
        assert_eq!(
            conflict_of(rows, Path::new("src/gone.rs")),
            Some(UnmergedKind::DeletedByThem)
        );
        assert_eq!(conflict_of(rows, Path::new("src/lib.rs")), None);
    }

    /// The path has to match the whole row: `main.rs` is not
    /// `src/main.rs`'s conflict.
    #[test]
    fn a_row_for_another_path_is_not_this_ones() {
        let rows = ["  both modified   src/main.rs"];
        assert_eq!(conflict_of(rows, Path::new("main.rs")), None);
    }

    /// A diff buffer has no labels, but git's combined-diff header says
    /// the path is conflicted.
    #[test]
    fn a_combined_diff_header_is_a_conflict_of_unknown_kind() {
        let lines = ["diff --cc a.txt", "index 1,2..3", "@@@ -1,1 -1,1 +1,5 @@@"];
        assert_eq!(
            conflict_of(lines, Path::new("a.txt")),
            Some(UnmergedKind::Other)
        );
    }

    fn effect(kind: UnmergedKind) -> (Effect, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "<<<<<<< HEAD\n").unwrap();
        (
            conflict_diff_effect("repo", dir.path(), Path::new("a.txt"), kind),
            dir,
        )
    }

    /// Both sides: the two copies open, then the three-pane layout names
    /// them, in that order, because `DiffResolve` looks them up by name.
    #[test]
    fn both_sides_open_their_copies_then_the_resolution_layout() {
        let (effect, dir) = effect(UnmergedKind::BothModified);
        let Effect::Many(parts) = effect else {
            panic!("expected Many, got {effect:?}");
        };
        let opened: Vec<String> = parts[..2]
            .iter()
            .map(|e| match e {
                Effect::OpenSyntheticBuffer { name, .. } => name.clone(),
                other => panic!("expected a copy opening, got {other:?}"),
            })
            .collect();
        match &parts[2] {
            Effect::DiffResolve {
                ours,
                working,
                theirs,
            } => {
                assert!(opened.contains(ours) && opened.contains(theirs));
                assert!(ours.contains("[ours]") && theirs.contains("[theirs]"));
                assert_eq!(working, &dir.path().join("a.txt"));
            }
            other => panic!("expected DiffResolve, got {other:?}"),
        }
    }

    /// One side: a two-pane diff against the copy that exists.
    #[test]
    fn one_side_diffs_against_the_copy_there_is() {
        for (kind, token) in [
            (UnmergedKind::DeletedByThem, "[ours]"),
            (UnmergedKind::AddedByThem, "[theirs]"),
        ] {
            let (effect, _dir) = effect(kind);
            let Effect::Many(parts) = effect else {
                panic!("expected Many for {kind:?}");
            };
            assert!(
                matches!(&parts[0], Effect::OpenSyntheticBuffer { name, .. } if name.contains(token)),
                "{kind:?} opens {token}: {parts:?}"
            );
            assert!(matches!(parts[1], Effect::Diffsplit { remote: None, .. }));
        }
    }

    #[test]
    fn both_deleted_and_a_missing_working_file_echo_instead() {
        let (effect, _dir) = effect(UnmergedKind::BothDeleted);
        assert!(matches!(effect, Effect::Echo { .. }), "{effect:?}");
        let gone = conflict_diff_effect(
            "repo",
            &PathBuf::from("/nonexistent"),
            Path::new("a.txt"),
            UnmergedKind::BothModified,
        );
        assert!(
            matches!(&gone, Effect::Echo { text, .. } if text.contains("no working-tree copy")),
            "{gone:?}"
        );
    }
}
