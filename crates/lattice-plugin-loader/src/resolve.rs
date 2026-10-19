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
    /// PRL.3: a component published as a GitHub release asset —
    /// `require-release` (`docs/dev/architecture/plugin-distribution.md` §3).
    /// `repo` is `owner/name`; `version` a tag (`v0.2.0`); `sha256` pins the
    /// bytes, not just the tag.
    Release {
        repo: String,
        version: Option<String>,
        sha256: Option<String>,
    },
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
    /// PRL.2: through the plugin host's hardened download core rather than a
    /// bare GET — connect / response / body timeouts and a byte ceiling, so a
    /// stalled server cannot hang the install task, and the body renamed into
    /// place only when complete, so an interrupted transfer cannot leave a
    /// truncated `.wasm` that looks like a cached artifact next boot.
    ///
    /// Any `https` host: the URL is one the user wrote in their own config,
    /// and following the server's redirects (a release host's CDN) is what
    /// any downloader does.
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        lattice_plugin_host::download_to_file(
            url,
            lattice_plugin_host::Hosts::AnyHttps,
            dest,
            None,
            lattice_plugin_host::DownloadLimits::default(),
        )
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
/// `cache_root` holds git checkouts (`~/.config/lattice/cache/sources/`);
/// `user_root` is the plugin cache (`~/.config/lattice/plugins/`) a
/// `Prebuilt` artifact lands in.
///
/// `policy` decides whether an already-cloned git source is advanced from the
/// network, and whether a `Prebuilt` already on disk is fetched again; it is
/// meaningless for `Local` (the tree IS the source).
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
            let artifact = resolve_prebuilt(fetcher, url, name, user_root, policy)?;
            Ok(Resolved::Artifact(artifact))
        }
        PluginSource::Release {
            repo,
            version,
            sha256,
        } => {
            let artifact = resolve_release(
                fetcher,
                repo,
                version.as_deref(),
                sha256.as_deref(),
                name,
                user_root,
                policy,
            )?;
            Ok(Resolved::Artifact(artifact))
        }
    }
}

/// PRL.3: where a release asset lives. GitHub-specific URLs are confined to
/// this one function, so a second forge is a second function (design §3).
fn release_asset_url(repo: &str, tag: &str, asset: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/{asset}")
}

/// The asset a release publishes for this editor's plugin API generation:
/// `<id>-<version>-api<M>.<m>.wasm` (design §2).
pub fn release_asset_name(name: &str, tag: &str) -> String {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    format!("{name}-{version}-api{}.wasm", lattice_wit::api_generation())
}

/// `owner/name`, and a tag, made of the characters GitHub allows — checked
/// before either is put into a URL.
fn check_release_spec(repo: &str, tag: &str) -> Result<(), String> {
    let safe = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if safe(owner) && safe(name) => {}
        _ => return Err(format!("release repo `{repo}` is not `owner/name`")),
    }
    if !safe(tag) {
        return Err(format!("release version `{tag}` is not a tag name"));
    }
    Ok(())
}

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// PRL.3: install `name` from a GitHub release of `repo`, tag `version`.
///
/// 1. The asset is named for this editor's API generation, so a release with
///    no build for it fails on the request, not at instantiation.
/// 2. Its published `.sha256` is fetched, then the component, and the bytes
///    must hash to it — and to the spec's `sha256` when one is pinned, so a
///    re-published tag is refused.
/// 3. The component must carry its manifest (PMF.1), whose `id` must be
///    `name`: the user asked for one plugin, and the release may hold another.
/// 4. Only then does it replace `<name>.wasm`, by rename. The plugin's
///    directory is NOT swapped wholesale — it holds the plugin's `data/` — so
///    every failure leaves the previous version in place and loading.
///
/// Under `UseCache`, an artifact installed from the same repo, tag, pin and
/// API generation is used as it is: no network on a warm boot.
fn resolve_release(
    fetcher: &dyn Fetcher,
    repo: &str,
    version: Option<&str>,
    sha256: Option<&str>,
    name: &str,
    user_root: &Path,
    policy: RefreshPolicy,
) -> Result<PathBuf, String> {
    let Some(tag) = version else {
        return Err(format!(
            "release `{repo}`: name a version (`version: Some(\"v0.2.0\")`)"
        ));
    };
    check_release_spec(repo, tag)?;
    let pin = sha256.map(str::to_ascii_lowercase);
    if let Some(pin) = &pin
        && (pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(format!("release `{repo}`: `{pin}` is not a SHA-256 digest"));
    }

    let dir = user_root.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let artifact = dir.join(format!("{name}.wasm"));
    let wanted = crate::SourceRecord::Release {
        repo: repo.to_string(),
        version: tag.to_string(),
        sha256: pin.clone(),
        api: lattice_wit::api_generation().to_string(),
    };
    if policy == RefreshPolicy::UseCache
        && artifact.is_file()
        && crate::source_record::read(&dir) == wanted
    {
        return Ok(artifact);
    }

    let asset = release_asset_name(name, tag);
    let url = release_asset_url(repo, tag, &asset);
    let label = format!("{name} {tag}");
    let part = dir.join(format!(".{name}.wasm.part"));
    let digest_part = dir.join(format!(".{name}.wasm.sha256.part"));
    let outcome = (|| {
        fetcher
            .fetch(&format!("{url}.sha256"), &digest_part)
            .map_err(|e| format!("{label}: no published checksum ({asset}.sha256): {e}"))?;
        let published = std::fs::read_to_string(&digest_part)
            .map_err(|e| format!("{label}: reading the checksum: {e}"))?;
        let published = published
            .split_whitespace()
            .next()
            .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| format!("{label}: {asset}.sha256 holds no SHA-256 digest"))?;
        if let Some(pin) = &pin
            && *pin != published
        {
            return Err(format!(
                "{label}: the release publishes {published}, not the pinned {pin} \
                 — the tag was re-published"
            ));
        }
        fetcher
            .fetch(&url, &part)
            .map_err(|e| format!("{label}: {e}"))?;
        let bytes =
            std::fs::read(&part).map_err(|e| format!("{label}: reading the download: {e}"))?;
        let got = sha256_hex(&bytes);
        if got != published {
            return Err(format!(
                "{label}: {asset} hashes to {got}, but the release publishes {published}"
            ));
        }
        let manifest = lattice_plugin_host::PluginManifest::from_component_bytes(&bytes)
            .map_err(|e| format!("{label}: its manifest does not parse: {e}"))?
            .ok_or_else(|| {
                format!("{label}: {asset} carries no manifest (a release is one file)")
            })?;
        if manifest.id != name {
            return Err(format!(
                "{label}: the release holds plugin `{}`, not `{name}`",
                manifest.id
            ));
        }
        // The directory holds exactly one component (discovery's contract):
        // drop any other a previous install or a hand copy left.
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path != artifact && path.extension().is_some_and(|e| e == "wasm") {
                let _ = std::fs::remove_file(&path);
            }
        }
        std::fs::rename(&part, &artifact)
            .map_err(|e| format!("{label}: installing {}: {e}", artifact.display()))
    })();
    let _ = std::fs::remove_file(&digest_part);
    if outcome.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    outcome.map(|()| artifact)
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
    policy: RefreshPolicy,
) -> Result<PathBuf, String> {
    let dir = user_root.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let artifact = dir.join(format!("{name}.wasm"));
    // PRL.2: a boot (`UseCache`) uses what is on disk. It downloaded on every
    // resolve — every start — though `RefreshPolicy` said not to. Fetched
    // again only when it is missing, when the URL in the config changed, or
    // when asked (`Update`).
    let cached = artifact.is_file()
        && crate::source_record::read(&dir)
            == crate::SourceRecord::Prebuilt {
                url: url.to_string(),
            };
    if policy == RefreshPolicy::UseCache && cached {
        return Ok(artifact);
    }
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

    /// PRL.2: what a boot does, twice — the second start fetches nothing.
    #[test]
    fn a_prebuilt_already_on_disk_is_not_fetched_again_at_boot() {
        let root = tempdir("prebuilt-warm");
        let user = root.join("user");
        let url = "https://example.invalid/demo.wasm".to_string();
        let source = PluginSource::Prebuilt { url: url.clone() };
        let f = fetcher(b"\0asm-prebuilt");
        let boot = |f: &FakeFetcher, policy| {
            resolve(
                &FakeGit::default(),
                f,
                &source,
                "demo",
                &root.join("cache"),
                &user,
                policy,
            )
            .unwrap()
        };
        boot(&f, RefreshPolicy::UseCache);
        // The pipeline records where it came from beside the artifact.
        crate::source_record::write(&user.join("demo"), &source);
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);

        boot(&f, RefreshPolicy::UseCache);
        assert_eq!(
            f.calls.load(Ordering::SeqCst),
            1,
            "a warm boot makes no request"
        );

        boot(&f, RefreshPolicy::Update);
        assert_eq!(f.calls.load(Ordering::SeqCst), 2, "`update` asks again");
    }

    #[test]
    fn a_prebuilt_whose_url_changed_is_fetched_again() {
        let root = tempdir("prebuilt-moved");
        let user = root.join("user");
        let dir = user.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("demo.wasm"), b"old").unwrap();
        crate::source_record::write(
            &dir,
            &PluginSource::Prebuilt {
                url: "https://example.invalid/v1/demo.wasm".into(),
            },
        );
        let f = fetcher(b"new");
        resolve(
            &FakeGit::default(),
            &f,
            &PluginSource::Prebuilt {
                url: "https://example.invalid/v2/demo.wasm".into(),
            },
            "demo",
            &root.join("cache"),
            &user,
            RefreshPolicy::UseCache,
        )
        .unwrap();
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(dir.join("demo.wasm")).unwrap(), b"new");
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

    // --- Release (PRL.3) -------------------------------------------------

    /// Serves `url → body`; anything else is a 404. Counts requests.
    #[derive(Default)]
    struct MapFetcher {
        bodies: std::collections::HashMap<String, Vec<u8>>,
        calls: AtomicUsize,
    }

    impl MapFetcher {
        fn serve(mut self, url: &str, body: impl Into<Vec<u8>>) -> Self {
            self.bodies.insert(url.to_string(), body.into());
            self
        }
    }

    impl Fetcher for MapFetcher {
        fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let body = self
                .bodies
                .get(url)
                .ok_or_else(|| format!("GET {url}: HTTP 404"))?;
            std::fs::write(dest, body).map_err(|e| e.to_string())
        }
    }

    /// A component carrying `manifest` as its `lattice-manifest` section —
    /// all the installer reads of it.
    fn component(manifest: &str) -> Vec<u8> {
        let name = b"lattice-manifest";
        let mut bytes = b"\0asm\x0d\x00\x01\x00".to_vec();
        let body_len = 1 + name.len() + manifest.len();
        assert!(body_len < 128, "one-byte LEB in this fixture");
        bytes.push(0);
        bytes.push(body_len as u8);
        bytes.push(name.len() as u8);
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(manifest.as_bytes());
        bytes
    }

    const REPO: &str = "dhruvasagar/lattice-org-plugin";

    fn asset_url(tag: &str) -> String {
        release_asset_url(REPO, tag, &release_asset_name("org", tag))
    }

    /// A release of `org` at `tag` serving `wasm`, with a matching checksum.
    fn release(tag: &str, wasm: &[u8]) -> MapFetcher {
        MapFetcher::default().serve(&asset_url(tag), wasm).serve(
            &format!("{}.sha256", asset_url(tag)),
            format!("{}  org.wasm\n", sha256_hex(wasm)),
        )
    }

    fn install(f: &MapFetcher, user: &Path, pin: Option<&str>) -> Result<Resolved, String> {
        resolve(
            &FakeGit::default(),
            f,
            &PluginSource::Release {
                repo: REPO.into(),
                version: Some("v0.2.0".into()),
                sha256: pin.map(str::to_string),
            },
            "org",
            Path::new("/unused-cache"),
            user,
            RefreshPolicy::UseCache,
        )
    }

    /// `org/org.wasm` from an earlier install, to show it survives a failure.
    fn installed_before(user: &Path) -> PathBuf {
        let dir = user.join("org");
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data").join("store"), b"user state").unwrap();
        std::fs::write(dir.join("org.wasm"), b"previous").unwrap();
        dir
    }

    fn assert_previous_still_there(dir: &Path) {
        assert_eq!(std::fs::read(dir.join("org.wasm")).unwrap(), b"previous");
        assert_eq!(
            std::fs::read(dir.join("data").join("store")).unwrap(),
            b"user state"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "nothing half-written left: {leftovers:?}"
        );
    }

    #[test]
    fn the_asset_is_named_for_this_editors_api_generation() {
        assert_eq!(
            release_asset_name("org", "v0.2.0"),
            format!("org-0.2.0-api{}.wasm", lattice_wit::api_generation())
        );
    }

    #[test]
    fn a_release_installs_its_component_and_keeps_the_plugins_data() {
        let root = tempdir("release");
        let user = root.join("user");
        let dir = installed_before(&user);
        let wasm = component("id = \"org\"\n");
        let got = install(&release("v0.2.0", &wasm), &user, None).unwrap();
        assert_eq!(got, Resolved::Artifact(dir.join("org.wasm")));
        assert_eq!(std::fs::read(dir.join("org.wasm")).unwrap(), wasm);
        assert_eq!(
            std::fs::read(dir.join("data").join("store")).unwrap(),
            b"user state",
            "the plugin's data is not part of an upgrade"
        );
    }

    #[test]
    fn a_release_already_installed_is_not_fetched_at_boot() {
        let root = tempdir("release-warm");
        let user = root.join("user");
        let f = release("v0.2.0", &component("id = \"org\"\n"));
        install(&f, &user, None).unwrap();
        crate::source_record::write(
            &user.join("org"),
            &PluginSource::Release {
                repo: REPO.into(),
                version: Some("v0.2.0".into()),
                sha256: None,
            },
        );
        let before = f.calls.load(Ordering::SeqCst);
        install(&f, &user, None).unwrap();
        assert_eq!(
            f.calls.load(Ordering::SeqCst),
            before,
            "a warm boot makes no request"
        );
    }

    #[test]
    fn a_component_that_does_not_hash_to_the_checksum_is_refused() {
        let root = tempdir("release-mismatch");
        let user = root.join("user");
        let dir = installed_before(&user);
        let wasm = component("id = \"org\"\n");
        let f = MapFetcher::default()
            .serve(&asset_url("v0.2.0"), wasm)
            .serve(&format!("{}.sha256", asset_url("v0.2.0")), "0".repeat(64));
        let err = install(&f, &user, None).unwrap_err();
        assert!(err.contains("hashes to"), "{err}");
        assert_previous_still_there(&dir);
    }

    #[test]
    fn a_pinned_hash_the_release_no_longer_publishes_is_refused() {
        let root = tempdir("release-pin");
        let user = root.join("user");
        let dir = installed_before(&user);
        let f = release("v0.2.0", &component("id = \"org\"\n"));
        let err = install(&f, &user, Some(&"a".repeat(64))).unwrap_err();
        assert!(err.contains("re-published"), "{err}");
        assert_previous_still_there(&dir);
    }

    #[test]
    fn a_component_without_a_manifest_or_for_another_plugin_is_refused() {
        let root = tempdir("release-identity");
        let user = root.join("user");
        let dir = installed_before(&user);
        let err = install(&release("v0.2.0", b"\0asm\x0d\x00\x01\x00"), &user, None).unwrap_err();
        assert!(err.contains("carries no manifest"), "{err}");
        assert_previous_still_there(&dir);

        let err = install(
            &release("v0.2.0", &component("id = \"other\"\n")),
            &user,
            None,
        )
        .unwrap_err();
        assert!(err.contains("holds plugin `other`, not `org`"), "{err}");
        assert_previous_still_there(&dir);
    }

    #[test]
    fn a_release_with_no_build_for_this_api_is_refused_by_name() {
        let root = tempdir("release-missing");
        let user = root.join("user");
        let dir = installed_before(&user);
        let err = install(&MapFetcher::default(), &user, None).unwrap_err();
        assert!(err.starts_with("org v0.2.0:"), "named: {err}");
        assert!(err.contains(&release_asset_name("org", "v0.2.0")), "{err}");
        assert_previous_still_there(&dir);
    }

    #[test]
    fn a_release_spec_is_checked_before_it_reaches_a_url() {
        let root = tempdir("release-spec");
        let user = root.join("user");
        for (repo, tag) in [
            ("no-slash", "v1"),
            ("a/b/c", "v1"),
            ("owner/name", "../../evil"),
            ("owner/name", ""),
        ] {
            let f = MapFetcher::default();
            let err = resolve(
                &FakeGit::default(),
                &f,
                &PluginSource::Release {
                    repo: repo.into(),
                    version: Some(tag.into()),
                    sha256: None,
                },
                "org",
                Path::new("/unused"),
                &user,
                RefreshPolicy::UseCache,
            )
            .unwrap_err();
            assert!(!err.is_empty());
            assert_eq!(
                f.calls.load(Ordering::SeqCst),
                0,
                "{repo} {tag}: no request"
            );
        }
    }
}
