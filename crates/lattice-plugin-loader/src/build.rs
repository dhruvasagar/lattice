//! PM.5: the build service — a plugin source directory becomes a cached
//! `.wasm` component.
//!
//! Design: [`plugin-manager.md`](../../../docs/dev/architecture/plugin-manager.md)
//! §5. One primitive with two callers (§6): every user plugin, and the
//! user's own `init.rs`.
//!
//! ## The requirement that shapes everything here
//!
//! **A warm boot with an unchanged source must not rebuild.** A cold
//! component build is seconds to minutes; paying that on every start
//! would make the editor unusable, so the service is a *cache* with a
//! build fallback, not a build step with a cache in front. That is what
//! the `.build-stamp` is for: it records what the artifact was built
//! from, and a matching stamp short-circuits to a pure load.
//!
//! ## Failure is a skip, never a stall
//!
//! Three failure modes, three different answers, and the middle one is
//! the one worth naming:
//!
//! - **No artifact, build fails** → [`BuildOutcome::Failed`]. The plugin
//!   does not load. Logged, surfaced in `:plugins`; boot continues.
//! - **An artifact exists, a *stale* rebuild fails** →
//!   [`BuildOutcome::StaleKept`]. The previous artifact keeps loading.
//!   A user who pushes a broken revision to a plugin they depend on
//!   should lose the *new* code, not the working editor they had five
//!   minutes ago.
//! - **Stamp matches** → [`BuildOutcome::Cached`]. No toolchain is
//!   invoked at all, so a machine with no Rust installed still boots
//!   every already-built plugin.
//!
//! Nothing in this module panics and nothing blocks: the caller runs
//! [`build_plugin`] on `spawn_blocking` (paramount goal #1 / #4 — never
//! the boot or actor thread).

use std::path::{Path, PathBuf};

/// The file recording what the cached artifact was built from.
const STAMP_FILE: &str = ".build-stamp";

/// Produces a `.wasm` component from a source directory.
///
/// A trait rather than a free function so the staleness, caching and
/// failure logic below can be tested without a `wasm32-wasip2`
/// toolchain — those are the parts with the interesting behaviour, and
/// they should not be untestable on a machine that cannot compile a
/// component.
pub trait ComponentBuilder: Send + Sync {
    /// Build `source_dir`; return the path of the produced component.
    fn build(&self, source_dir: &Path) -> Result<PathBuf, String>;
}

/// The real builder: `cargo build --release --target wasm32-wasip2`.
///
/// Runs in a **clean environment**. Inherited workspace `RUSTFLAGS` /
/// target / rustc-wrapper settings break a wasm build, which is the
/// same lesson `lattice-plugin-host`'s `build.rs` and `cargo xtask
/// build-core-plugins` both already encode — this is the third site, so
/// the env-scrubbing list is deliberately identical to theirs.
#[derive(Debug, Default, Clone, Copy)]
pub struct CargoComponentBuilder;

impl ComponentBuilder for CargoComponentBuilder {
    fn build(&self, source_dir: &Path) -> Result<PathBuf, String> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let target_dir = source_dir.join("target");
        let output = std::process::Command::new(&cargo)
            .current_dir(source_dir)
            .args(["build", "--release", "--target", "wasm32-wasip2"])
            // Pin the target dir so a leaked `CARGO_TARGET_DIR` cannot
            // redirect the output away from where we stage from.
            .arg("--target-dir")
            .arg(&target_dir)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_BUILD_RUSTFLAGS")
            .env_remove("CARGO_BUILD_TARGET")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
            .map_err(|e| format!("failed to run cargo: {e}"))?;
        if !output.status.success() {
            // The compiler's own diagnostics are the useful part; keep
            // the tail so `:plugins` can show why without holding a
            // whole build log in memory.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(format!(
                "cargo build failed ({}). Is the target installed? \
                 `rustup target add wasm32-wasip2`\n{tail}",
                output.status
            ));
        }
        let release = target_dir.join("wasm32-wasip2").join("release");
        find_component(&release)
            .ok_or_else(|| format!("build produced no .wasm in {}", release.display()))
    }
}

/// The single `.wasm` in `dir`, if there is exactly one.
///
/// "Exactly one" rather than "the first": a directory with two
/// components is ambiguous, and silently picking one would stage an
/// artifact the user did not mean to ship.
fn find_component(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            if found.is_some() {
                tracing::warn!(dir = %dir.display(), "more than one .wasm; refusing to guess");
                return None;
            }
            found = Some(path);
        }
    }
    found
}

/// What [`build_plugin`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildOutcome {
    /// The stamp matched; no toolchain was invoked.
    Cached { artifact: PathBuf },
    /// Built (or rebuilt) now.
    Fresh { artifact: PathBuf },
    /// A stale rebuild failed, but a previous artifact is still there
    /// and still loads. The plugin runs old code; the error surfaces.
    StaleKept { artifact: PathBuf, error: String },
    /// No usable artifact. The plugin does not load.
    Failed { error: String },
}

impl BuildOutcome {
    /// The artifact to load, if any.
    pub fn artifact(&self) -> Option<&Path> {
        match self {
            BuildOutcome::Cached { artifact }
            | BuildOutcome::Fresh { artifact }
            | BuildOutcome::StaleKept { artifact, .. } => Some(artifact),
            BuildOutcome::Failed { .. } => None,
        }
    }

    /// The error, if the build did not fully succeed. `StaleKept`
    /// carries one *and* an artifact — a partial success is not a
    /// silent one.
    pub fn error(&self) -> Option<&str> {
        match self {
            BuildOutcome::StaleKept { error, .. } | BuildOutcome::Failed { error } => Some(error),
            _ => None,
        }
    }
}

/// A fingerprint of `source_dir`'s contents, for staleness comparison.
///
/// Max mtime plus file count, walking the source tree and skipping
/// `target/`, `.git/` and hidden entries. The file count is what makes
/// a *deletion* register: mtimes only ever move forward, so a removed
/// file would otherwise leave the stamp unchanged and the artifact
/// wrongly considered current.
///
/// Not a content hash. Hashing every byte of a cargo project on every
/// boot is real I/O for a check that runs before we know whether we
/// even need to build, and mtime+count is what cargo itself trusts for
/// the same job.
pub fn source_stamp(source_dir: &Path) -> String {
    let mut newest: u128 = 0;
    let mut files: u64 = 0;
    let mut stack = vec![source_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // An unreadable subdirectory is not worth failing over; it
            // just does not contribute to the fingerprint.
            Err(e) => {
                tracing::debug!(dir = %dir.display(), error = %e, "stamp: skipping unreadable dir");
                continue;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Build output and VCS metadata churn constantly and say
            // nothing about the source; including them would make every
            // boot look stale.
            if name.starts_with('.') || name == "target" {
                continue;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(path),
                Ok(_) => {
                    files += 1;
                    if let Ok(meta) = entry.metadata()
                        && let Ok(modified) = meta.modified()
                        && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
                    {
                        newest = newest.max(since.as_nanos());
                    }
                }
                Err(_) => {}
            }
        }
    }
    format!("mtime:{newest}:files:{files}")
}

/// Where a plugin's cached artifact lives.
pub fn artifact_path(user_root: &Path, name: &str) -> PathBuf {
    user_root.join(name).join(format!("{name}.wasm"))
}

fn stamp_path(user_root: &Path, name: &str) -> PathBuf {
    user_root.join(name).join(STAMP_FILE)
}

/// Build `name` from `source_dir` into `user_root`, unless the cached
/// artifact is already current.
///
/// `pinned` skips the staleness check entirely: build only if the
/// artifact is **absent**. That is the escape hatch for a user who
/// wants a known-good build to stay put regardless of what the source
/// tree does.
///
/// Blocking. Run it on `spawn_blocking` — never the boot or actor
/// thread (§5).
pub fn build_plugin(
    builder: &dyn ComponentBuilder,
    source_dir: &Path,
    name: &str,
    user_root: &Path,
    pinned: bool,
) -> BuildOutcome {
    let artifact = artifact_path(user_root, name);
    let has_artifact = artifact.is_file();

    // Pinned + present: never look at the source at all.
    if pinned && has_artifact {
        return BuildOutcome::Cached { artifact };
    }

    let stamp = source_stamp(source_dir);
    if has_artifact
        && !pinned
        && std::fs::read_to_string(stamp_path(user_root, name))
            .is_ok_and(|cached| cached.trim() == stamp)
    {
        tracing::debug!(
            plugin = name,
            "build: stamp matches; loading cached artifact"
        );
        return BuildOutcome::Cached { artifact };
    }

    tracing::info!(plugin = name, "building plugin from source");
    match builder.build(source_dir) {
        Ok(produced) => match stage(&produced, source_dir, &artifact, user_root, name, &stamp) {
            Ok(()) => BuildOutcome::Fresh { artifact },
            Err(error) => fail(has_artifact, artifact, error, name),
        },
        Err(error) => fail(has_artifact, artifact, error, name),
    }
}

/// Whether two paths name the same file on disk.
///
/// Compared after canonicalisation rather than as strings: the same directory
/// reached as itself and as `<parent>/<name>` is one directory spelled two
/// ways, which is precisely the init dir's case. A path that cannot be
/// canonicalised (the destination does not exist yet — the common staging
/// case) is not the source, so the copy proceeds.
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Turn a build/stage error into the right outcome: keep a previous
/// artifact when one exists, otherwise report a hard failure.
fn fail(has_artifact: bool, artifact: PathBuf, error: String, name: &str) -> BuildOutcome {
    if has_artifact {
        tracing::warn!(
            plugin = name,
            %error,
            "rebuild failed; keeping the previously built artifact"
        );
        BuildOutcome::StaleKept { artifact, error }
    } else {
        tracing::warn!(plugin = name, %error, "build failed; plugin will not load");
        BuildOutcome::Failed { error }
    }
}

/// Copy the built component and the source's manifest into the user
/// root, then write the stamp.
///
/// The stamp is written **last, and only on full success**. A stamp
/// written before the copy would mark a half-staged plugin as current
/// and suppress the rebuild that would fix it — the artifact and the
/// claim about it have to land in that order.
fn stage(
    produced: &Path,
    source_dir: &Path,
    artifact: &Path,
    user_root: &Path,
    name: &str,
    stamp: &str,
) -> Result<(), String> {
    let dir = user_root.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    if !is_same_file(produced, artifact) {
        std::fs::copy(produced, artifact)
            .map_err(|e| format!("stage {} → {}: {e}", produced.display(), artifact.display()))?;
    }
    // The manifest travels with the artifact: discovery reads both out
    // of the user root, and a staged `.wasm` with no `plugin.toml`
    // beside it is invisible to it.
    let manifest_src = source_dir.join("plugin.toml");
    if manifest_src.is_file() {
        let manifest_dst = dir.join("plugin.toml");
        // **A source that IS its own staging dir must not be copied over.**
        // `init.rs` is exactly that: `build_init_if_needed` passes
        // `user_root = <config>/lattice` and `name = "init"`, so
        // `user_root.join(name)` is the init dir the source lives in, and
        // `manifest_src == manifest_dst`.
        //
        // `fs::copy(p, p)` does not no-op — it opens the destination with
        // `O_TRUNC` before reading the source, then reports `Ok(0)`. The
        // manifest is left EMPTY and the build reports success. Next boot the
        // empty manifest has no `id`, so init.rs fails to load, and because
        // that failure is a `debug!` the user sees an editor with no config
        // and no message. Whatever init.rs `require`d never installs either.
        if !is_same_file(&manifest_src, &manifest_dst) {
            std::fs::copy(&manifest_src, &manifest_dst)
                .map_err(|e| format!("stage manifest → {}: {e}", manifest_dst.display()))?;
        }
    } else {
        return Err(format!(
            "source has no plugin.toml at {}",
            manifest_src.display()
        ));
    }
    std::fs::write(stamp_path(user_root, name), stamp)
        .map_err(|e| format!("write build stamp: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A builder that records how many times it ran and writes a stub
    /// component. Standing in for the toolchain is the whole point —
    /// the behaviour under test is the caching, not cargo.
    struct FakeBuilder {
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeBuilder {
        fn ok() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: true,
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ComponentBuilder for FakeBuilder {
        fn build(&self, source_dir: &Path) -> Result<PathBuf, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err("compile error".into());
            }
            // Write OUTSIDE the source tree. Real cargo emits into
            // `target/`, which the stamp excludes; a fake that dirtied
            // the source would make every build look stale and quietly
            // invert the test it appears in.
            let out = source_dir
                .parent()
                .unwrap_or(source_dir)
                .join("fake-build-out.wasm");
            std::fs::write(&out, b"\0asm-stub").unwrap();
            Ok(out)
        }
    }

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Unique temp dir. The counter is load-bearing under parallel
    /// `cargo test`: a timestamp alone collides.
    fn tempdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("lattice-pm5-{tag}-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// **The init directory's shape: the source IS the staging destination.**
    ///
    /// `build_init_if_needed` passes `user_root = <config>/lattice` and
    /// `name = "init"`, so `user_root.join(name)` is the very directory the
    /// source lives in. `fs::copy(p, p)` truncates — it opens the destination
    /// with `O_TRUNC` before reading the source and then reports `Ok(0)` — so
    /// staging emptied the user's own `plugin.toml` and called it a success.
    ///
    /// The cost was invisible and total: next boot the empty manifest has no
    /// `id`, init.rs fails to load behind a `debug!`, and everything it
    /// `require`d never installs. Found on a real machine whose
    /// `~/.config/lattice/init/plugin.toml` was 0 bytes.
    #[test]
    fn staging_into_the_source_directory_does_not_empty_the_manifest() {
        let root = tempdir("selfstage");
        // The layout `build_init_if_needed` produces.
        let user_root = root.join("lattice");
        let init_dir = user_root.join("init");
        std::fs::create_dir_all(init_dir.join("src")).unwrap();
        let manifest = "id = \"init\"\nprovides = [\"modes\"]\n";
        std::fs::write(init_dir.join("plugin.toml"), manifest).unwrap();
        std::fs::write(init_dir.join("src").join("lib.rs"), "// init").unwrap();

        let builder = FakeBuilder::ok();
        let outcome = build_plugin(&builder, &init_dir, "init", &user_root, false);

        assert!(
            matches!(outcome, BuildOutcome::Fresh { .. }),
            "the build still succeeds: {outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(init_dir.join("plugin.toml")).unwrap(),
            manifest,
            "the manifest survives staging into its own directory"
        );
        assert!(
            artifact_path(&user_root, "init").is_file(),
            "and the artifact still lands"
        );
    }

    /// A minimal plugin source: a manifest and one source file.
    fn source(dir: &Path) -> PathBuf {
        let src = dir.join("src-tree");
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::write(src.join("plugin.toml"), "id = \"demo\"\n").unwrap();
        std::fs::write(src.join("src").join("lib.rs"), "// v1").unwrap();
        src
    }

    #[test]
    fn a_first_build_produces_and_stages_the_artifact() {
        let root = tempdir("first");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();

        let out = build_plugin(&b, &src, "demo", &user, false);
        let artifact = user.join("demo").join("demo.wasm");
        assert_eq!(
            out,
            BuildOutcome::Fresh {
                artifact: artifact.clone()
            }
        );
        assert!(artifact.is_file(), "the component is staged");
        assert!(
            user.join("demo").join("plugin.toml").is_file(),
            "the manifest travels with it, or discovery cannot see the plugin"
        );
        assert_eq!(b.calls(), 1);
    }

    #[test]
    fn an_unchanged_source_does_not_rebuild() {
        // The requirement the whole module exists for: a warm boot is a
        // pure load.
        let root = tempdir("warm");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();

        build_plugin(&b, &src, "demo", &user, false);
        let second = build_plugin(&b, &src, "demo", &user, false);

        assert!(matches!(second, BuildOutcome::Cached { .. }));
        assert_eq!(b.calls(), 1, "the toolchain must not be invoked again");
    }

    #[test]
    fn an_edited_source_rebuilds() {
        let root = tempdir("edit");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();

        build_plugin(&b, &src, "demo", &user, false);
        // Move the mtime forward decisively — a same-nanosecond write
        // would make this test about clock resolution instead of about
        // staleness.
        let f = src.join("src").join("lib.rs");
        std::fs::write(&f, "// v2").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
        let _ = filetime_set(&f, later);

        let second = build_plugin(&b, &src, "demo", &user, false);
        assert!(matches!(second, BuildOutcome::Fresh { .. }));
        assert_eq!(b.calls(), 2);
    }

    #[test]
    fn a_deleted_source_file_registers_as_stale() {
        // mtimes only move forward, so a deletion is invisible to a
        // max-mtime stamp on its own. The file count is what catches it.
        let root = tempdir("delete");
        let src = source(&root);
        std::fs::write(src.join("src").join("extra.rs"), "// x").unwrap();
        let user = root.join("user");
        let b = FakeBuilder::ok();

        build_plugin(&b, &src, "demo", &user, false);
        std::fs::remove_file(src.join("src").join("extra.rs")).unwrap();
        let second = build_plugin(&b, &src, "demo", &user, false);

        assert!(matches!(second, BuildOutcome::Fresh { .. }));
        assert_eq!(b.calls(), 2);
    }

    #[test]
    fn target_and_dotdirs_do_not_make_a_source_look_stale() {
        // Build output churns on every build; if it counted, nothing
        // would ever be cached.
        let root = tempdir("ignore");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();
        build_plugin(&b, &src, "demo", &user, false);

        std::fs::create_dir_all(src.join("target").join("deep")).unwrap();
        std::fs::write(src.join("target").join("deep").join("x.rlib"), "junk").unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git").join("HEAD"), "ref").unwrap();

        let second = build_plugin(&b, &src, "demo", &user, false);
        assert!(matches!(second, BuildOutcome::Cached { .. }));
        assert_eq!(b.calls(), 1);
    }

    #[test]
    fn a_failed_first_build_yields_no_artifact() {
        let root = tempdir("fail");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::failing();

        let out = build_plugin(&b, &src, "demo", &user, false);
        assert!(matches!(out, BuildOutcome::Failed { .. }));
        assert_eq!(out.artifact(), None);
        assert!(out.error().unwrap().contains("compile error"));
    }

    #[test]
    fn a_failed_rebuild_keeps_the_previous_artifact_loading() {
        // The behaviour worth protecting: pushing a broken revision to a
        // plugin costs you the new code, not the editor you had.
        let root = tempdir("stale-keep");
        let src = source(&root);
        let user = root.join("user");
        build_plugin(&FakeBuilder::ok(), &src, "demo", &user, false);

        let f = src.join("src").join("lib.rs");
        std::fs::write(&f, "// broken").unwrap();
        let _ = filetime_set(
            &f,
            std::time::SystemTime::now() + std::time::Duration::from_secs(120),
        );

        let out = build_plugin(&FakeBuilder::failing(), &src, "demo", &user, false);
        match out {
            BuildOutcome::StaleKept { artifact, error } => {
                assert!(artifact.is_file(), "the old artifact is still there");
                assert!(
                    error.contains("compile error"),
                    "and the failure is not silent"
                );
            }
            other => panic!("expected StaleKept, got {other:?}"),
        }
    }

    #[test]
    fn a_pinned_plugin_ignores_source_changes() {
        let root = tempdir("pinned");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();
        build_plugin(&b, &src, "demo", &user, false);

        let f = src.join("src").join("lib.rs");
        std::fs::write(&f, "// v2").unwrap();
        let _ = filetime_set(
            &f,
            std::time::SystemTime::now() + std::time::Duration::from_secs(120),
        );

        let out = build_plugin(&b, &src, "demo", &user, true);
        assert!(matches!(out, BuildOutcome::Cached { .. }));
        assert_eq!(b.calls(), 1, "pinned means do not look at the source");
    }

    #[test]
    fn a_pinned_plugin_with_no_artifact_still_builds() {
        // Pinned means "don't rebuild", not "never build" — otherwise a
        // pinned plugin could never be installed in the first place.
        let root = tempdir("pinned-cold");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();

        let out = build_plugin(&b, &src, "demo", &user, true);
        assert!(matches!(out, BuildOutcome::Fresh { .. }));
        assert_eq!(b.calls(), 1);
    }

    #[test]
    fn a_source_without_a_manifest_fails_rather_than_staging_half_a_plugin() {
        let root = tempdir("nomanifest");
        let src = root.join("bare");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "// x").unwrap();
        let user = root.join("user");

        let out = build_plugin(&FakeBuilder::ok(), &src, "demo", &user, false);
        assert!(matches!(out, BuildOutcome::Failed { .. }));
        assert!(out.error().unwrap().contains("plugin.toml"));
        assert!(
            !stamp_path(&user, "demo").exists(),
            "no stamp may be written for a build that did not fully stage"
        );
    }

    #[test]
    fn a_half_staged_plugin_rebuilds_rather_than_reporting_cached() {
        // Guards the stamp-written-last ordering: if a stamp could
        // outlive its artifact, the rebuild that would fix the install
        // would be suppressed forever.
        let root = tempdir("halfstage");
        let src = source(&root);
        let user = root.join("user");
        let b = FakeBuilder::ok();
        build_plugin(&b, &src, "demo", &user, false);

        std::fs::remove_file(artifact_path(&user, "demo")).unwrap();
        let out = build_plugin(&b, &src, "demo", &user, false);

        assert!(matches!(out, BuildOutcome::Fresh { .. }));
        assert_eq!(b.calls(), 2);
    }

    #[test]
    fn stamps_differ_between_different_sources() {
        let root = tempdir("stamps");
        let a = source(&root);
        let b = root.join("other");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("plugin.toml"), "id = \"other\"\n").unwrap();
        assert_ne!(source_stamp(&a), source_stamp(&b));
    }

    #[test]
    fn stamping_a_missing_dir_is_not_a_panic() {
        let stamp = source_stamp(Path::new("/definitely/not/here/lattice-pm5"));
        assert_eq!(stamp, "mtime:0:files:0");
    }

    /// Set a file's mtime.
    ///
    /// `std::fs::File::set_times` rather than shelling out to `touch`:
    /// the `-d @epoch` form is GNU-only and macOS rejects it, which made
    /// the first cut of these tests depend on a silent fallback that
    /// merely happened to move the clock far enough.
    fn filetime_set(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
        let f = std::fs::OpenOptions::new().write(true).open(path)?;
        f.set_times(std::fs::FileTimes::new().set_modified(when))
    }
}
