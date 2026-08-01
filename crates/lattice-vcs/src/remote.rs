use crate::{Repository, Result, VcsError};

/// Remote management — list, add, rename, remove, set-url, prune.
///
/// The peer of [`crate::Branch`] and [`crate::Stash`]: a thin, typed
/// wrapper over the git CLI. Fetch / pull / push are *not* here — those
/// are long-running network operations owned by `lattice-magit`'s
/// `RemoteOp`, which runs them detached. Everything on this type is a
/// local config edit that returns immediately.
pub struct Remote;

/// A single configured remote.
///
/// `push_url` falls back to `fetch_url` when the remote has no separate
/// `pushurl` configured — which is the common case, and matches what
/// `git remote -v` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// The remote's name, e.g. `origin`.
    pub name: String,
    /// The URL fetches read from.
    pub fetch_url: String,
    /// The URL pushes write to.
    pub push_url: String,
}

/// Parse the output of `git remote -v`.
///
/// The format is one `<name>\t<url> (fetch)` line and one
/// `<name>\t<url> (push)` line per remote, fetch first. Remotes are
/// returned in first-appearance order, which is the alphabetical order
/// git itself emits.
///
/// **Unparseable lines are skipped, not fatal.** A remote whose name or
/// URL contains something this parser does not expect must not take the
/// whole list down with it — the user would see an empty buffer with no
/// way to reach the remotes that *are* fine. A line missing its tab, or
/// carrying neither `(fetch)` nor `(push)`, is dropped silently here;
/// the caller renders whatever survived.
pub fn parse_remote_v(out: &str) -> Vec<RemoteEntry> {
    let mut entries: Vec<RemoteEntry> = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some((name, rest)) = line.split_once('\t') else {
            continue;
        };
        let (url, kind) = match rest.rsplit_once(' ') {
            Some((url, kind)) => (url.trim(), kind.trim()),
            None => continue,
        };
        let is_push = match kind {
            "(push)" => true,
            "(fetch)" => false,
            _ => continue,
        };
        match entries.iter_mut().find(|e| e.name == name) {
            Some(existing) => {
                if is_push {
                    existing.push_url = url.to_string();
                } else {
                    existing.fetch_url = url.to_string();
                }
            }
            None => entries.push(RemoteEntry {
                name: name.to_string(),
                // Both slots start at this URL so a remote that prints
                // only one of the two lines still renders a sane pair.
                fetch_url: url.to_string(),
                push_url: url.to_string(),
            }),
        }
    }
    entries
}

impl Remote {
    /// List every configured remote with its fetch and push URLs.
    ///
    /// Equivalent to `git remote -v`.
    pub fn list(repo: &Repository) -> Result<Vec<RemoteEntry>> {
        let out = repo
            .run_git_str(["remote", "-v"])
            .map_err(|e| VcsError::Remote(format!("remote list: {}", e)))?;
        Ok(parse_remote_v(&out))
    }

    /// Add a new remote.
    ///
    /// Equivalent to `git remote add <name> <url>`. Does not fetch.
    pub fn add(repo: &Repository, name: &str, url: &str) -> Result<()> {
        repo.run_git(["remote", "add", name, url])
            .map(|_| ())
            .map_err(|e| VcsError::Remote(format!("remote add {}: {}", name, e)))
    }

    /// Rename a remote, rewriting its tracking refspecs.
    ///
    /// Equivalent to `git remote rename <old> <new>`.
    pub fn rename(repo: &Repository, old: &str, new: &str) -> Result<()> {
        repo.run_git(["remote", "rename", old, new])
            .map(|_| ())
            .map_err(|e| VcsError::Remote(format!("remote rename {} -> {}: {}", old, new, e)))
    }

    /// Remove a remote and its remote-tracking branches.
    ///
    /// Equivalent to `git remote remove <name>`.
    pub fn remove(repo: &Repository, name: &str) -> Result<()> {
        repo.run_git(["remote", "remove", name])
            .map(|_| ())
            .map_err(|e| VcsError::Remote(format!("remote remove {}: {}", name, e)))
    }

    /// Point a remote at a different URL.
    ///
    /// Equivalent to `git remote set-url <name> <url>`. This sets the
    /// fetch URL; a remote with a separately-configured `pushurl` keeps
    /// it, which is why [`RemoteEntry`] carries both.
    pub fn set_url(repo: &Repository, name: &str, url: &str) -> Result<()> {
        repo.run_git(["remote", "set-url", name, url])
            .map(|_| ())
            .map_err(|e| VcsError::Remote(format!("remote set-url {}: {}", name, e)))
    }

    /// Delete local refs for branches that no longer exist on the remote.
    ///
    /// Equivalent to `git remote prune <name>`. This talks to the
    /// network, so callers run it off the actor thread like the other
    /// remote operations.
    pub fn prune(repo: &Repository, name: &str) -> Result<()> {
        repo.run_git(["remote", "prune", name])
            .map(|_| ())
            .map_err(|e| VcsError::Remote(format!("remote prune {}: {}", name, e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_ordinary_two_line_per_remote_shape() {
        let out = "origin\tgit@github.com:a/b.git (fetch)\n\
                   origin\tgit@github.com:a/b.git (push)\n\
                   upstream\thttps://example.com/c.git (fetch)\n\
                   upstream\thttps://example.com/c.git (push)\n";
        assert_eq!(
            parse_remote_v(out),
            vec![
                RemoteEntry {
                    name: "origin".into(),
                    fetch_url: "git@github.com:a/b.git".into(),
                    push_url: "git@github.com:a/b.git".into(),
                },
                RemoteEntry {
                    name: "upstream".into(),
                    fetch_url: "https://example.com/c.git".into(),
                    push_url: "https://example.com/c.git".into(),
                },
            ]
        );
    }

    #[test]
    fn a_separate_pushurl_shows_as_a_differing_push_url() {
        let out = "origin\thttps://example.com/read.git (fetch)\n\
                   origin\tgit@example.com:write.git (push)\n";
        let got = parse_remote_v(out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].fetch_url, "https://example.com/read.git");
        assert_eq!(got[0].push_url, "git@example.com:write.git");
    }

    #[test]
    fn a_remote_printing_only_one_line_still_gets_both_urls() {
        // Defensive: git always prints the pair, but a truncated read
        // must not produce an entry with an empty URL column.
        let out = "origin\tgit@github.com:a/b.git (fetch)\n";
        let got = parse_remote_v(out);
        assert_eq!(got[0].fetch_url, got[0].push_url);
        assert!(!got[0].push_url.is_empty());
    }

    #[test]
    fn unparseable_lines_are_skipped_and_do_not_lose_the_good_ones() {
        let out = "garbage with no tab\n\
                   origin\tgit@github.com:a/b.git (fetch)\n\
                   weird\tsomething (mirror)\n\
                   origin\tgit@github.com:a/b.git (push)\n";
        let got = parse_remote_v(out);
        assert_eq!(got.len(), 1, "only origin is well-formed: {:?}", got);
        assert_eq!(got[0].name, "origin");
    }

    #[test]
    fn no_remotes_is_an_empty_list_not_an_error() {
        assert!(parse_remote_v("").is_empty());
        assert!(parse_remote_v("\n\n").is_empty());
    }

    #[test]
    fn a_url_containing_spaces_keeps_its_whole_url() {
        // `rsplit_once(' ')` splits on the LAST space, so only the
        // `(fetch)` / `(push)` suffix is peeled off.
        let out = "local\t/tmp/my repo/.git (fetch)\n";
        assert_eq!(parse_remote_v(out)[0].fetch_url, "/tmp/my repo/.git");
    }
}
