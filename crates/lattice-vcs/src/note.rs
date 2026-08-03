//! MG.37: git notes — read, write, remove, prune, merge.
//!
//! The peer of [`crate::Stash`] / [`crate::Remote`] / [`crate::Submodule`]:
//! a thin, typed wrapper over the git CLI.
//!
//! **Nothing here opens an editor.** `git notes edit` and
//! `git notes add` (without `-F`/`-m`) both spawn `$EDITOR`, which
//! inside this editor means a child process waiting on a terminal that
//! is not there — the operation hangs holding a blocking-pool thread and
//! never reports either way. [`Note::set`] takes the text and pipes it
//! to `-F -` instead, which is what makes the notes buffer
//! (`magit-notes-mode`) the editor rather than `$EDITOR`.

use std::path::Path;

use crate::{Repository, Result, VcsError};

/// Git-notes operations.
pub struct Note;

/// How `git notes merge` resolves a conflict. Magit offers the same
/// set, which is git's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteMergeStrategy {
    /// Stop and leave the conflict for the user to resolve, then
    /// `--commit` or `--abort`. Git's default.
    Manual,
    Ours,
    Theirs,
    Union,
    CatSortUniq,
}

impl NoteMergeStrategy {
    /// The `--strategy=` value git expects.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Union => "union",
            Self::CatSortUniq => "cat_sort_uniq",
        }
    }

    /// Parse the name a user typed. `None` for anything else — the
    /// caller says so rather than silently falling back to `manual`,
    /// which would resolve a merge differently from what was asked.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "manual" => Some(Self::Manual),
            "ours" => Some(Self::Ours),
            "theirs" => Some(Self::Theirs),
            "union" => Some(Self::Union),
            "cat_sort_uniq" => Some(Self::CatSortUniq),
            _ => None,
        }
    }
}

impl Note {
    /// The note attached to `commit`, or `None` when there is none.
    ///
    /// `git notes show` exits non-zero for a commit with no note, which
    /// is the ordinary case rather than a failure — hence `Option`, not
    /// `Err`. A caller opening an edit buffer wants an empty buffer
    /// there, not an error.
    pub fn show(repo: &Repository, commit: &str) -> Option<String> {
        repo.run_git_str(["notes", "show", commit]).ok()
    }

    /// Write `text` as `commit`'s note, replacing any existing one.
    ///
    /// `-F -` reads the note from stdin, the same seam
    /// [`crate::Index::apply_patch`] uses and for the same reasons: a
    /// temp file leaks on a crash and races a concurrent magit in the
    /// same repository. (It also does not work here — a first attempt
    /// wrote one and git reported "could not open or read" it.)
    ///
    /// `--force` because this is "set", not "add": the buffer was seeded
    /// with the existing note, so refusing to overwrite would refuse
    /// every edit after the first.
    pub fn set(repo: &Repository, commit: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            // An empty buffer means "no note", and `git notes add -F` on
            // empty input errors. Removing is the honest translation —
            // and it is what the user asked for by clearing the buffer.
            return Self::remove(repo, commit);
        }
        repo.run_git_stdin(
            ["notes", "add", "--force", "-F", "-", commit],
            text.as_bytes(),
        )
        .map(|_| ())
        .map_err(|e| VcsError::Note(format!("notes add {commit}: {e}")))
    }

    /// Remove `commit`'s note.
    ///
    /// `--ignore-missing`: removing a note that is not there is what the
    /// user asked for either way, and erroring would make "clear this
    /// buffer and save" fail on a commit that never had one.
    pub fn remove(repo: &Repository, commit: &str) -> Result<()> {
        repo.run_git(["notes", "remove", "--ignore-missing", commit])
            .map(|_| ())
            .map_err(|e| VcsError::Note(format!("notes remove {commit}: {e}")))
    }

    /// Drop notes for objects that no longer exist.
    ///
    /// `dry_run` reports what would go without removing anything —
    /// magit's `-n` on this transient, and worth having because the
    /// operation is otherwise unreviewable.
    pub fn prune(repo: &Repository, dry_run: bool) -> Result<String> {
        let mut argv: Vec<&str> = vec!["notes", "prune"];
        if dry_run {
            argv.push("--dry-run");
        }
        repo.run_git_str(argv)
            .map_err(|e| VcsError::Note(format!("notes prune: {e}")))
    }

    /// Merge the notes ref `from` into the current notes ref.
    ///
    /// With [`NoteMergeStrategy::Manual`] a conflict leaves the merge in
    /// progress; [`Self::merge_commit`] / [`Self::merge_abort`] finish
    /// it. The other strategies resolve without stopping.
    pub fn merge(repo: &Repository, from: &str, strategy: NoteMergeStrategy) -> Result<String> {
        repo.run_git_str([
            "notes",
            "merge",
            &format!("--strategy={}", strategy.as_str()),
            from,
        ])
        .map_err(|e| VcsError::Note(format!("notes merge {from}: {e}")))
    }

    /// Commit a notes merge that stopped on a conflict.
    pub fn merge_commit(repo: &Repository) -> Result<()> {
        repo.run_git(["notes", "merge", "--commit"])
            .map(|_| ())
            .map_err(|e| VcsError::Note(format!("notes merge --commit: {e}")))
    }

    /// Abandon a notes merge that stopped on a conflict.
    pub fn merge_abort(repo: &Repository) -> Result<()> {
        repo.run_git(["notes", "merge", "--abort"])
            .map(|_| ())
            .map_err(|e| VcsError::Note(format!("notes merge --abort: {e}")))
    }

    /// Is a notes merge stopped mid-flight?
    ///
    /// Git records one as `NOTES_MERGE_REF` in the gitdir. Checked so
    /// the menu can offer *commit* / *abort* only when they mean
    /// something — the same gating `magit-notes` does, and the same
    /// reason `B` bisect is gated (MG.21g): outside a merge git errors
    /// on both, so ungated rows would look actionable and fail.
    pub fn merge_in_progress(gitdir: &Path) -> bool {
        gitdir.join("NOTES_MERGE_REF").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strategy names are git's, and a typo in one is a merge that
    /// resolves differently from what was asked — so they are pinned
    /// rather than trusted to the `Display` of an enum.
    #[test]
    fn strategy_names_are_gits_own() {
        for (s, name) in [
            (NoteMergeStrategy::Manual, "manual"),
            (NoteMergeStrategy::Ours, "ours"),
            (NoteMergeStrategy::Theirs, "theirs"),
            (NoteMergeStrategy::Union, "union"),
            (NoteMergeStrategy::CatSortUniq, "cat_sort_uniq"),
        ] {
            assert_eq!(s.as_str(), name);
            assert_eq!(NoteMergeStrategy::parse(name), Some(s), "round-trips");
        }
    }

    /// An unknown strategy is refused, not silently defaulted. Falling
    /// back to `manual` would resolve the merge a different way than the
    /// user asked and give no sign it had.
    #[test]
    fn an_unknown_strategy_is_refused_rather_than_defaulted() {
        for bad in ["", "  ", "Ours", "cat-sort-uniq", "mine"] {
            assert_eq!(
                NoteMergeStrategy::parse(bad),
                None,
                "{bad:?} must not resolve to a strategy"
            );
        }
    }

    /// A merge is only in progress when git says so. Gating the
    /// commit/abort rows on this is what keeps them from being rows that
    /// error when pressed.
    #[test]
    fn a_gitdir_with_no_notes_merge_is_not_in_progress() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!Note::merge_in_progress(dir.path()));
        std::fs::write(dir.path().join("NOTES_MERGE_REF"), "ref\n").expect("write");
        assert!(Note::merge_in_progress(dir.path()));
    }
}
