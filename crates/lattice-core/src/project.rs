//! PR.1: which project a path belongs to.
//!
//! Design: [`project-resolution.md`](../../../docs/dev/architecture/project-resolution.md).
//!
//! A project is a **pure function of a path** with a cache in front of
//! it. There is no "current project" here — no mutable session state to
//! persist, invalidate, or fall out of sync. Multiple projects therefore
//! co-exist by construction: buffers from three checkouts each answer
//! with their own, and an action in one cannot re-root another because
//! there is no shared cell for it to write.
//!
//! ## Why `for_path` is total
//!
//! [`ProjectResolver::for_path`] returns `Project`, never
//! `Option<Project>`. That signature *is* the design. `lattice-magit`'s
//! `workdir.rs` records what the alternative produced: eleven
//! hand-written copies of the same discovery, three of them passing a
//! file where a directory was required and silently defaulting — one of
//! which meant gutter diff signs had never worked, for any file, since
//! they landed. Every caller writing its own `.unwrap_or_else(cwd)` is
//! every caller getting a chance to write it wrong. A consumer that
//! cannot express "no project" cannot get "no project" wrong.
//!
//! ## Why no `gix`
//!
//! `.git` is the first marker, and a marker walk is sufficient: a `.git`
//! entry marks a worktree root whether it is a directory (ordinary
//! clone) or a file (submodule, `git worktree add`). The walk and
//! `gix::discover` therefore agree in every case a user will meet; they
//! diverge only under `GIT_WORK_TREE` / `GIT_DIR`, `core.worktree`, and
//! ceiling directories.
//!
//! That divergence is accepted deliberately. Only three crates depend on
//! `lattice-vcs` today, and every crate depends on this one — routing
//! resolution through `gix` would pull a heavy compile-time and
//! binary-size dependency behind `lattice-core` and therefore into the
//! whole workspace, to walk up a directory tree. `magit` is the one
//! consumer for which the distinction is load-bearing, and it keeps its
//! own `gix` discovery because it needs the `Repository` object anyway.
//!
//! [`ProjectResolver`] is a trait, so this is reversible: a
//! `gix`-backed impl can be registered later without any consumer
//! changing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The markers a project root is recognised by, in priority order
/// within a single directory.
///
/// `.git` first. `.lattice` is ours (it already marked a workspace root
/// for the persistent-config loader before this module existed). The
/// rest are the ecosystem manifests whose presence means "the thing
/// above this is not my project".
pub const DEFAULT_ROOT_MARKERS: &[&str] = &[
    ".git",
    ".hg",
    ".jj",
    ".lattice",
    "Cargo.toml",
    "go.work",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "flake.nix",
];

/// How a [`Project`]'s root was decided — for `:project-root`, for
/// diagnostics, and for the rare consumer that legitimately cares
/// (magit wants a VCS root specifically; a terminal does not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    /// A marker file or directory was found. Carries which one, so
    /// "why is my project root here" is answerable.
    Marker(String),
    /// No marker anywhere up the tree; the working directory stands in.
    Pwd,
}

/// The project a path belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub kind: ProjectKind,
}

impl Project {
    /// Whether this root was found rather than fallen back to. A
    /// consumer that wants to say "not in a project" in its UI asks
    /// this — as opposed to being handed an `Option` it must decide
    /// what to do with.
    pub fn is_rooted(&self) -> bool {
        matches!(self.kind, ProjectKind::Marker(_))
    }
}

/// Resolves paths to projects.
///
/// `&self` throughout and `Send + Sync`, so any thread may ask: this is
/// consulted off the actor thread by subsystems that spawn processes,
/// and the cache write takes a short mutex never held across an await.
pub trait ProjectResolver: Send + Sync + std::fmt::Debug {
    /// The project containing `path`, which may be a file or a
    /// directory. Total — see the module docs.
    fn for_path(&self, path: &Path) -> Project;

    /// Re-point the working directory the [`ProjectKind::Pwd`] fallback
    /// uses. Called on `:cd`. Drops the cache, because entries that
    /// fell back to the old pwd are now wrong.
    fn set_pwd(&self, pwd: PathBuf);

    /// Replace the marker set. Called when `project.root-markers`
    /// changes, and at boot once config has been applied.
    ///
    /// The markers cannot be fixed at construction because of a
    /// genuine ordering knot: the persistent-config loader finds
    /// `.lattice/config.toml` *by resolving a project root*, so the
    /// resolver has to exist before the config that configures it has
    /// been read. It is built with the defaults and re-pointed here —
    /// which is also why this drops the cache.
    fn set_markers(&self, markers: Vec<String>);

    /// Drop every cached answer — `:project-refresh`, after a `git init`
    /// mid-session. The cache is an optimisation and never a source of
    /// truth, so this changes latency and nothing else.
    fn invalidate(&self);
}

/// Per the `ServiceRegistry` Arc/TypeId convention: register and look up
/// under this exact alias.
pub type ProjectResolverHandle = Arc<dyn ProjectResolver>;

/// The project root containing the process's working directory, over
/// the default markers.
///
/// The **boot-time** answer, for the persistent-config loader: it runs
/// before any buffer exists (so there is nothing to resolve from) and
/// before config has been read (so `project.root-markers` is not
/// available yet — that config is what this call is on the way to
/// loading). Everything after boot goes through the registered
/// [`ProjectResolverHandle`] instead, which is buffer-aware and honours
/// both `:cd` and the configured markers.
///
/// PR.2: this replaces `Editor::workspace_root_from_cwd`, which
/// hand-walked for `.git` / `.lattice` and which each renderer called
/// independently. Living here rather than on the host is the point —
/// a renderer should not own a rule about where projects begin.
///
/// `None` only when the working directory itself is unreadable.
pub fn root_from_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(
        MarkerResolver::with_default_markers(cwd.clone())
            .for_path(&cwd)
            .root,
    )
}

/// The built-in [`ProjectResolver`]: walk up for a marker, else pwd.
pub struct MarkerResolver {
    /// Guarded because `project.root-markers` re-points it; see
    /// [`ProjectResolver::set_markers`].
    markers: Mutex<Vec<String>>,
    /// Guarded because `:cd` re-points it; see
    /// [`ProjectResolver::set_pwd`].
    pwd: Mutex<PathBuf>,
    /// Keyed by **directory**, not file, so every buffer in a directory
    /// shares one entry and the walk runs once per directory per
    /// session.
    cache: Mutex<HashMap<PathBuf, Project>>,
}

impl std::fmt::Debug for MarkerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkerResolver")
            .field(
                "markers",
                &self.markers.lock().map(|m| m.len()).unwrap_or_default(),
            )
            .field(
                "cached",
                &self.cache.lock().map(|c| c.len()).unwrap_or_default(),
            )
            .finish_non_exhaustive()
    }
}

impl MarkerResolver {
    /// Build a resolver over `markers`, falling back to `pwd`.
    ///
    /// The marker list arrives as a parameter rather than being read
    /// from a typed option here because `lattice-config` depends on
    /// *this* crate — core cannot name the option type. The host reads
    /// `project.root-markers` and passes it in, which is also what
    /// keeps this crate free of a config dependency for one
    /// `Vec<String>`.
    pub fn new(markers: Vec<String>, pwd: PathBuf) -> Self {
        Self {
            markers: Mutex::new(markers),
            pwd: Mutex::new(pwd),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The default marker set ([`DEFAULT_ROOT_MARKERS`]) over `pwd`.
    pub fn with_default_markers(pwd: PathBuf) -> Self {
        Self::new(
            DEFAULT_ROOT_MARKERS
                .iter()
                .map(|m| (*m).to_string())
                .collect(),
            pwd,
        )
    }

    /// The directory to start walking from: `path` itself when it is a
    /// directory, else its parent.
    ///
    /// A path that does not exist yet (a buffer for a file not saved
    /// yet) is not a directory, so it walks from its parent — which is
    /// the wanted answer, not a special case.
    ///
    /// **Relative input is made absolute against pwd first**, and that
    /// is load-bearing rather than tidiness. A relative path walks up to
    /// the empty path, and `Path::new("").join(".git").exists()` is a
    /// test against the *process's* working directory — so a relative
    /// path would silently root wherever the process happened to be
    /// started, reporting an empty root while looking like it worked.
    /// That is the same silent-resolution-against-the-wrong-base bug
    /// `magit/workdir.rs` exists to prevent; the totality test caught
    /// this one.
    fn start_dir(&self, path: &Path) -> PathBuf {
        let absolute: PathBuf = if path.is_absolute() {
            path.to_path_buf()
        } else {
            match self.pwd.lock() {
                Ok(pwd) => pwd.join(path),
                Err(_) => return PathBuf::from("/"),
            }
        };
        if absolute.is_dir() {
            absolute
        } else {
            absolute.parent().map(Path::to_path_buf).unwrap_or(absolute)
        }
    }

    /// The first marker `dir` directly contains, if any.
    ///
    /// Refuses a relative `dir` outright. `start_dir` already
    /// absolutises, so reaching here with one would mean the walk
    /// produced it — and `Path::new("").join(".git")` tests the
    /// process's working directory, which is never what was asked.
    /// Belt and braces on the same bug, because the cost is one
    /// comparison and the failure is silent.
    fn marker_in(&self, dir: &Path) -> Option<String> {
        if !dir.is_absolute() {
            return None;
        }
        self.markers
            .lock()
            .ok()?
            .iter()
            .find(|m| dir.join(m.as_str()).exists())
            .cloned()
    }

    fn fallback(&self) -> Project {
        Project {
            root: self
                .pwd
                .lock()
                .map(|p| p.clone())
                .unwrap_or_else(|_| PathBuf::from(".")),
            kind: ProjectKind::Pwd,
        }
    }
}

impl ProjectResolver for MarkerResolver {
    fn for_path(&self, path: &Path) -> Project {
        let start = self.start_dir(path);

        if let Ok(cache) = self.cache.lock()
            && let Some(hit) = cache.get(&start)
        {
            return hit.clone();
        }

        // Walk up, remembering what we passed. Every directory between
        // `start` and the hit was checked and had no marker, so they all
        // resolve to the same root — caching them here means opening
        // fifty files across a tree costs one walk, not fifty.
        let mut passed: Vec<PathBuf> = Vec::new();
        let mut cursor: Option<&Path> = Some(start.as_path());
        let mut found: Option<Project> = None;

        while let Some(dir) = cursor {
            if let Some(marker) = self.marker_in(dir) {
                found = Some(Project {
                    root: dir.to_path_buf(),
                    kind: ProjectKind::Marker(marker),
                });
                break;
            }
            passed.push(dir.to_path_buf());
            cursor = dir.parent();
        }

        // The pwd fallback is deliberately NOT cached against the
        // directories we passed: it is not a property of those
        // directories, and `set_pwd` would have to know which entries
        // to expire. It clears everything instead, which is only
        // correct if a Pwd answer is never the thing being cleared
        // selectively.
        let Some(project) = found else {
            return self.fallback();
        };

        if let Ok(mut cache) = self.cache.lock() {
            for dir in passed {
                cache.insert(dir, project.clone());
            }
            cache.insert(project.root.clone(), project.clone());
        }
        project
    }

    fn set_pwd(&self, pwd: PathBuf) {
        if let Ok(mut p) = self.pwd.lock() {
            *p = pwd;
        }
        self.invalidate();
    }

    fn set_markers(&self, markers: Vec<String>) {
        if let Ok(mut m) = self.markers.lock() {
            *m = markers;
        }
        self.invalidate();
    }

    fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Per-test unique directory.
    ///
    /// The counter is not decoration: a timestamp alone collides under
    /// parallel `cargo test`, because two tests can enter this within
    /// the same nanosecond tick and then fight over the same tree.
    fn tempdir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lattice-project-{id}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn mkdirs(base: &Path, rel: &str) -> PathBuf {
        let p = base.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    fn resolver(pwd: &Path) -> MarkerResolver {
        MarkerResolver::with_default_markers(pwd.to_path_buf())
    }

    #[test]
    fn an_ordinary_repo_roots_at_the_worktree() {
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let deep = mkdirs(&repo, "src/inner");

        let r = resolver(&base);
        let got = r.for_path(&deep.join("main.rs"));
        assert_eq!(got.root, repo);
        assert_eq!(got.kind, ProjectKind::Marker(".git".to_string()));
        assert!(got.is_rooted());
        cleanup(&base);
    }

    #[test]
    fn a_git_file_roots_too() {
        // Submodules and `git worktree add` write `.git` as a FILE, not
        // a directory. Both mark a worktree root, which is why a marker
        // walk is sufficient and `gix` is not needed here.
        let base = tempdir();
        let sub = mkdirs(&base, "outer/vendor/sub");
        mkdirs(&base, "outer/.git");
        touch(&sub, ".git");
        let deep = mkdirs(&sub, "src");

        let r = resolver(&base);
        assert_eq!(r.for_path(&deep.join("lib.rs")).root, sub);
        cleanup(&base);
    }

    #[test]
    fn the_innermost_marker_wins() {
        // A crate inside a Cargo workspace is its own project. This is
        // deliberately the opposite of LSP's outermost-root rule: that
        // asks where a language server should start, this asks where
        // YOU are working.
        let base = tempdir();
        let ws = mkdirs(&base, "ws");
        touch(&ws, "Cargo.toml");
        mkdirs(&ws, ".git");
        let member = mkdirs(&ws, "crates/thing");
        touch(&member, "Cargo.toml");
        let src = mkdirs(&member, "src");

        let r = resolver(&base);
        assert_eq!(r.for_path(&src.join("lib.rs")).root, member);
        cleanup(&base);
    }

    #[test]
    fn a_repo_nested_in_a_repo_roots_at_the_inner_one() {
        let base = tempdir();
        let outer = mkdirs(&base, "outer");
        mkdirs(&outer, ".git");
        let inner = mkdirs(&outer, "vendor/inner");
        mkdirs(&inner, ".git");

        let r = resolver(&base);
        assert_eq!(r.for_path(&inner.join("a.rs")).root, inner);
        assert_eq!(r.for_path(&outer.join("b.rs")).root, outer);
        cleanup(&base);
    }

    #[test]
    fn no_marker_anywhere_falls_back_to_pwd() {
        let base = tempdir();
        let lonely = mkdirs(&base, "nothing/here");
        let pwd = mkdirs(&base, "elsewhere");

        let r = resolver(&pwd);
        let got = r.for_path(&lonely.join("scratch.rs"));
        assert_eq!(got.root, pwd);
        assert_eq!(got.kind, ProjectKind::Pwd);
        assert!(!got.is_rooted());
        cleanup(&base);
    }

    #[test]
    fn a_directory_argument_resolves_from_itself() {
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let sub = mkdirs(&repo, "sub");

        let r = resolver(&base);
        // Passing the directory must not walk from its PARENT — that is
        // exactly the file-vs-directory confusion magit's workdir.rs
        // was written to make unrepresentable.
        assert_eq!(r.for_path(&sub).root, repo);
        cleanup(&base);
    }

    #[test]
    fn a_path_that_does_not_exist_yet_resolves_from_its_parent() {
        // A buffer for a file that has never been saved.
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let src = mkdirs(&repo, "src");

        let r = resolver(&base);
        assert_eq!(r.for_path(&src.join("brand-new.rs")).root, repo);
        cleanup(&base);
    }

    #[test]
    fn resolution_is_always_total() {
        // The property the signature exists to guarantee: no input
        // yields "no project", including paths that cannot exist.
        let base = tempdir();
        let r = resolver(&base);
        for p in [
            Path::new("/"),
            Path::new("relative/thing.rs"),
            Path::new(""),
            &base.join("no/such/place/at/all.rs"),
        ] {
            let got = r.for_path(p);
            assert!(
                !got.root.as_os_str().is_empty(),
                "{p:?} produced an empty root"
            );
        }
        cleanup(&base);
    }

    #[test]
    fn a_relative_path_resolves_against_pwd_not_the_process_cwd() {
        // Regression, found by `resolution_is_always_total`. A relative
        // path walks up to the EMPTY path, and
        // `Path::new("").join(".git").exists()` tests the process's
        // working directory — so `relative/thing.rs` rooted at "" for
        // any test run from inside a git repo, i.e. always. It reported
        // an empty root while looking like it had worked.
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let src = mkdirs(&repo, "src");

        // pwd is inside the repo; the relative path is relative to it.
        let r = resolver(&src);
        let got = r.for_path(Path::new("thing.rs"));
        assert_eq!(got.root, repo, "relative paths resolve against pwd");

        // And a relative path with nothing above it must still not
        // reach the process cwd — it falls back to pwd.
        let bare = tempdir();
        let r2 = resolver(&bare);
        let got2 = r2.for_path(Path::new("deep/er/thing.rs"));
        assert_eq!(got2.root, bare);
        assert_eq!(got2.kind, ProjectKind::Pwd);

        cleanup(&base);
        cleanup(&bare);
    }

    #[test]
    fn the_walk_is_cached_for_every_directory_it_passed() {
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let deep = mkdirs(&repo, "a/b/c");

        let r = resolver(&base);
        r.for_path(&deep.join("f.rs"));

        // `a`, `a/b`, `a/b/c` and the root itself were all resolved by
        // that one walk — fifty files across a tree cost one walk, not
        // fifty.
        let cache = r.cache.lock().unwrap();
        for rel in ["a", "a/b", "a/b/c"] {
            assert!(
                cache.contains_key(&repo.join(rel)),
                "{rel} should have been cached by the walk"
            );
        }
        assert!(cache.contains_key(&repo));
        cleanup(&base);
    }

    #[test]
    fn a_cache_hit_answers_the_same_as_the_walk() {
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let src = mkdirs(&repo, "src");

        let r = resolver(&base);
        let first = r.for_path(&src.join("a.rs"));
        let second = r.for_path(&src.join("b.rs"));
        assert_eq!(first, second);
        cleanup(&base);
    }

    #[test]
    fn invalidate_lets_a_new_marker_be_seen() {
        // `git init` mid-session: the answer was cached before the
        // marker existed.
        let base = tempdir();
        let dir = mkdirs(&base, "not-yet");
        let pwd = mkdirs(&base, "pwd");

        let r = resolver(&pwd);
        assert_eq!(r.for_path(&dir.join("a.rs")).kind, ProjectKind::Pwd);

        mkdirs(&dir, ".git");
        r.invalidate();
        assert_eq!(r.for_path(&dir.join("a.rs")).root, dir);
        cleanup(&base);
    }

    #[test]
    fn set_pwd_repoints_the_fallback_and_drops_stale_answers() {
        let base = tempdir();
        let lonely = mkdirs(&base, "lonely");
        let before = mkdirs(&base, "before");
        let after = mkdirs(&base, "after");

        let r = resolver(&before);
        assert_eq!(r.for_path(&lonely.join("a.rs")).root, before);

        r.set_pwd(after.clone());
        assert_eq!(
            r.for_path(&lonely.join("a.rs")).root,
            after,
            "a `:cd` must re-point answers that had fallen back to pwd"
        );
        cleanup(&base);
    }

    #[test]
    fn a_marker_rooted_answer_survives_a_cd() {
        // `:cd` changes only the fallback. A path that HAS a project
        // must not follow the working directory around.
        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let elsewhere = mkdirs(&base, "elsewhere");

        let r = resolver(&base);
        assert_eq!(r.for_path(&repo.join("a.rs")).root, repo);
        r.set_pwd(elsewhere);
        assert_eq!(r.for_path(&repo.join("a.rs")).root, repo);
        cleanup(&base);
    }

    #[test]
    fn set_markers_repoints_the_walk_and_drops_stale_answers() {
        // The ordering knot this exists for: the persistent-config
        // loader finds `.lattice/config.toml` BY resolving a project
        // root, so the resolver is built with defaults before the
        // config that configures it has been read, then re-pointed.
        let base = tempdir();
        let proj = mkdirs(&base, "proj");
        touch(&proj, "WORKSPACE.bazel");
        let deep = mkdirs(&proj, "src");

        let r = resolver(&base);
        // Not a default marker, so this falls back to pwd first.
        assert_eq!(r.for_path(&deep.join("a.cc")).kind, ProjectKind::Pwd);

        r.set_markers(vec!["WORKSPACE.bazel".to_string()]);
        let got = r.for_path(&deep.join("a.cc"));
        assert_eq!(got.root, proj, "the cached pwd answer must not survive");
        assert_eq!(got.kind, ProjectKind::Marker("WORKSPACE.bazel".to_string()));

        // And the reverse: dropping a marker un-roots what it rooted.
        r.set_markers(vec![".git".to_string()]);
        assert_eq!(r.for_path(&deep.join("a.cc")).kind, ProjectKind::Pwd);
        cleanup(&base);
    }

    #[test]
    fn a_custom_marker_set_is_honoured() {
        // What `project.root-markers` buys: a new ecosystem needs a
        // config line, not a release.
        let base = tempdir();
        let proj = mkdirs(&base, "proj");
        touch(&proj, "WORKSPACE.bazel");
        let deep = mkdirs(&proj, "src/x");

        let r = MarkerResolver::new(vec!["WORKSPACE.bazel".to_string()], base.clone());
        let got = r.for_path(&deep.join("a.cc"));
        assert_eq!(got.root, proj);
        assert_eq!(got.kind, ProjectKind::Marker("WORKSPACE.bazel".to_string()));
        cleanup(&base);
    }

    #[test]
    fn marker_priority_is_declaration_order_within_a_directory() {
        // Both present in one directory: the earlier marker names the
        // kind, so `:project-root` reports `.git` rather than whichever
        // the filesystem happened to yield first.
        let base = tempdir();
        let proj = mkdirs(&base, "proj");
        mkdirs(&proj, ".git");
        touch(&proj, "Cargo.toml");

        let r = resolver(&base);
        assert_eq!(
            r.for_path(&proj.join("a.rs")).kind,
            ProjectKind::Marker(".git".to_string())
        );
        cleanup(&base);
    }

    #[test]
    fn the_resolver_is_shareable_across_threads() {
        // The trait bound that lets subsystems ask off the actor thread.
        fn assert_shareable<T: Send + Sync + std::fmt::Debug>() {}
        assert_shareable::<MarkerResolver>();

        let base = tempdir();
        let repo = mkdirs(&base, "repo");
        mkdirs(&repo, ".git");
        let handle: ProjectResolverHandle = Arc::new(resolver(&base));

        let threads: Vec<_> = (0..8)
            .map(|i| {
                let h = handle.clone();
                let p = repo.join(format!("f{i}.rs"));
                std::thread::spawn(move || h.for_path(&p).root)
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), repo);
        }
        cleanup(&base);
    }
}

/// OA.23 — where a line of a composed buffer came from.
///
/// A multibuffer shows excerpts of other files, so a consumer holding composed
/// coordinates cannot say which file it is looking at. The agenda is the case
/// that needs this: rewriting a headline in place propagates through the
/// excerpt, but writing a planning line BELOW it targets a line the view does
/// not contain — and the view is a synthetic buffer with no path of its own.
///
/// An abstract handle for the same reason [`ProjectResolver`] is one: the
/// plugin host must be able to answer the question without depending on
/// `lattice-multibuffer`, which sits above it. Whoever owns multibuffers
/// implements this and wires it in at boot; a host with none wired answers
/// `None`, which is the honest degradation rather than a panic.
/// Where a composed line came from: the source document, its file, and the
/// 0-based line within it.
///
/// `source` is the identity to ACT on and `path` the identity to show. They
/// are not interchangeable: a multibuffer's sources are documents the view
/// owns and no buffer store holds, so a caller that resolved only the path
/// and then addressed the file by name would be editing a *different*
/// document — the user's own buffer for that file, if any — and the view's
/// `:w` could later overwrite one with the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcerptSource {
    /// The source document's id. Addressable by `Effect::ApplyEdit`.
    pub source: crate::buffers::BufferId,
    pub path: std::path::PathBuf,
    pub line: u32,
}

pub trait ExcerptSourceResolver: Send + Sync + std::fmt::Debug {
    /// Where composed `line` of `buffer` came from.
    ///
    /// `None` when `buffer` is not composed, when `line` is not source text (a
    /// header or separator row belongs to the view, not to any file), or when
    /// the source has no path. All three are ordinary answers: a caller asks
    /// about the cursor's line, and a cursor can be anywhere.
    fn excerpt_source(&self, buffer: crate::buffers::BufferId, line: u32) -> Option<ExcerptSource>;

    /// OA.23b: one line of a source document, without its trailing newline.
    ///
    /// The read that pairs with acting on [`ExcerptSource::source`]. A caller
    /// deciding what to write at the line below a headline has to see what is
    /// there, and that line is outside every excerpt — so neither the composed
    /// text nor a file read answers it. A file read is wrong twice over: it
    /// misses edits the view has made and not yet saved.
    ///
    /// `source` is an [`ExcerptSource::source`], not an arbitrary buffer:
    /// `None` for anything no view owns.
    fn source_line(&self, source: crate::buffers::BufferId, line: u32) -> Option<String>;
}

/// Shared handle to an [`ExcerptSourceResolver`].
pub type ExcerptSourceResolverHandle = Arc<dyn ExcerptSourceResolver>;
