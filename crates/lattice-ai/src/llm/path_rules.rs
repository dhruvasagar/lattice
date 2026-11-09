//! A list of gitignore patterns, compiled once and anchored at the project
//! root: what `llm.exclude` (and the other path lists) match with.
//!
//! Gitignore syntax because it is the one every vendor's ignore file shares
//! (`.aiexclude`, `.aiignore`, `.agentignore`, `.cursorignore`): a bare name
//! matches at any depth, a leading `/` anchors at the root, a `/` inside
//! anchors the pattern to the root as well, and a trailing `/` names a
//! directory. A pattern matching a directory matches everything under it.
//!
//! Compiling is the expensive part, so a [`PathRules`] is built when its list
//! or root changes and checked many times; the check itself is a lookup per
//! path component, cheap enough for every tool call and every captured span.

use std::path::{Component, Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Compiled patterns, with the text each came from.
#[derive(Debug)]
pub struct PathRules {
    root: PathBuf,
    set: Gitignore,
    /// For a path outside the root: the bare-name patterns (no `/` but a
    /// trailing one), matched against the file name and every directory name.
    names: Vec<(String, globset::GlobMatcher)>,
}

impl PathRules {
    /// Compile `patterns` against `root`. A pattern gitignore cannot parse is
    /// returned as the error, named, so the caller can say which entry is bad.
    pub fn new(root: &Path, patterns: &[String]) -> Result<Self, String> {
        let mut builder = GitignoreBuilder::new(root);
        let mut names = Vec::new();
        for pattern in patterns {
            builder
                .add_line(None, pattern)
                .map_err(|e| format!("`{pattern}` is not a gitignore pattern ({e})"))?;
            // Comments and blank lines (an ignore file has both) are for
            // gitignore alone; they name nothing.
            if pattern.trim().is_empty() || pattern.starts_with('#') {
                continue;
            }
            let bare = pattern.trim_end_matches('/');
            if !bare.is_empty()
                && !bare.contains('/')
                && !bare.starts_with('!')
                && let Ok(glob) = globset::Glob::new(bare)
            {
                names.push((pattern.clone(), glob.compile_matcher()));
            }
        }
        let set = builder
            .build()
            .map_err(|e| format!("the patterns do not compile ({e})"))?;
        Ok(Self {
            root: root.to_path_buf(),
            set,
            names,
        })
    }

    /// Whether `patterns` would compile: what option parsing checks.
    pub fn validate(patterns: &[String]) -> Result<(), String> {
        Self::new(Path::new("/"), patterns).map(|_| ())
    }

    /// The pattern `path` matches, if any.
    ///
    /// Inside the root the full gitignore rules apply, including a `!`
    /// re-include within this set. The path is checked as a directory too, so
    /// a `name/` pattern also refuses a file called `name`: refusing too much
    /// is the safe side, and it spares a `stat` per check.
    ///
    /// Outside the root (a file attached from elsewhere) only bare-name
    /// patterns can mean anything, and they match by name, the file's or any
    /// directory's above it: `.env` still refuses `~/other/.env`, and `.aws/`
    /// refuses `~/.aws/credentials`.
    pub fn matching(&self, path: &Path) -> Option<&str> {
        if let Ok(rel) = path.strip_prefix(&self.root) {
            if rel.as_os_str().is_empty() {
                return None;
            }
            return match self.set.matched_path_or_any_parents(rel, true) {
                ignore::Match::Ignore(glob) => Some(glob.original()),
                _ => None,
            };
        }
        let file = path.file_name()?;
        let dirs: Vec<&std::ffi::OsStr> = path
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .filter_map(|c| match c {
                Component::Normal(name) => Some(name),
                _ => None,
            })
            .collect();
        self.names
            .iter()
            .find(|(_, glob)| glob.is_match(file) || dirs.iter().any(|d| glob.is_match(d)))
            .map(|(pattern, _)| pattern.as_str())
    }
}

/// The ignore files other harnesses read, taken as `exclude` additions.
pub const IGNORE_FILES: [&str; 3] = [".aiexclude", ".aiignore", ".agentignore"];

/// The most of one ignore file read; past it the rest is not read, and said so.
const MAX_IGNORE_FILE: u64 = 256 * 1024;

/// What a project adds to the safety lists, compiled for the root it was read
/// under. Kept apart from the global lists and matched as its own set, so a
/// project's `!pattern` can only re-include what the project itself excluded.
#[derive(Debug)]
pub struct ProjectRules {
    pub root: PathBuf,
    pub exclude: PathRules,
    pub review: PathRules,
    pub deny: Vec<String>,
}

impl ProjectRules {
    /// Compile `lists` and the ignore files under `root`. Blocking file reads:
    /// call it off the editor thread. A problem (an unreadable or oversized
    /// ignore file, a pattern that will not compile) is returned beside what
    /// could be built, never instead of it: a bad pattern is skipped alone, so
    /// one typo cannot drop the project's other excludes.
    pub fn load(root: &Path, lists: &super::settings::ProjectLists) -> (Option<Self>, Vec<String>) {
        let mut problems = Vec::new();
        let mut exclude = lists.exclude.clone();
        for name in IGNORE_FILES {
            let path = root.join(name);
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {
                    use std::io::Read;
                    let mut text = String::new();
                    let read = std::fs::File::open(&path)
                        .and_then(|f| f.take(MAX_IGNORE_FILE).read_to_string(&mut text));
                    match read {
                        Ok(_) => exclude.extend(text.lines().map(str::to_string)),
                        Err(e) => problems.push(format!("llm: cannot read {name}: {e}")),
                    }
                    if meta.len() > MAX_IGNORE_FILE {
                        problems.push(format!(
                            "llm: {name} is over {MAX_IGNORE_FILE} bytes; the rest is not read"
                        ));
                    }
                }
                _ => {}
            }
        }
        let mut review = lists.review.clone();
        for (list, name) in [(&mut exclude, "exclude"), (&mut review, "review")] {
            list.retain(|p| match PathRules::validate(std::slice::from_ref(p)) {
                Ok(()) => true,
                Err(e) => {
                    problems.push(format!("llm: the project's {name} list skips {e}"));
                    false
                }
            });
        }
        let mut deny = lists.deny.clone();
        deny.retain(|r| {
            match super::tools::rules::ToolRules::validate(
                std::slice::from_ref(r),
                super::tools::rules::List::Deny,
            ) {
                Ok(()) => true,
                Err(e) => {
                    problems.push(format!("llm: the project's deny list skips {e}"));
                    false
                }
            }
        });
        if exclude.is_empty() && review.is_empty() && deny.is_empty() {
            return (None, problems);
        }
        // Every pattern was checked alone above, so these compile.
        match (
            PathRules::new(root, &exclude),
            PathRules::new(root, &review),
        ) {
            (Ok(exclude), Ok(review)) => (
                Some(Self {
                    root: root.to_path_buf(),
                    exclude,
                    review,
                    deny,
                }),
                problems,
            ),
            (Err(e), _) | (_, Err(e)) => {
                problems.push(format!("llm: the project's lists: {e}"));
                (None, problems)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn rules(patterns: &[&str]) -> PathRules {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        PathRules::new(Path::new("/p"), &patterns).unwrap()
    }

    fn at(r: &PathRules, path: &str) -> Option<String> {
        r.matching(Path::new(path)).map(str::to_string)
    }

    #[test]
    fn a_bare_name_matches_at_any_depth() {
        let r = rules(&[".env", "*.pem"]);
        assert_eq!(at(&r, "/p/.env").as_deref(), Some(".env"));
        assert_eq!(at(&r, "/p/deploy/prod/.env").as_deref(), Some(".env"));
        assert_eq!(at(&r, "/p/certs/site.pem").as_deref(), Some("*.pem"));
        assert_eq!(at(&r, "/p/src/env.rs"), None);
    }

    /// R.4: a pattern with a `/` was matched against the absolute path, so it
    /// never matched anything; it is relative to the root.
    #[test]
    fn a_pattern_with_a_slash_is_relative_to_the_root() {
        let r = rules(&["secrets/*.txt", "/build.log"]);
        assert_eq!(at(&r, "/p/secrets/a.txt").as_deref(), Some("secrets/*.txt"));
        assert_eq!(at(&r, "/p/other/secrets/a.txt"), None, "anchored");
        assert_eq!(at(&r, "/p/build.log").as_deref(), Some("/build.log"));
        assert_eq!(at(&r, "/p/sub/build.log"), None, "a leading / anchors");
    }

    #[test]
    fn a_directory_pattern_covers_everything_under_it() {
        let r = rules(&[".aws/"]);
        assert_eq!(at(&r, "/p/.aws/credentials").as_deref(), Some(".aws/"));
        assert_eq!(at(&r, "/p/home/.aws/config").as_deref(), Some(".aws/"));
        assert_eq!(at(&r, "/p/aws/config"), None);
    }

    #[test]
    fn a_negation_reincludes_within_its_own_set() {
        let r = rules(&["*.key", "!public.key"]);
        assert_eq!(at(&r, "/p/private.key").as_deref(), Some("*.key"));
        assert_eq!(at(&r, "/p/public.key"), None);
    }

    #[test]
    fn outside_the_root_only_names_match() {
        let r = rules(&[".env", ".aws/", "secrets/*.txt"]);
        assert_eq!(at(&r, "/elsewhere/.env").as_deref(), Some(".env"));
        assert_eq!(at(&r, "/home/u/.aws/credentials").as_deref(), Some(".aws/"));
        assert_eq!(at(&r, "/elsewhere/secrets/a.txt"), None);
        assert_eq!(at(&r, "/elsewhere/notes.txt"), None);
    }

    /// The vendor ignore files are read as excludes, comments and blank lines
    /// skipped; a bad pattern is skipped alone, so the rest still holds.
    #[test]
    fn the_ignore_files_add_to_the_project_excludes() {
        let dir = std::env::temp_dir().join(format!("lattice-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".aiexclude"), "# keys\n*.secret\n\n").unwrap();
        std::fs::write(dir.join(".agentignore"), "private/\na{b\n").unwrap();
        let lists = crate::llm::settings::ProjectLists {
            exclude: vec!["notes.md".to_string()],
            ..Default::default()
        };
        let (rules, problems) = ProjectRules::load(&dir, &lists);
        let rules = rules.unwrap();
        let hit = |p: &str| rules.exclude.matching(&dir.join(p)).map(str::to_string);
        assert_eq!(hit("notes.md").as_deref(), Some("notes.md"));
        assert_eq!(hit("x/api.secret").as_deref(), Some("*.secret"));
        assert_eq!(hit("private/a.txt").as_deref(), Some("private/"));
        assert_eq!(hit("src/main.rs"), None);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("a{b"), "{problems:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_pattern_is_named() {
        let bad = vec!["a{b".to_string()];
        let why = PathRules::validate(&bad).unwrap_err();
        assert!(why.contains("`a{b`"), "{why}");
    }
}
