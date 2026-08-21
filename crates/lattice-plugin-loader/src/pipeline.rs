//! PM.7: the resolve → build → stage pipeline behind a `require`.
//!
//! Design: [`plugin-manager.md`](../../../docs/dev/architecture/plugin-manager.md)
//! §2, §5, §6. The host records `require` specs during a guest's
//! `register-plugins` export (`lattice_plugin_host::plugin_manager_host`) and
//! drains them afterwards; each drained spec comes here.
//!
//! The body is short because PM.5 and PM.6 are the parts with behaviour:
//! [`crate::resolve`] turns a source into either a tree or a finished
//! artifact, and [`crate::build_plugin`] turns a tree into a cached artifact.
//! A `Prebuilt` short-circuits the build entirely — that is the whole reason
//! `Resolved` is an enum.
//!
//! What this module adds is the **failure policy**. Every step can fail for
//! reasons outside the editor's control (no network, no toolchain, a bad
//! revision), and none of them may take the editor down:
//!
//! - a spec that fails to resolve or build becomes [`Install::Skipped`],
//!   logged and reportable, and the *next* spec is still attempted;
//! - a stale rebuild that fails still yields an artifact
//!   ([`crate::BuildOutcome::StaleKept`]) and is installed, carrying its
//!   error forward so `:plugins` can show that it is running old code.
//!
//! Blocking throughout — clone, download, compile. Callers run it on
//! `spawn_blocking` (paramount goal #1 / #4).

use std::path::{Path, PathBuf};

use crate::build::{BuildOutcome, ComponentBuilder, build_plugin};
use crate::resolve::{Fetcher, GitRunner, PluginSource, Resolved, resolve};

/// One plugin a guest declared. The loader-side mirror of the host's
/// `RequiredPlugin`.
///
/// Mirrored rather than shared because the dependency runs `loader → host`,
/// and a WIT-facing type that changed shape when a loader refactor touched it
/// would break a public API on an internal edit. The conversion is one `match`
/// at the boot call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredSpec {
    pub name: String,
    pub source: PluginSource,
    /// Carried opaquely to the mode-enable step; the loader never interprets
    /// it (`feedback_mode_owns_its_surface`).
    pub enable_mode: Option<String>,
    pub pinned: bool,
}

/// Convert a host-side `RequiredPlugin` into the loader's mirror.
///
/// The one `match` the mirroring costs. Worth it: without it, the WIT-facing
/// type and the loader's would be the same type, and a loader refactor could
/// reshape a published plugin API.
pub fn to_required_spec(
    p: lattice_plugin_host::plugin_manager_host::RequiredPlugin,
) -> RequiredSpec {
    use lattice_plugin_host::plugin_manager_host::RequiredSource as Host;
    RequiredSpec {
        name: p.name,
        source: match p.source {
            Host::Local(path) => PluginSource::Local(PathBuf::from(path)),
            Host::Git { url, rev } => PluginSource::Git { url, rev },
            Host::Prebuilt { url } => PluginSource::Prebuilt { url },
        },
        enable_mode: p.enable_mode,
        pinned: p.pinned,
    }
}

/// What happened to one required plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// There is an artifact to load.
    Ready {
        name: String,
        artifact: PathBuf,
        enable_mode: Option<String>,
        /// Set when the artifact is **stale** — a rebuild failed and the
        /// previous build is what will load. Not a failure; a caveat the
        /// user should be able to see.
        stale: Option<String>,
    },
    /// Nothing loadable. The plugin is absent this session.
    Skipped { name: String, error: String },
}

impl Install {
    pub fn name(&self) -> &str {
        match self {
            Install::Ready { name, .. } | Install::Skipped { name, .. } => name,
        }
    }
}

/// Resolve, build and stage one required plugin.
///
/// Blocking. Run on `spawn_blocking`.
pub fn install_required(
    git: &dyn GitRunner,
    fetcher: &dyn Fetcher,
    builder: &dyn ComponentBuilder,
    spec: &RequiredSpec,
    cache_root: &Path,
    user_root: &Path,
) -> Install {
    let skip = |error: String| Install::Skipped {
        name: spec.name.clone(),
        error,
    };

    let resolved = match resolve(
        git,
        fetcher,
        &spec.source,
        &spec.name,
        cache_root,
        user_root,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(plugin = %spec.name, error = %e, "require: resolve failed; skipping");
            return skip(e);
        }
    };

    let source_dir = match resolved {
        // A prebuilt is already the artifact — no toolchain is consulted at
        // all, which is the point of the kind.
        Resolved::Artifact(artifact) => {
            // PM.8a: remember where it came from, so the view can say so and
            // a later re-download knows the URL.
            if let Some(dir) = artifact.parent() {
                crate::source_record::write(dir, &spec.source);
            }
            return Install::Ready {
                name: spec.name.clone(),
                artifact,
                enable_mode: spec.enable_mode.clone(),
                stale: None,
            };
        }
        Resolved::Source(dir) => dir,
    };

    let outcome = build_plugin(builder, &source_dir, &spec.name, user_root, spec.pinned);
    // PM.8a: the marker goes beside the artifact whenever there IS one —
    // including the stale-kept case, where knowing the source is exactly what
    // lets the user retry the build that failed.
    if let Some(artifact) = outcome.artifact()
        && let Some(dir) = artifact.parent()
    {
        crate::source_record::write(dir, &spec.source);
    }
    match outcome {
        BuildOutcome::Cached { artifact } | BuildOutcome::Fresh { artifact } => Install::Ready {
            name: spec.name.clone(),
            artifact,
            enable_mode: spec.enable_mode.clone(),
            stale: None,
        },
        BuildOutcome::StaleKept { artifact, error } => Install::Ready {
            name: spec.name.clone(),
            artifact,
            enable_mode: spec.enable_mode.clone(),
            stale: Some(error),
        },
        BuildOutcome::Failed { error } => {
            tracing::warn!(plugin = %spec.name, error = %error, "require: build failed; skipping");
            skip(error)
        }
    }
}

/// Run every required spec, in declaration order.
///
/// One spec's failure never stops the next: a user with five plugins and one
/// broken source should lose that one, not the four that work.
pub fn install_all(
    git: &dyn GitRunner,
    fetcher: &dyn Fetcher,
    builder: &dyn ComponentBuilder,
    specs: &[RequiredSpec],
    cache_root: &Path,
    user_root: &Path,
) -> Vec<Install> {
    specs
        .iter()
        .map(|spec| install_required(git, fetcher, builder, spec, cache_root, user_root))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("lattice-pm7-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct NoGit;
    impl GitRunner for NoGit {
        fn run(&self, _cwd: &Path, _args: &[&str]) -> Result<String, String> {
            Err("git unavailable".into())
        }
    }

    struct StubFetch;
    impl Fetcher for StubFetch {
        fn fetch(&self, _url: &str, dest: &Path) -> Result<(), String> {
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::write(dest, b"\0asm-prebuilt").unwrap();
            Ok(())
        }
    }

    struct StubBuild {
        fail: bool,
        calls: AtomicUsize,
    }
    impl StubBuild {
        fn ok() -> Self {
            Self {
                fail: false,
                calls: AtomicUsize::new(0),
            }
        }
        fn failing() -> Self {
            Self {
                fail: true,
                calls: AtomicUsize::new(0),
            }
        }
    }
    impl ComponentBuilder for StubBuild {
        fn build(&self, source_dir: &Path) -> Result<PathBuf, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err("compile error".into());
            }
            let out = source_dir
                .parent()
                .unwrap_or(source_dir)
                .join("stub-out.wasm");
            std::fs::write(&out, b"\0asm").unwrap();
            Ok(out)
        }
    }

    fn source_tree(root: &Path, name: &str) -> PathBuf {
        let src = root.join(format!("{name}-src"));
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::write(src.join("plugin.toml"), format!("id = \"{name}\"\n")).unwrap();
        std::fs::write(src.join("src").join("lib.rs"), "// v1").unwrap();
        src
    }

    fn local(root: &Path, name: &str) -> RequiredSpec {
        RequiredSpec {
            name: name.to_string(),
            source: PluginSource::Local(source_tree(root, name)),
            enable_mode: Some(format!("{name}-mode")),
            pinned: false,
        }
    }

    #[test]
    fn a_local_spec_resolves_builds_and_becomes_ready() {
        let root = tempdir("ready");
        let out = install_required(
            &NoGit,
            &StubFetch,
            &StubBuild::ok(),
            &local(&root, "demo"),
            &root.join("cache"),
            &root.join("user"),
        );
        match out {
            Install::Ready {
                name,
                artifact,
                enable_mode,
                stale,
            } => {
                assert_eq!(name, "demo");
                assert!(artifact.is_file());
                assert_eq!(enable_mode.as_deref(), Some("demo-mode"));
                assert_eq!(stale, None);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn a_prebuilt_spec_never_reaches_the_builder() {
        // The kind exists so a user with no toolchain can install a plugin;
        // consulting the builder would defeat it.
        let root = tempdir("prebuilt");
        let builder = StubBuild::ok();
        let out = install_required(
            &NoGit,
            &StubFetch,
            &builder,
            &RequiredSpec {
                name: "pre".into(),
                source: PluginSource::Prebuilt {
                    url: "https://example.invalid/p.wasm".into(),
                },
                enable_mode: None,
                pinned: false,
            },
            &root.join("cache"),
            &root.join("user"),
        );
        assert!(matches!(out, Install::Ready { .. }));
        assert_eq!(
            builder.calls.load(Ordering::SeqCst),
            0,
            "a prebuilt must skip the build entirely"
        );
    }

    #[test]
    fn a_resolve_failure_is_a_skip_not_a_panic() {
        let root = tempdir("resolve-fail");
        let out = install_required(
            &NoGit,
            &StubFetch,
            &StubBuild::ok(),
            &RequiredSpec {
                name: "gone".into(),
                source: PluginSource::Local(root.join("does-not-exist")),
                enable_mode: None,
                pinned: false,
            },
            &root.join("cache"),
            &root.join("user"),
        );
        match out {
            Install::Skipped { name, error } => {
                assert_eq!(name, "gone");
                assert!(error.contains("not a directory"), "got: {error}");
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn a_build_failure_with_no_prior_artifact_is_a_skip() {
        let root = tempdir("build-fail");
        let out = install_required(
            &NoGit,
            &StubFetch,
            &StubBuild::failing(),
            &local(&root, "broken"),
            &root.join("cache"),
            &root.join("user"),
        );
        assert!(matches!(out, Install::Skipped { .. }));
    }

    #[test]
    fn a_stale_rebuild_failure_still_installs_and_reports_the_caveat() {
        // The plugin runs old code — which is right — but the user must be
        // able to find out that it is.
        let root = tempdir("stale");
        let user = root.join("user");
        let spec = local(&root, "demo");
        install_required(
            &NoGit,
            &StubFetch,
            &StubBuild::ok(),
            &spec,
            &root.join("cache"),
            &user,
        );

        // Dirty the source so the next attempt is a rebuild, then fail it.
        let PluginSource::Local(ref dir) = spec.source else {
            unreachable!()
        };
        let f = dir.join("src").join("lib.rs");
        std::fs::write(&f, "// broken").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
        let file = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(later))
            .unwrap();

        let out = install_required(
            &NoGit,
            &StubFetch,
            &StubBuild::failing(),
            &spec,
            &root.join("cache"),
            &user,
        );
        match out {
            Install::Ready {
                stale, artifact, ..
            } => {
                assert!(artifact.is_file(), "the previous build still loads");
                assert!(
                    stale.unwrap().contains("compile error"),
                    "the caveat must be reportable, not silent"
                );
            }
            other => panic!("expected a stale Ready, got {other:?}"),
        }
    }

    #[test]
    fn one_broken_spec_does_not_stop_the_others() {
        // A user with five plugins and one broken source loses that one.
        let root = tempdir("install-all");
        let specs = vec![
            local(&root, "first"),
            RequiredSpec {
                name: "broken".into(),
                source: PluginSource::Local(root.join("nope")),
                enable_mode: None,
                pinned: false,
            },
            local(&root, "third"),
        ];
        let out = install_all(
            &NoGit,
            &StubFetch,
            &StubBuild::ok(),
            &specs,
            &root.join("cache"),
            &root.join("user"),
        );

        assert_eq!(out.len(), 3, "every spec is attempted");
        assert!(matches!(out[0], Install::Ready { .. }));
        assert!(matches!(out[1], Install::Skipped { .. }));
        assert!(
            matches!(out[2], Install::Ready { .. }),
            "a failure must not abort the specs after it"
        );
    }

    #[test]
    fn the_host_to_loader_conversion_preserves_every_field() {
        // The one cost of mirroring the types across the boundary. If it
        // drifts, a user's `pinned` or `enable-mode` silently stops working
        // with nothing failing to compile — so it gets a test rather than a
        // reviewer's attention.
        use lattice_plugin_host::plugin_manager_host::{RequiredPlugin, RequiredSource};
        let got = to_required_spec(RequiredPlugin {
            name: "demo".into(),
            source: RequiredSource::Git {
                url: "https://example.invalid/d.git".into(),
                rev: Some("abc".into()),
            },
            enable_mode: Some("demo-mode".into()),
            pinned: true,
        });
        assert_eq!(
            got,
            RequiredSpec {
                name: "demo".into(),
                source: PluginSource::Git {
                    url: "https://example.invalid/d.git".into(),
                    rev: Some("abc".into()),
                },
                enable_mode: Some("demo-mode".into()),
                pinned: true,
            }
        );
    }

    #[test]
    fn every_source_kind_survives_the_conversion() {
        use lattice_plugin_host::plugin_manager_host::{RequiredPlugin, RequiredSource};
        let spec = |source| RequiredPlugin {
            name: "d".into(),
            source,
            enable_mode: None,
            pinned: false,
        };
        assert_eq!(
            to_required_spec(spec(RequiredSource::Local("/tmp/x".into()))).source,
            PluginSource::Local(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            to_required_spec(spec(RequiredSource::Prebuilt {
                url: "https://example.invalid/x.wasm".into()
            }))
            .source,
            PluginSource::Prebuilt {
                url: "https://example.invalid/x.wasm".into()
            }
        );
    }

    #[test]
    fn declaration_order_is_preserved() {
        // `require` order is the user's stated order; a plugin that
        // configures another should be able to rely on it.
        let root = tempdir("order");
        let specs = vec![
            local(&root, "aaa"),
            local(&root, "bbb"),
            local(&root, "ccc"),
        ];
        let out = install_all(
            &NoGit,
            &StubFetch,
            &StubBuild::ok(),
            &specs,
            &root.join("cache"),
            &root.join("user"),
        );
        let names: Vec<&str> = out.iter().map(|i| i.name()).collect();
        assert_eq!(names, vec!["aaa", "bbb", "ccc"]);
    }
}
