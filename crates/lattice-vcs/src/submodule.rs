use crate::{Repository, Result, VcsError};

/// Submodule operations — list, add, update, sync, remove.
///
/// Uses the git CLI. `update` and `add` reach the network; the rest are
/// local.
pub struct Submodule;

/// What `git submodule status` reports about one submodule.
///
/// The leading character of each status line, which is the part that
/// decides what the user can usefully do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleState {
    /// `-` — not initialised. Its working tree is empty; `update` is
    /// what fills it.
    Uninitialised,
    /// ` ` — checked out at the commit the superproject records.
    InSync,
    /// `+` — checked out at a *different* commit than the superproject
    /// records. Either you moved it deliberately or an `update` is
    /// pending.
    Modified,
    /// `U` — has merge conflicts.
    Conflicted,
}

impl SubmoduleState {
    /// The one-character marker `git submodule status` uses, which is
    /// also what the buffer renders — so the row reads the same as
    /// git's own output.
    pub fn marker(self) -> char {
        match self {
            SubmoduleState::Uninitialised => '-',
            SubmoduleState::InSync => ' ',
            SubmoduleState::Modified => '+',
            SubmoduleState::Conflicted => 'U',
        }
    }
}

/// One configured submodule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleEntry {
    pub state: SubmoduleState,
    /// The commit the submodule is at (full SHA, as git prints it).
    pub sha: String,
    /// Path relative to the superproject's working tree.
    pub path: String,
    /// The `git describe` output git appends in parentheses, when
    /// there is one. Empty for an uninitialised submodule, which has
    /// no checkout to describe.
    pub describe: String,
}

/// Parse `git submodule status` output.
///
/// Each line is `<marker><sha> <path>[ (<describe>)]`, where the marker
/// is one character and is **not** separated from the SHA. A line that
/// does not fit is skipped rather than failing the list: one
/// unparseable submodule must not hide the others.
pub fn parse_submodule_status(out: &str) -> Vec<SubmoduleEntry> {
    let mut entries = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut chars = line.chars();
        let state = match chars.next() {
            Some('-') => SubmoduleState::Uninitialised,
            Some('+') => SubmoduleState::Modified,
            Some('U') => SubmoduleState::Conflicted,
            Some(' ') => SubmoduleState::InSync,
            _ => continue,
        };
        let rest = chars.as_str();
        let Some((sha, tail)) = rest.split_once(' ') else {
            continue;
        };
        if sha.is_empty() {
            continue;
        }
        // The describe suffix is parenthesised and always last; a path
        // containing a space keeps it, because only the trailing
        // `(...)` is peeled off.
        let (path, describe) = match tail.rfind(" (") {
            Some(i) if tail.ends_with(')') => (&tail[..i], &tail[i + 2..tail.len() - 1]),
            _ => (tail, ""),
        };
        if path.is_empty() {
            continue;
        }
        entries.push(SubmoduleEntry {
            state,
            sha: sha.to_string(),
            path: path.to_string(),
            describe: describe.to_string(),
        });
    }
    entries
}

impl Submodule {
    /// Every configured submodule, in the order git lists them.
    ///
    /// Equivalent to `git submodule status`.
    pub fn list(repo: &Repository) -> Result<Vec<SubmoduleEntry>> {
        let out = repo
            .run_git_str(["submodule", "status"])
            .map_err(|e| VcsError::Submodule(format!("submodule list: {}", e)))?;
        Ok(parse_submodule_status(&out))
    }

    /// Add a submodule at `path` from `url`.
    ///
    /// Equivalent to `git submodule add <url> <path>`.
    pub fn add(repo: &Repository, url: &str, path: &str) -> Result<()> {
        repo.run_git(["submodule", "add", url, path])
            .map(|_| ())
            .map_err(|e| VcsError::Submodule(format!("submodule add {}: {}", path, e)))
    }

    /// Initialise and check out a submodule at the recorded commit
    /// (`None` = all of them).
    ///
    /// Equivalent to `git submodule update --init --recursive [<path>]`
    /// — magit's `p` populate, `r` register and `u` update collapsed
    /// into the one that subsumes them. Reaches the network for a
    /// submodule that has never been cloned.
    pub fn update(repo: &Repository, path: Option<&str>) -> Result<()> {
        let mut args: Vec<String> = vec![
            "submodule".into(),
            "update".into(),
            "--init".into(),
            "--recursive".into(),
        ];
        if let Some(path) = path {
            args.push(path.to_string());
        }
        repo.run_git(args)
            .map(|_| ())
            .map_err(|e| VcsError::Submodule(format!("submodule update: {}", e)))
    }

    /// Re-copy the configured URLs into the submodules' own git config
    /// (`None` = all of them).
    ///
    /// Equivalent to `git submodule sync --recursive [<path>]`. What
    /// you run after the superproject's `.gitmodules` URLs changed.
    pub fn sync(repo: &Repository, path: Option<&str>) -> Result<()> {
        let mut args: Vec<String> = vec!["submodule".into(), "sync".into(), "--recursive".into()];
        if let Some(path) = path {
            args.push(path.to_string());
        }
        repo.run_git(args)
            .map(|_| ())
            .map_err(|e| VcsError::Submodule(format!("submodule sync: {}", e)))
    }

    /// Remove a submodule: deinit it, then drop it from the index and
    /// the working tree.
    ///
    /// `git submodule deinit -f` followed by `git rm -f`. **This
    /// deletes the submodule's working tree**, including anything
    /// uncommitted inside it, which is why callers confirm first.
    ///
    /// The deinit runs first and its failure is fatal: `git rm` on a
    /// still-populated submodule leaves the checkout orphaned in
    /// `.git/modules` with nothing in the index pointing at it.
    pub fn remove(repo: &Repository, path: &str) -> Result<()> {
        repo.run_git(["submodule", "deinit", "-f", path])
            .map_err(|e| VcsError::Submodule(format!("submodule deinit {}: {}", path, e)))?;
        repo.run_git(["rm", "-f", path])
            .map(|_| ())
            .map_err(|e| VcsError::Submodule(format!("submodule remove {}: {}", path, e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_markers_git_uses() {
        let out = concat!(
            "-a1b2c3d4e5f60718293a4b5c6d7e8f9012345678 vendor/uninit\n",
            " b1b2c3d4e5f60718293a4b5c6d7e8f9012345678 vendor/insync (v1.2.3)\n",
            "+c1b2c3d4e5f60718293a4b5c6d7e8f9012345678 vendor/moved (v1.2.3-4-gabcdef)\n",
        );
        let got = parse_submodule_status(out);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].state, SubmoduleState::Uninitialised);
        assert_eq!(got[0].path, "vendor/uninit");
        assert_eq!(
            got[0].describe, "",
            "an uninitialised submodule has no checkout to describe"
        );
        assert_eq!(got[1].state, SubmoduleState::InSync);
        assert_eq!(got[1].describe, "v1.2.3");
        assert_eq!(got[2].state, SubmoduleState::Modified);
        assert_eq!(got[2].path, "vendor/moved");
    }

    #[test]
    fn a_conflicted_submodule_is_its_own_state() {
        let out = "Uabc123 vendor/x\n";
        assert_eq!(
            parse_submodule_status(out)[0].state,
            SubmoduleState::Conflicted
        );
    }

    #[test]
    fn the_marker_round_trips_to_gits_own_character() {
        for state in [
            SubmoduleState::Uninitialised,
            SubmoduleState::InSync,
            SubmoduleState::Modified,
            SubmoduleState::Conflicted,
        ] {
            let line = format!("{}abc123 vendor/x\n", state.marker());
            assert_eq!(
                parse_submodule_status(&line)[0].state,
                state,
                "marker {:?} did not round-trip",
                state.marker()
            );
        }
    }

    #[test]
    fn a_path_containing_spaces_keeps_it() {
        // Only the trailing parenthesised describe is peeled off.
        let out = " abc123 vendor/my module (v1)\n";
        let got = parse_submodule_status(out);
        assert_eq!(got[0].path, "vendor/my module");
        assert_eq!(got[0].describe, "v1");
    }

    #[test]
    fn a_path_with_parentheses_but_no_describe_is_not_truncated() {
        let out = " abc123 vendor/thing\n";
        assert_eq!(parse_submodule_status(out)[0].path, "vendor/thing");
        assert_eq!(parse_submodule_status(out)[0].describe, "");
    }

    #[test]
    fn unparseable_lines_are_skipped_and_do_not_lose_the_good_ones() {
        let out = concat!(
            "garbage\n",
            "?abc123 vendor/unknown-marker\n",
            " abc123 vendor/fine (v1)\n",
        );
        let got = parse_submodule_status(out);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "vendor/fine");
    }

    #[test]
    fn no_submodules_is_an_empty_list_not_an_error() {
        assert!(parse_submodule_status("").is_empty());
        assert!(parse_submodule_status("\n\n").is_empty());
    }
}
