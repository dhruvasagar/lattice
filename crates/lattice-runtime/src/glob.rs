//! Shared glob-set compilation (EF.1).
//!
//! One place to turn a list of string patterns into a compiled
//! [`globset::GlobSet`]. Used by [`crate::EventFilter`]'s `path_glob`
//! field (mode-activation triggers, mode-architecture.md §7.4) and
//! the major-mode resolver (MA.2); the LSP file-watcher and
//! workspace-exclude builders each carried their own
//! parse-skip-build loop before this, so the directive (EF.1) was
//! to consolidate rather than grow a third copy.
//!
//! Consolidating here means one graceful-degradation policy
//! (CLAUDE.md "graceful error handling -- log + skip on recoverable
//! failures, never panic"): an unparsable pattern is logged at
//! `warn` and skipped; a failed final build falls back to an empty
//! set (matches nothing) instead of panicking.

use globset::{Glob, GlobSet, GlobSetBuilder};

/// Compile `patterns` into a single [`GlobSet`] that matches a path
/// against any of them in one pass.
///
/// Recoverable failures degrade rather than panic:
/// - an individual unparsable pattern is logged at `warn` and
///   skipped (the surviving patterns still compile);
/// - if the final `GlobSet` build fails, an empty set is returned
///   (matches nothing) and the failure is logged at `warn`.
///
/// An empty `patterns` iterator yields an empty set, which matches
/// nothing -- callers that want "unconstrained" must not build a
/// glob at all (see [`crate::EventFilter::path_glob`], where `None`
/// is the unconstrained case).
pub fn compile_glob_set<I, S>(patterns: I) -> GlobSet
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let pat = pat.as_ref();
        match Glob::new(pat) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                tracing::warn!(pattern = pat, error = %e, "skipping unparsable glob pattern");
            }
        }
    }
    builder.build().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "glob set build failed; falling back to empty set");
        GlobSet::empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_matches_listed_patterns() {
        let set = compile_glob_set(["**/*.rs", "**/*.toml"]);
        assert!(set.is_match("src/lib.rs"));
        assert!(set.is_match("Cargo.toml"));
        assert!(!set.is_match("README.md"));
    }

    #[test]
    fn unparsable_pattern_is_skipped_survivors_compile() {
        // `[invalid` is an unterminated character class. It must be
        // skipped without taking down the valid patterns around it.
        let set = compile_glob_set(["**/*.rs", "[invalid", "**/*.md"]);
        assert!(set.is_match("src/lib.rs"));
        assert!(set.is_match("docs/x.md"));
        assert!(!set.is_match("Cargo.toml"));
    }

    #[test]
    fn empty_input_matches_nothing() {
        let set = compile_glob_set(Vec::<String>::new());
        assert!(!set.is_match("anything.rs"));
    }
}
