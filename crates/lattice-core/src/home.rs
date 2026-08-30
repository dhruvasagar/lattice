//! `~` expansion, in one place.
//!
//! ## Why this exists rather than a local helper per consumer
//!
//! There were four, and they disagreed in the way that matters. The agenda's,
//! the completion generator's, the dispatcher's and the org guest's were each a
//! private `expand_tilde`, and the first three resolved the home directory as
//! `std::env::var_os("HOME")` — which is POSIX-only. On Windows that returns
//! `None`, so `~/notes` was left verbatim and every lookup silently found
//! nothing. `org.agenda-files`' own documentation says "`~` is expanded", and
//! on Windows it did not.
//!
//! Silent is the operative word. A path that fails to expand does not error; it
//! becomes a path that does not exist, and every consumer here reports "no
//! files" rather than "that is not a real directory". The user is then told
//! their corpus is empty.
//!
//! [`dirs::home_dir`] answers on Windows too (`%USERPROFILE%`, then the
//! known-folder API), which is the whole reason to converge rather than to fix
//! four copies of the `HOME` lookup.
//!
//! ## What it deliberately is not
//!
//! Not shell expansion. No `$VAR`, no `~other-user`, no globbing. The one thing
//! a user types by hand that a `PathBuf` will not resolve is a leading tilde,
//! and every step past that is a parser with its own quoting rules — which a
//! config value is not asking for. A `$HOME/notes` that stayed verbatim would
//! be visible and fixable; guessing at it would not.

use std::path::{Path, PathBuf};

/// Expand a leading `~` against the user's home directory.
///
/// `~` alone and `~/rest` both expand; `~user` does not (there is no lookup for
/// another user's home that works across platforms). Anything else is returned
/// unchanged, so this is safe to call on a path that is already absolute.
///
/// Unresolvable home → the input, verbatim. That keeps the failure visible as a
/// path with a `~` still in it rather than as a silently-wrong location.
pub fn expand_tilde(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    // `~user` is not ours to resolve, and mangling it into `<home>user` would
    // be worse than leaving it: the result is a plausible path to the wrong
    // place, which is exactly the failure this module exists to end.
    if !rest.is_empty() && !rest.starts_with('/') && !rest.starts_with('\\') {
        return raw.to_string();
    }
    let Some(home) = dirs::home_dir() else {
        return raw.to_string();
    };
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        return home.display().to_string();
    }
    home.join(rest).display().to_string()
}

/// [`expand_tilde`] for a `Path`, returning a `PathBuf`.
pub fn expand_tilde_path(raw: &Path) -> PathBuf {
    match raw.to_str() {
        // Non-UTF-8 cannot contain a leading ASCII `~` we could act on without
        // re-encoding, and a path is never worth mangling to expand one.
        None => raw.to_path_buf(),
        Some(s) => PathBuf::from(expand_tilde(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_without_a_tilde_is_untouched() {
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde(""), "");
        assert_eq!(expand_tilde("a~b"), "a~b", "a tilde must LEAD to count");
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        let Some(home) = dirs::home_dir() else {
            eprintln!("SKIP: no home directory on this machine");
            return;
        };
        assert_eq!(expand_tilde("~"), home.display().to_string());
        assert_eq!(
            expand_tilde("~/notes"),
            home.join("notes").display().to_string()
        );
    }

    /// `~user` is left alone. Expanding it against OUR home would produce a
    /// plausible path to the wrong place, which is worse than not expanding.
    #[test]
    fn another_users_home_is_left_verbatim() {
        assert_eq!(expand_tilde("~alice/notes"), "~alice/notes");
        assert_eq!(expand_tilde("~alice"), "~alice");
    }

    /// No `$VAR`, deliberately — a config value is not a shell command line,
    /// and a half-implemented expansion is worse than none.
    #[test]
    fn shell_variables_are_not_expanded() {
        assert_eq!(expand_tilde("$HOME/notes"), "$HOME/notes");
        assert_eq!(expand_tilde("~/$FOO"), {
            let home = dirs::home_dir().map(|h| h.join("$FOO").display().to_string());
            home.unwrap_or_else(|| "~/$FOO".to_string())
        });
    }

    #[test]
    fn the_path_form_agrees_with_the_string_form() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert_eq!(expand_tilde_path(Path::new("~/x")), home.join("x"));
        assert_eq!(expand_tilde_path(Path::new("/x")), PathBuf::from("/x"));
    }
}
