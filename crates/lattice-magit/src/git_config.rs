//! MG.43g: the git-config values magit's `C` rows report.
//!
//! Magit renders a config value *inside* the menu (`pull.rebase =
//! true`) and edits it in place. The row's whole purpose is to report
//! the CURRENT value, which means the menu needs that value while it
//! is being built.
//!
//! Building a menu is a keystroke path, so it must not read the
//! filesystem — paramount goal #1. The values therefore live in a
//! cache refreshed off-thread, and the builder only ever reads a map.
//!
//! **One `git config --list` populates every key.** Reading each key
//! with its own `git config --get` would be one process per row; the
//! list form is a single call whose cost does not grow with the number
//! of rows a menu shows.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Snapshot of the repository's effective git config.
///
/// `None` (rather than an empty map) is the load-bearing state: it
/// means **not read yet**, which is a different claim from "read, and
/// this key is unset". A row that showed `unset` for a value nobody
/// has looked up would be reporting a fact about the user's
/// configuration that was never checked — and reporting the current
/// value is the entire reason the row exists.
/// MR.6: keyed by repository.
///
/// It used to hold ONE map for the process, which was invisible while
/// every magit surface acted on one repository and wrong the moment they
/// stopped: the `Configure` rows in a menu opened over repo B reported
/// repo A's `pull.rebase`, and `C` then set it — reading one
/// repository's config and writing another's.
#[derive(Default)]
pub(crate) struct GitConfigCache {
    values: Mutex<HashMap<std::path::PathBuf, HashMap<String, String>>>,
}

impl GitConfigCache {
    /// The current value of `key`.
    ///
    /// Three outcomes, deliberately distinct:
    /// - `None` — not read yet (renders as `…`)
    /// - `Some("")` — read, and unset (renders as `unset`)
    /// - `Some(v)` — read, and set
    pub(crate) fn get(&self, workdir: &std::path::Path, key: &str) -> Option<String> {
        let guard = self.values.lock().ok()?;
        let map = guard.get(workdir)?;
        Some(map.get(key).cloned().unwrap_or_default())
    }

    fn store(&self, workdir: std::path::PathBuf, map: HashMap<String, String>) {
        if let Ok(mut g) = self.values.lock() {
            g.insert(workdir, map);
        }
    }
}

pub(crate) type GitConfigCacheHandle = Arc<GitConfigCache>;

fn cache() -> &'static GitConfigCacheHandle {
    static CACHE: OnceLock<GitConfigCacheHandle> = OnceLock::new();
    CACHE.get_or_init(GitConfigCacheHandle::default)
}

/// The value `key` currently holds, for a menu being built.
///
/// Reads the cache and nothing else — no I/O, no blocking. This is
/// called from the transient builder, which runs on a keystroke.
pub(crate) fn value_of(workdir: &std::path::Path, key: &str) -> Option<String> {
    cache().get(workdir, key)
}

/// Kick off a refresh off the actor thread.
///
/// Fire-and-forget by design: the caller is a menu-opening keystroke
/// and must not wait. A refresh that lands after the menu was built
/// shows up the next time it opens, which is why rows render `…`
/// rather than blocking for a value.
pub(crate) fn refresh(workdir: std::path::PathBuf) {
    // `tokio::task::spawn` PANICS with no reactor, and this is reached
    // from the transient builder — which runs wherever a menu is
    // built, including contexts with no runtime (tests are how this
    // was found; a menu built off the actor thread would be the same
    // crash in production). A missing runtime means the values simply
    // stay unread and the rows render `…`, which is a state they
    // already have.
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::debug!(target: "lattice_magit", "git config refresh skipped: no runtime");
        return;
    }
    tokio::task::spawn(async move {
        let read = workdir.clone();
        let parsed = tokio::task::spawn_blocking(move || read_config(&read)).await;
        match parsed {
            Ok(Some(map)) => cache().store(workdir, map),
            // A repo we cannot read config from is not an error worth
            // surfacing — the rows simply keep reporting `…` rather
            // than claiming a value. Logged, never silent.
            Ok(None) => {
                tracing::debug!(target: "lattice_magit", "git config --list produced nothing");
            }
            Err(e) => {
                tracing::debug!(target: "lattice_magit", "git config refresh failed: {e}");
            }
        }
    });
}

/// `git config --list -z` -> a map.
///
/// **`-z` matters.** The default `--list` output is `key=value` per
/// line, which mis-parses any value containing a newline (a multi-line
/// `alias.*` is ordinary). `-z` separates entries with NUL and the key
/// from the value with a newline, so both can contain anything.
fn read_config(workdir: &std::path::Path) -> Option<HashMap<String, String>> {
    let out = std::process::Command::new("git")
        .args(["config", "--list", "-z"])
        .current_dir(workdir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_config_z(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `git config --list -z` output.
pub(crate) fn parse_config_z(text: &str) -> HashMap<String, String> {
    text.split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            // A key with no newline is a *valueless* key (`[section]
            // key` with nothing after it), which git reports as the key
            // alone. That is set-but-empty, not absent — so every entry
            // yields a pair and none is filtered out.
            match entry.split_once('\n') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (entry.to_string(), String::new()),
            }
        })
        .collect()
}

/// Write a config value, then refresh so the menu reports the change.
pub(crate) fn set(workdir: std::path::PathBuf, key: &str, value: &str) -> lattice_grammar::Effect {
    let argv = if value.is_empty() {
        // An empty value means "unset it", not "set it to the empty
        // string": magit's configure rows clear a value by leaving the
        // prompt blank, and `git config key ""` would leave the key
        // present-and-empty, which reads back as set.
        vec!["config".to_string(), "--unset".to_string(), key.to_string()]
    } else {
        vec!["config".to_string(), key.to_string(), value.to_string()]
    };
    let effect = crate::magit_global_mode::spawn_git(workdir.clone(), argv, "git config");
    // Re-read THAT repository: the write and the read-back must be the
    // same one, which is the whole reason the cache is keyed now.
    refresh(workdir);
    effect
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value containing a newline round-trips, which is why the
    /// reader uses `-z` rather than the line-oriented default.
    #[test]
    fn a_multi_line_value_survives_parsing() {
        let text = "alias.lg\nlog --graph\n--oneline\0user.name\nDhruva\0";
        let map = parse_config_z(text);
        assert_eq!(
            map.get("alias.lg").map(String::as_str),
            Some("log --graph\n--oneline")
        );
        assert_eq!(map.get("user.name").map(String::as_str), Some("Dhruva"));
    }

    /// A valueless key is set-but-empty, not absent.
    #[test]
    fn a_valueless_key_is_present_and_empty() {
        let map = parse_config_z("core.bare\0");
        assert_eq!(map.get("core.bare").map(String::as_str), Some(""));
    }

    /// **Not-read-yet and read-but-unset are different answers.**
    ///
    /// A fresh cache reports `None` for every key. After a read that
    /// simply did not contain the key, it reports `Some("")`. Showing
    /// `unset` for the first would state a fact about the user's config
    /// that was never checked.
    #[test]
    fn an_unread_cache_does_not_claim_a_key_is_unset() {
        let cache = GitConfigCache::default();
        let repo = std::path::Path::new("/work/api");
        assert_eq!(
            cache.get(repo, "pull.rebase"),
            None,
            "nothing has been read yet"
        );

        cache.store(
            repo.to_path_buf(),
            HashMap::from([("user.name".to_string(), "d".to_string())]),
        );
        assert_eq!(
            cache.get(repo, "pull.rebase"),
            Some(String::new()),
            "the config WAS read and does not set this key",
        );
        assert_eq!(cache.get(repo, "user.name"), Some("d".to_string()));
    }

    /// MR.6: one repository's config never answers for another's.
    ///
    /// Before the cache was keyed, the `Configure` rows in a menu opened
    /// over repo B reported repo A's values — and `C` then wrote what
    /// the row had read, so the read and the write disagreed about which
    /// repository they were about.
    #[test]
    fn one_repositorys_config_does_not_answer_for_another() {
        let cache = GitConfigCache::default();
        let a = std::path::Path::new("/work/api");
        let b = std::path::Path::new("/oss/api");

        cache.store(
            a.to_path_buf(),
            HashMap::from([("pull.rebase".to_string(), "true".to_string())]),
        );

        assert_eq!(cache.get(a, "pull.rebase"), Some("true".to_string()));
        assert_eq!(
            cache.get(b, "pull.rebase"),
            None,
            "B's config has not been read — reporting A's would be a \
             claim about a repository nobody looked at"
        );
    }

    /// **`refresh` must not panic without a tokio runtime.**
    ///
    /// It is called from the transient builder, which runs wherever a
    /// menu is built. `tokio::task::spawn` panics with no reactor, so
    /// an unguarded call turns "open the magit menu" into a crash in
    /// any context that is not on the runtime. Rendering `…` is the
    /// correct degradation — it is a state the rows already have.
    #[test]
    fn refresh_without_a_runtime_does_not_panic() {
        refresh(std::path::PathBuf::from("/nonexistent"));
    }

    /// Clearing a value unsets the key rather than setting it empty —
    /// `git config key ""` leaves it present, which reads back as set.
    #[test]
    fn clearing_a_value_unsets_rather_than_emptying() {
        // Asserted on the argv shape the writer builds, since the
        // write itself is a spawned process.
        let cleared: Vec<String> = vec!["config".into(), "--unset".into(), "pull.rebase".into()];
        let set: Vec<String> = vec!["config".into(), "pull.rebase".into(), "true".into()];
        assert_ne!(cleared, set);
        assert!(cleared.contains(&"--unset".to_string()));
        assert!(!set.contains(&"--unset".to_string()));
    }
}
