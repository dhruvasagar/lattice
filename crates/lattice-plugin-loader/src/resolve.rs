//! PM.6: the source resolver — a declared source becomes something the
//! build service can consume.
//!
//! Design: [`plugin-manager.md`](../../../docs/dev/architecture/plugin-manager.md)
//! §4. Three source kinds, and one of them deliberately does not end in
//! a build:
//!
//! - [`PluginSource::Local`] — the directory *is* the source tree.
//!   Built in place; the tree is never copied.
//! - [`PluginSource::Git`] — cloned/fetched into a source cache and
//!   checked out at `rev`, then built like `Local`.
//! - [`PluginSource::Prebuilt`] — a ready `.wasm` downloaded straight
//!   into the user root. **No build, no toolchain** (§7), which is the
//!   whole point of the kind: it is how a user on a machine with no
//!   Rust installs a plugin at all.
//!
//! So [`resolve`] returns [`Resolved`], not a path: "where is the
//! source" and "here is the artifact, skip the build" are different
//! answers and collapsing them would force `Prebuilt` to invent a
//! source tree it does not have.
//!
//! ## Why git is a subprocess
//!
//! `gix` is already in the workspace, but only with read-only features
//! — `lattice-vcs` inspects repositories, it does not clone them.
//! Turning on `blocking-network-client` would pull TLS and a network
//! stack into a crate that has neither, to replace a binary every
//! developer running a `Git` plugin source already has. It is also the
//! same bargain the build service already strikes with `cargo`: you
//! have the toolchain for the source kind you asked for.
//!
//! Everything here blocks. Callers run it on `spawn_blocking`.

use std::path::{Path, PathBuf};

/// Where a plugin comes from (mirrors the `plugin-source` WIT variant
/// PM.7 exposes to `init.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// A cargo project on disk. Built in place.
    Local(PathBuf),
    /// A git repository, optionally pinned to a revision.
    Git { url: String, rev: Option<String> },
    /// A URL serving a ready-built `.wasm` component.
    Prebuilt { url: String },
}

/// Whether a source that is already in the cache may be advanced from the
/// network.
///
/// The distinction exists because "resolve" is asked for by two callers that
/// want opposite things. Boot and rebuild want *what the user already has* —
/// deterministic, offline-capable, no per-start network round trip. The
/// `update` verb exists precisely to go and get something newer, and is the
/// only caller that should.
///
/// A **pinned** rev ignores this entirely: a pin is the answer to "which
/// commit", so there is nothing for either policy to decide. Both will still
/// fetch-and-move when the checkout is not at the pin (the user edited the pin
/// in `init.rs`), and neither touches the network when it already is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshPolicy {
    /// Use the checkout on disk as it stands. No network for a repository
    /// that is already cloned.
    #[default]
    UseCache,
    /// Fetch and move an unpinned checkout to the tracked head.
    Update,
}

/// What a source resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A source tree for the build service ([`crate::build_plugin`]).
    Source(PathBuf),
    /// An artifact already in place; the build is skipped entirely.
    Artifact(PathBuf),
}

/// Downloads a URL to a file.
///
/// Behind a trait for the same reason the build service's toolchain is
/// (PM.5): the interesting behaviour around it — where the bytes land,
/// what manifest gets synthesised, what happens when the download fails
/// — should be testable without reaching the network, which is neither
/// fast nor available in CI sandboxes.
pub trait Fetcher: Send + Sync {
    /// Fetch `url`, writing the body to `dest`.
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String>;
}

/// The real fetcher.
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;
        let mut body = response.body_mut().as_reader();
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        // Download to a sibling temp path and rename into place, so an
        // interrupted transfer cannot leave a truncated `.wasm` that
        // looks like a valid cached artifact on the next boot.
        let tmp = dest.with_extension("wasm.part");
        let mut file =
            std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        std::io::copy(&mut body, &mut file).map_err(|e| format!("download {url}: {e}"))?;
        drop(file);
        std::fs::rename(&tmp, dest).map_err(|e| format!("finalise {}: {e}", dest.display()))?;
        Ok(())
    }
}

/// Runs git. Behind a trait so the resolver's cache/checkout logic is
/// testable against a fake, and against a real local repository, with
/// no network.
pub trait GitRunner: Send + Sync {
    /// Run `git <args>` in `cwd`; return stdout on success.
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<String, String>;
}

/// The real runner.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<String, String> {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .map_err(|e| format!("failed to run git: {e}. Is git installed?"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "git {} failed ({}): {}",
                args.join(" "),
                output.status,
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// The source-cache directory for `name`'s git checkout.
pub fn git_cache_dir(cache_root: &Path, name: &str) -> PathBuf {
    cache_root.join(name)
}

/// Resolve `source` for the plugin called `name`.
///
/// `cache_root` holds git checkouts (`~/.cache/lattice/sources/`);
/// `user_root` is the plugin cache (`~/.config/lattice/plugins/`) a
/// `Prebuilt` artifact lands in.
///
/// `policy` decides whether an already-cloned git source is advanced from the
/// network; it is meaningless for `Local` (the tree IS the source) and for
/// `Prebuilt` (which re-downloads whenever the artifact is missing).
///
/// Blocking (clone / fetch / download). Run on `spawn_blocking`.
pub fn resolve(
    git: &dyn GitRunner,
    fetcher: &dyn Fetcher,
    source: &PluginSource,
    name: &str,
    cache_root: &Path,
    user_root: &Path,
    policy: RefreshPolicy,
) -> Result<Resolved, String> {
    match source {
        PluginSource::Local(path) => {
            if !path.is_dir() {
                return Err(format!(
                    "local source is not a directory: {}",
                    path.display()
                ));
            }
            Ok(Resolved::Source(path.clone()))
        }
        PluginSource::Git { url, rev } => {
            let dir = resolve_git(git, url, rev.as_deref(), name, cache_root, policy)?;
            Ok(Resolved::Source(dir))
        }
        PluginSource::Prebuilt { url } => {
            let artifact = resolve_prebuilt(fetcher, url, name, user_root)?;
            Ok(Resolved::Artifact(artifact))
        }
    }
}

/// Clone `name`'s checkout, or bring an existing one to where `rev` and
/// `policy` say it should be.
///
/// Only reach the network when the answer requires it:
///
/// | on disk | pinned | policy | network |
/// |---|---|---|---|
/// | absent | either | either | clone |
/// | present | at the pin | either | none |
/// | present | pin differs | either | fetch + checkout |
/// | present | unpinned | `UseCache` | **none** |
/// | present | unpinned | `Update` | fetch + move to the tracked head |
///
/// **The unpinned rows are the 2026-09-14 fix.** Every re-resolve used to
/// `fetch` an unpinned checkout and then stop — the `checkout` below is
/// reachable only with a pin — so the objects arrived and local `HEAD` never
/// moved. An unpinned plugin was frozen at the commit it was first cloned at,
/// for the life of the checkout, while paying a network round trip on every
/// single boot to stay that way. Both halves are wrong and they are each
/// other's fix: `UseCache` drops the pointless fetch, and `Update` follows its
/// fetch with the move that makes it mean something.
///
/// `reset --hard FETCH_HEAD` rather than a merge or a pull: the checkout is a
/// cache the editor owns, never a working tree the user edits, so the tracked
/// head is simply what it should contain. A merge could conflict, and there is
/// nobody to resolve it.
fn resolve_git(
    git: &dyn GitRunner,
    url: &str,
    rev: Option<&str>,
    name: &str,
    cache_root: &Path,
    policy: RefreshPolicy,
) -> Result<PathBuf, String> {
    let dir = git_cache_dir(cache_root, name);
    if !dir.join(".git").is_dir() {
        clone_git(git, url, rev, &dir)?;
        if let Some(rev) = rev {
            git.run(&dir, &["checkout", "--detach", rev])?;
        }
        return Ok(dir);
    }

    match rev {
        // Pinned. The pin is the answer whatever the policy, so the only
        // question is whether we are already at it.
        Some(rev) => {
            if git.run(&dir, &["rev-parse", "HEAD"]).ok().as_deref() == Some(rev) {
                tracing::debug!(plugin = name, rev, "git: already at the requested rev");
                return Ok(dir);
            }
            git.run(&dir, &["fetch", "--tags", "origin"])?;
            git.run(&dir, &["checkout", "--detach", rev])?;
        }
        // Unpinned. `UseCache` is every boot and every rebuild; `Update` is
        // the `update` verb, and nothing else.
        None => {
            if policy == RefreshPolicy::Update {
                git.run(&dir, &["fetch", "--tags", "origin"])?;
                git.run(&dir, &["reset", "--hard", "FETCH_HEAD"])?;
            } else {
                tracing::debug!(plugin = name, "git: using the cached checkout");
            }
        }
    }
    Ok(dir)
}

/// Clone into `dir`, shallow unless a rev is pinned.
///
/// Full history when a rev is pinned (a shallow clone may not contain it);
/// shallow when tracking the default branch, which is the common case and much
/// cheaper.
fn clone_git(git: &dyn GitRunner, url: &str, rev: Option<&str>, dir: &Path) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| format!("bad cache path {}", dir.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let dir_str = dir.to_string_lossy().to_string();
    let mut args = vec!["clone"];
    if rev.is_none() {
        args.extend(["--depth", "1"]);
    }
    args.extend([url, dir_str.as_str()]);
    git.run(parent, &args)?;
    Ok(())
}

/// Download a prebuilt component and give it a manifest.
///
/// The manifest is **synthesised** with the plugin's id and nothing
/// else — in particular no capabilities. A downloaded binary is the
/// least-known code the editor runs, so it gets the smallest grant that
/// still lets it load; a plugin that needs more ships a real
/// `plugin.toml` through a `Local` or `Git` source, where the user can
/// read what it asked for before it runs.
///
/// An existing manifest is never overwritten: a user who hand-edited
/// one to grant a capability should not have that silently reverted by
/// a re-download.
fn resolve_prebuilt(
    fetcher: &dyn Fetcher,
    url: &str,
    name: &str,
    user_root: &Path,
) -> Result<PathBuf, String> {
    let dir = user_root.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let artifact = dir.join(format!("{name}.wasm"));
    fetcher.fetch(url, &artifact)?;
    let manifest = dir.join("plugin.toml");
    if !manifest.exists() {
        std::fs::write(&manifest, format!("id = \"{name}\"\n"))
            .map_err(|e| format!("write {}: {e}", manifest.display()))?;
    }
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("lattice-pm6-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Records the git commands issued, so the tests can assert on the
    /// *shape* of the interaction (shallow vs full, fetch skipped)
    /// without a network or a real repository.
    #[derive(Default)]
    struct FakeGit {
        calls: Mutex<Vec<String>>,
        head: Mutex<Option<String>>,
        clone_makes_repo: bool,
    }

    impl FakeGit {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl GitRunner for FakeGit {
        fn run(&self, cwd: &Path, args: &[&str]) -> Result<String, String> {
            self.calls.lock().unwrap().push(args.join(" "));
            if args[0] == "clone" && self.clone_makes_repo {
                let dest = PathBuf::from(args[args.len() - 1]);
                std::fs::create_dir_all(dest.join(".git")).unwrap();
                let _ = cwd;
            }
            if args[0] == "rev-parse" {
                return self
                    .head
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| "no head".to_string());
            }
            Ok(String::new())
        }
    }

    struct FakeFetcher {
        body: Vec<u8>,
        fail: bool,
        calls: AtomicUsize,
    }

    impl Fetcher for FakeFetcher {
        fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(format!("GET {url}: 404"));
            }
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::write(dest, &self.body).unwrap();
            Ok(())
        }
    }

    fn fetcher(body: &[u8]) -> FakeFetcher {
        FakeFetcher {
            body: body.to_vec(),
            fail: false,
            calls: AtomicUsize::new(0),
        }
    }

    // --- Local ---------------------------------------------------------

    #[test]
    fn a_local_source_resolves_to_itself_and_is_not_copied() {
        let root = tempdir("local");
        let src = root.join("my-plugin");
        std::fs::create_dir_all(&src).unwrap();

        let got = resolve(
            &FakeGit::default(),
            &fetcher(b""),
            &PluginSource::Local(src.clone()),
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        assert_eq!(got, Resolved::Source(src.clone()));
        assert!(
            !root.join("cache").exists(),
            "a local source must be built in place, never copied into the cache"
        );
    }

    #[test]
    fn a_missing_local_source_is_a_clear_error() {
        let root = tempdir("local-missing");
        let err = resolve(
            &FakeGit::default(),
            &fetcher(b""),
            &PluginSource::Local(root.join("nope")),
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap_err();
        assert!(err.contains("not a directory"), "got: {err}");
    }

    // --- Git -----------------------------------------------------------

    #[test]
    fn an_unpinned_git_source_clones_shallow() {
        let root = tempdir("git-shallow");
        let git = FakeGit {
            clone_makes_repo: true,
            ..Default::default()
        };
        let got = resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: None,
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        assert_eq!(got, Resolved::Source(root.join("cache").join("demo")));
        let calls = git.calls();
        assert!(calls[0].contains("--depth 1"), "got: {calls:?}");
        assert!(
            !calls.iter().any(|c| c.starts_with("checkout")),
            "no rev pinned ⇒ nothing to check out: {calls:?}"
        );
    }

    #[test]
    fn a_pinned_git_source_clones_full_then_checks_out() {
        // A shallow clone may not contain the pinned rev, so pinning has
        // to opt out of the cheap path.
        let root = tempdir("git-pinned");
        let git = FakeGit {
            clone_makes_repo: true,
            ..Default::default()
        };
        resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: Some("abc123".into()),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let calls = git.calls();
        assert!(
            !calls[0].contains("--depth"),
            "pinned ⇒ full clone: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "checkout --detach abc123"),
            "got: {calls:?}"
        );
    }

    #[test]
    fn a_re_resolve_at_the_same_rev_touches_no_network() {
        // The warm-boot requirement, applied to the network: a pinned
        // plugin must not fetch on every start.
        let root = tempdir("git-warm");
        let dir = root.join("cache").join("demo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let git = FakeGit {
            head: Mutex::new(Some("abc123".into())),
            ..Default::default()
        };

        resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: Some("abc123".into()),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let calls = git.calls();
        assert_eq!(calls, vec!["rev-parse HEAD".to_string()]);
        assert!(
            !calls.iter().any(|c| c.starts_with("fetch")),
            "an unchanged rev must not fetch: {calls:?}"
        );
    }

    #[test]
    fn a_changed_rev_fetches_then_checks_out() {
        let root = tempdir("git-move");
        let dir = root.join("cache").join("demo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let git = FakeGit {
            head: Mutex::new(Some("old".into())),
            ..Default::default()
        };

        resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: Some("new".into()),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let calls = git.calls();
        assert!(calls.iter().any(|c| c.starts_with("fetch")), "{calls:?}");
        assert!(
            calls.iter().any(|c| c == "checkout --detach new"),
            "{calls:?}"
        );
    }

    /// Boot and rebuild resolve with `UseCache`, and an unpinned checkout that
    /// is already on disk is then answered entirely from it.
    ///
    /// This asserted the opposite until 2026-09-14 — that a warm unpinned
    /// checkout fetches — which was true and useless: the fetch was never
    /// followed by anything that moved `HEAD`, so it bought a network round
    /// trip per boot and changed nothing. See [`super::resolve_git`].
    #[test]
    fn a_warm_unpinned_checkout_touches_no_network_under_use_cache() {
        let root = tempdir("git-head");
        let dir = root.join("cache").join("demo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let git = FakeGit::default();

        resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: None,
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        assert!(
            git.calls().is_empty(),
            "a cloned, unpinned checkout needs no git at all: {:?}",
            git.calls()
        );
    }

    /// `Update` is the policy the `update` verb passes, and it is the whole
    /// point: fetch AND move. A fetch on its own leaves the plugin exactly
    /// where it was, which is the bug this pair exists to keep fixed.
    #[test]
    fn an_unpinned_checkout_advances_to_the_fetched_head_under_update() {
        let root = tempdir("git-update");
        let dir = root.join("cache").join("demo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let git = FakeGit::default();

        resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: None,
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::Update,
        )
        .unwrap();

        let calls = git.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("fetch")),
            "got: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "reset --hard FETCH_HEAD"),
            "the fetch must be followed by the move that makes it mean \
             something: {calls:?}"
        );
    }

    /// A pin does not move, whatever the policy asks for. `update` on a pinned
    /// plugin is a no-op by design — the pin IS the answer to "which commit" —
    /// and a fetch here would be network traffic for a decision already made.
    #[test]
    fn a_pinned_checkout_at_its_rev_stays_put_even_under_update() {
        let root = tempdir("git-pinned-update");
        let dir = root.join("cache").join("demo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let git = FakeGit::default();
        *git.head.lock().unwrap() = Some("abc123".to_string());

        resolve(
            &git,
            &fetcher(b""),
            &PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: Some("abc123".into()),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::Update,
        )
        .unwrap();

        let calls = git.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("fetch")),
            "already at the pin ⇒ no network: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("reset")),
            "a pin never moves: {calls:?}"
        );
    }

    /// The fake asserts shape; this asserts the real thing works. A
    /// local repository exercises clone / fetch / checkout end to end
    /// with no network.
    #[test]
    fn a_real_local_repository_clones_and_checks_out() {
        let root = tempdir("git-real");
        let origin = root.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        let g = SystemGit;
        if g.run(&origin, &["init", "-q"]).is_err() {
            eprintln!("git unavailable; skipping");
            return;
        }
        let _ = g.run(&origin, &["config", "user.email", "t@example.invalid"]);
        let _ = g.run(&origin, &["config", "user.name", "t"]);
        std::fs::write(origin.join("plugin.toml"), "id = \"demo\"\n").unwrap();
        g.run(&origin, &["add", "."]).unwrap();
        g.run(&origin, &["commit", "-qm", "one"]).unwrap();
        let rev = g.run(&origin, &["rev-parse", "HEAD"]).unwrap();

        let got = resolve(
            &g,
            &fetcher(b""),
            &PluginSource::Git {
                url: origin.to_string_lossy().to_string(),
                rev: Some(rev.clone()),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let dir = match got {
            Resolved::Source(d) => d,
            other => panic!("expected a source tree, got {other:?}"),
        };
        assert!(dir.join("plugin.toml").is_file(), "the tree is checked out");
        assert_eq!(g.run(&dir, &["rev-parse", "HEAD"]).unwrap(), rev);

        // And a second resolve at the same rev is a no-op.
        let again = resolve(
            &g,
            &fetcher(b""),
            &PluginSource::Git {
                url: origin.to_string_lossy().to_string(),
                rev: Some(rev),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        );
        assert!(again.is_ok());
    }

    // --- Prebuilt ------------------------------------------------------

    #[test]
    fn a_prebuilt_source_lands_an_artifact_and_skips_the_build() {
        let root = tempdir("prebuilt");
        let user = root.join("user");
        let f = fetcher(b"\0asm-prebuilt");

        let got = resolve(
            &FakeGit::default(),
            &f,
            &PluginSource::Prebuilt {
                url: "https://example.invalid/demo.wasm".into(),
            },
            "demo",
            &root.join("cache"),
            &user,
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let artifact = user.join("demo").join("demo.wasm");
        assert_eq!(
            got,
            Resolved::Artifact(artifact.clone()),
            "Artifact, not Source — a prebuilt has no source tree to build"
        );
        assert_eq!(std::fs::read(&artifact).unwrap(), b"\0asm-prebuilt");
    }

    #[test]
    fn a_prebuilt_gets_a_synthesised_manifest_with_no_capabilities() {
        // A downloaded binary is the least-known code the editor runs,
        // so it gets the smallest grant that still lets it load.
        let root = tempdir("prebuilt-manifest");
        let user = root.join("user");
        resolve(
            &FakeGit::default(),
            &fetcher(b"x"),
            &PluginSource::Prebuilt {
                url: "https://example.invalid/demo.wasm".into(),
            },
            "demo",
            &root.join("cache"),
            &user,
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let manifest = std::fs::read_to_string(user.join("demo").join("plugin.toml")).unwrap();
        assert!(manifest.contains("id = \"demo\""));
        assert!(
            !manifest.contains("capabilit"),
            "a synthesised manifest must not grant anything: {manifest}"
        );
    }

    #[test]
    fn a_hand_edited_manifest_survives_a_re_download() {
        let root = tempdir("prebuilt-keep");
        let user = root.join("user");
        let dir = user.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), "id = \"demo\"\nhand = true\n").unwrap();

        resolve(
            &FakeGit::default(),
            &fetcher(b"x"),
            &PluginSource::Prebuilt {
                url: "https://example.invalid/demo.wasm".into(),
            },
            "demo",
            &root.join("cache"),
            &user,
            RefreshPolicy::UseCache,
        )
        .unwrap();

        let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
        assert!(
            manifest.contains("hand = true"),
            "a re-download must not silently revert a user's grant: {manifest}"
        );
    }

    #[test]
    fn a_failed_download_is_an_error_not_a_panic() {
        let root = tempdir("prebuilt-fail");
        let f = FakeFetcher {
            body: Vec::new(),
            fail: true,
            calls: AtomicUsize::new(0),
        };
        let err = resolve(
            &FakeGit::default(),
            &f,
            &PluginSource::Prebuilt {
                url: "https://example.invalid/gone.wasm".into(),
            },
            "demo",
            &root.join("cache"),
            &root.join("user"),
            RefreshPolicy::UseCache,
        )
        .unwrap_err();
        assert!(err.contains("404"), "got: {err}");
    }
}
