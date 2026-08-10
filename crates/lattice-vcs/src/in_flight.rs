//! Which multi-commit operation the repository is stopped in the
//! middle of.
//!
//! Git records every one of these as a marker in the gitdir and
//! removes it on `--continue` / `--abort` / `--quit`, so the marker's
//! presence IS the state. Nothing is cached and nothing is parsed:
//! a cached flag can go stale behind our back when the user runs git
//! in a terminal, which for "am I mid-rebase?" is the difference
//! between offering `--continue` and offering nonsense.
//!
//! This lives in `lattice-vcs` rather than in the magit UI because
//! two consumers need the same answer and had been asking it
//! separately: the transient menus (to decide whether to offer the
//! way IN or the ways OUT) and the status buffer's headerline (to say
//! what the user is in the middle of at all). Two detectors for one
//! fact is how they drift.

use std::path::Path;

use crate::Repository;

/// A multi-commit operation stopped mid-flight.
///
/// Ordered by specificity in [`InFlightOp::detect`]: `git am` and
/// rebase share the `rebase-apply` directory, so the more specific
/// marker is tested first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InFlightOp {
    /// A merge stopped with conflicts — `MERGE_HEAD`.
    Merge,
    /// A rebase stopped at a conflict or an `edit` stop —
    /// `rebase-merge/` (the default backend) or `rebase-apply/` (the
    /// legacy `--apply` backend).
    Rebase,
    /// A cherry-pick sequence stopped — `CHERRY_PICK_HEAD`.
    CherryPick,
    /// A revert sequence stopped — `REVERT_HEAD`.
    Revert,
    /// `git am` stopped applying a patch — `rebase-apply/applying`.
    ApplyPatch,
}

impl InFlightOp {
    /// The headerline alert for this state, in the wording git itself
    /// uses when it tells the user what to do next.
    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "MERGING",
            Self::Rebase => "REBASING",
            Self::CherryPick => "CHERRY-PICKING",
            Self::Revert => "REVERTING",
            Self::ApplyPatch => "APPLYING",
        }
    }

    /// Which side of a conflict "us" and "them" name during this
    /// operation — and they INVERT between the two families.
    ///
    /// In a merge, "us" is the branch you are on and "them" is the
    /// branch being merged in, which is what everyone expects. In a
    /// rebase, cherry-pick, revert or `am`, git replays your work ONTO
    /// the other side, so "us" is the *upstream* being replayed onto
    /// and "them" is *your own commit*.
    ///
    /// This is the single most confusing thing about resolving a
    /// conflict mid-rebase, and it is why the unmerged labels
    /// ("added by us", "deleted by them") are ambiguous without
    /// knowing which operation produced them.
    pub fn ours_is_local(self) -> bool {
        matches!(self, Self::Merge)
    }

    /// Every variant, for exhaustive tests and label tables.
    pub const ALL: [Self; 5] = [
        Self::Merge,
        Self::Rebase,
        Self::CherryPick,
        Self::Revert,
        Self::ApplyPatch,
    ];

    /// Detect the operation in flight, or `None` when the repository
    /// is idle.
    ///
    /// At most one is reported. They are not strictly exclusive in
    /// git's data model — a rebase that hits a conflict while
    /// cherry-picking a commit writes `CHERRY_PICK_HEAD` too — so the
    /// order matters: the *outer* operation is the one the user has to
    /// finish, and the one whose `--continue` they need.
    pub fn detect(repo: &Repository) -> Option<Self> {
        Self::detect_in(repo.gitdir())
    }

    /// [`Self::detect`] against a gitdir path directly, so it is
    /// testable without a live repository.
    pub fn detect_in(gitdir: &Path) -> Option<Self> {
        // `git am` before rebase: both use `rebase-apply/`, and only
        // the `applying` file distinguishes them.
        if gitdir.join("rebase-apply").join("applying").exists() {
            return Some(Self::ApplyPatch);
        }
        // Rebase before cherry-pick/revert: an interactive rebase that
        // stops on a conflict leaves `CHERRY_PICK_HEAD` behind as an
        // implementation detail of how it replays commits. Reporting
        // "CHERRY-PICKING" there would send the user to
        // `cherry-pick --continue`, which is not what finishes a
        // rebase.
        if gitdir.join("rebase-merge").exists() || gitdir.join("rebase-apply").exists() {
            return Some(Self::Rebase);
        }
        if gitdir.join("MERGE_HEAD").exists() {
            return Some(Self::Merge);
        }
        if gitdir.join("CHERRY_PICK_HEAD").exists() {
            return Some(Self::CherryPick);
        }
        if gitdir.join("REVERT_HEAD").exists() {
            return Some(Self::Revert);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gitdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn touch(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(p, b"").expect("write");
    }

    #[test]
    fn an_idle_repository_reports_nothing() {
        let d = gitdir();
        assert_eq!(InFlightOp::detect_in(d.path()), None);
    }

    #[test]
    fn each_marker_is_recognised() {
        for (marker, expected) in [
            ("MERGE_HEAD", InFlightOp::Merge),
            ("CHERRY_PICK_HEAD", InFlightOp::CherryPick),
            ("REVERT_HEAD", InFlightOp::Revert),
        ] {
            let d = gitdir();
            touch(d.path(), marker);
            assert_eq!(
                InFlightOp::detect_in(d.path()),
                Some(expected),
                "{marker} means {}",
                expected.label()
            );
        }
        // Both rebase backends.
        for dir in ["rebase-merge", "rebase-apply"] {
            let d = gitdir();
            std::fs::create_dir_all(d.path().join(dir)).expect("mkdir");
            assert_eq!(InFlightOp::detect_in(d.path()), Some(InFlightOp::Rebase));
        }
    }

    /// `git am` and the legacy rebase backend share `rebase-apply/`;
    /// only the `applying` file tells them apart, so it is tested
    /// first.
    #[test]
    fn am_is_distinguished_from_the_legacy_rebase_backend() {
        let d = gitdir();
        touch(d.path(), "rebase-apply/applying");
        assert_eq!(
            InFlightOp::detect_in(d.path()),
            Some(InFlightOp::ApplyPatch)
        );
    }

    /// An interactive rebase that stops on a conflict leaves
    /// `CHERRY_PICK_HEAD` behind — an implementation detail of how it
    /// replays commits. Reporting "CHERRY-PICKING" would send the user
    /// to `cherry-pick --continue`, which does not finish a rebase.
    #[test]
    fn a_rebase_carrying_cherry_pick_head_is_still_a_rebase() {
        let d = gitdir();
        std::fs::create_dir_all(d.path().join("rebase-merge")).expect("mkdir");
        touch(d.path(), "CHERRY_PICK_HEAD");
        assert_eq!(InFlightOp::detect_in(d.path()), Some(InFlightOp::Rebase));
    }

    /// "us" and "them" invert between a merge and everything that
    /// replays commits — the single most confusing thing about
    /// resolving a conflict mid-rebase.
    #[test]
    fn ours_is_local_only_for_a_merge() {
        assert!(InFlightOp::Merge.ours_is_local());
        for op in [
            InFlightOp::Rebase,
            InFlightOp::CherryPick,
            InFlightOp::Revert,
            InFlightOp::ApplyPatch,
        ] {
            assert!(
                !op.ours_is_local(),
                "{} replays onto the other side, so \"us\" is the upstream",
                op.label()
            );
        }
    }

    #[test]
    fn every_op_has_a_distinct_label() {
        let labels: std::collections::HashSet<&str> =
            InFlightOp::ALL.iter().map(|o| o.label()).collect();
        assert_eq!(labels.len(), InFlightOp::ALL.len());
    }
}
