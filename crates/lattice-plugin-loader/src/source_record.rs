//! PM.8a: the `.source` marker — where an installed plugin came from,
//! remembered on disk beside its artifact.
//!
//! Design: [`plugin-manager.md`](../../../docs/dev/architecture/plugin-manager.md)
//! §4, §8.
//!
//! ## Why this is persisted rather than derived
//!
//! The obvious cheaper thing is to remember a plugin's source in memory for
//! the boot that installed it. That fails on the two cases that matter:
//!
//! - **The next boot.** A plugin cached in the user root loads from the
//!   on-disk scan. Nothing in that path has ever seen a `require`, so a
//!   derived source column would read `local` for a plugin that came from git
//!   — confidently wrong, which is worse than blank.
//! - **The rebuild chord (PM.8b).** You cannot re-clone a git plugin without
//!   its URL and rev. A chord that only worked for plugins required earlier in
//!   *this* session would be a chord that mostly does not work.
//!
//! So the source travels with the artifact, next to the `.build-stamp` that
//! already records what the artifact was built *from*. Together they answer
//! the two questions the view asks: where did this come from, and is it
//! current.
//!
//! ## Format
//!
//! A tiny hand-rolled `key = value` file rather than serde. The record is four
//! optional scalars and this crate does not otherwise depend on a TOML
//! deserializer; a malformed or absent file degrades to "unknown source",
//! never an error — a plugin whose marker got corrupted must still load.

use std::path::{Path, PathBuf};

use crate::resolve::PluginSource;

/// The file, beside `plugin.toml` and `.build-stamp`.
const SOURCE_FILE: &str = ".source";

/// Where a plugin on disk came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRecord {
    /// Ships with lattice, discovered from the runtime root (§7). Never built
    /// by the editor.
    Bundled,
    /// Built in place from a directory the user named.
    Local(PathBuf),
    /// Cloned from a git remote.
    Git { url: String, rev: Option<String> },
    /// Downloaded ready-built.
    Prebuilt { url: String },
    /// PRL.3: a component published as a GitHub release asset — what
    /// `require-release` installs. `api` is the plugin API generation it was
    /// installed for: an editor speaking another one needs another build, so a
    /// mismatch means "download again", not "use what is there".
    Release {
        repo: String,
        version: String,
        sha256: Option<String>,
        api: String,
        /// The spec named no version: `version` is the tag it resolved to,
        /// and an update looks for a newer one. Without this the record of
        /// an unpinned install reads back as pinned to that tag, and
        /// `:plugin-update` would fetch the same release again.
        latest: bool,
    },
    /// Present on disk with no marker — hand-installed, or installed by a
    /// lattice older than PM.8. Explicitly a *state*, not a fallback to
    /// `Local`: claiming a source we do not know would make the column lie.
    Unknown,
}

impl SourceRecord {
    /// The view's SOURCE cell.
    pub fn label(&self) -> String {
        match self {
            SourceRecord::Bundled => "bundled".to_string(),
            SourceRecord::Local(_) => "local".to_string(),
            SourceRecord::Git { rev: Some(rev), .. } => {
                // Short rev — a full 40-char sha would dominate the row and
                // the first 7 are what a user recognises.
                format!("git@{}", &rev[..rev.len().min(7)])
            }
            SourceRecord::Git { rev: None, .. } => "git".to_string(),
            SourceRecord::Prebuilt { .. } => "prebuilt".to_string(),
            SourceRecord::Release { repo, version, .. } => format!("release {repo}@{version}"),
            SourceRecord::Unknown => "—".to_string(),
        }
    }

    /// Can the editor rebuild this from source? `Prebuilt` and `Bundled`
    /// cannot — there is nothing to build — and `Unknown` has nowhere to
    /// build from.
    pub fn is_buildable(&self) -> bool {
        matches!(self, SourceRecord::Local(_) | SourceRecord::Git { .. })
    }

    /// The resolver input this record describes, for a rebuild (PM.8b).
    pub fn as_plugin_source(&self) -> Option<PluginSource> {
        match self {
            SourceRecord::Local(path) => Some(PluginSource::Local(path.clone())),
            SourceRecord::Git { url, rev } => Some(PluginSource::Git {
                url: url.clone(),
                rev: rev.clone(),
            }),
            SourceRecord::Prebuilt { url } => Some(PluginSource::Prebuilt { url: url.clone() }),
            SourceRecord::Release {
                repo,
                version,
                sha256,
                latest,
                ..
            } => Some(PluginSource::Release {
                repo: repo.clone(),
                version: (!latest).then(|| version.clone()),
                sha256: sha256.clone(),
            }),
            SourceRecord::Bundled | SourceRecord::Unknown => None,
        }
    }

    pub(crate) fn from_plugin_source(source: &PluginSource) -> Self {
        match source {
            PluginSource::Local(p) => SourceRecord::Local(p.clone()),
            PluginSource::Git { url, rev } => SourceRecord::Git {
                url: url.clone(),
                rev: rev.clone(),
            },
            PluginSource::Prebuilt { url } => SourceRecord::Prebuilt { url: url.clone() },
            // An unpinned spec is recorded as the tag it resolved to — the
            // installer writes the record once it knows it (PRL.4); until then
            // there is nothing to record a version of.
            PluginSource::Release {
                repo,
                version,
                sha256,
            } => SourceRecord::Release {
                repo: repo.clone(),
                version: version.clone().unwrap_or_default(),
                sha256: sha256.clone(),
                api: lattice_wit::api_generation().to_string(),
                latest: version.is_none(),
            },
        }
    }

    fn to_file(&self) -> String {
        match self {
            SourceRecord::Bundled => "kind = bundled\n".to_string(),
            SourceRecord::Local(p) => format!("kind = local\npath = {}\n", p.display()),
            SourceRecord::Git { url, rev } => {
                let mut s = format!("kind = git\nurl = {url}\n");
                if let Some(rev) = rev {
                    s.push_str(&format!("rev = {rev}\n"));
                }
                s
            }
            SourceRecord::Prebuilt { url } => format!("kind = prebuilt\nurl = {url}\n"),
            SourceRecord::Release {
                repo,
                version,
                sha256,
                api,
                latest,
            } => {
                let mut s = format!("kind = release\nrepo = {repo}\nversion = {version}\n");
                if let Some(sha256) = sha256 {
                    s.push_str(&format!("sha256 = {sha256}\n"));
                }
                s.push_str(&format!("api = {api}\n"));
                if *latest {
                    s.push_str("latest = true\n");
                }
                s
            }
            SourceRecord::Unknown => "kind = unknown\n".to_string(),
        }
    }

    fn parse(text: &str) -> Self {
        let mut kind = "";
        let mut path = "";
        let mut url = "";
        let mut rev: Option<String> = None;
        let mut repo = "";
        let mut version = "";
        let mut sha256: Option<String> = None;
        let mut api = "";
        let mut latest = false;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "kind" => kind = v,
                "path" => path = v,
                "url" => url = v,
                "rev" if !v.is_empty() => rev = Some(v.to_string()),
                "repo" => repo = v,
                "version" => version = v,
                "sha256" if !v.is_empty() => sha256 = Some(v.to_string()),
                "api" => api = v,
                "latest" => latest = v == "true",
                _ => {}
            }
        }
        match kind {
            "bundled" => SourceRecord::Bundled,
            "local" if !path.is_empty() => SourceRecord::Local(PathBuf::from(path)),
            "git" if !url.is_empty() => SourceRecord::Git {
                url: url.to_string(),
                rev,
            },
            "prebuilt" if !url.is_empty() => SourceRecord::Prebuilt {
                url: url.to_string(),
            },
            "release" if !repo.is_empty() && !version.is_empty() => SourceRecord::Release {
                repo: repo.to_string(),
                version: version.to_string(),
                sha256,
                api: api.to_string(),
                latest,
            },
            // A `kind` we do not know, or one missing the field that makes it
            // usable, is unknown rather than a guess.
            _ => SourceRecord::Unknown,
        }
    }
}

/// Write `plugin_dir`'s source marker. Best-effort: a failure is logged and
/// the install still counts — losing the marker costs a column cell and a
/// rebuild, not the plugin.
pub fn write(plugin_dir: &Path, source: &PluginSource) {
    write_record(plugin_dir, &SourceRecord::from_plugin_source(source));
}

/// [`write`] for a record built by hand — the release installer's, which
/// knows the tag an unpinned spec resolved to.
pub(crate) fn write_record(plugin_dir: &Path, record: &SourceRecord) {
    if let Err(e) = std::fs::write(plugin_dir.join(SOURCE_FILE), record.to_file()) {
        tracing::debug!(
            dir = %plugin_dir.display(),
            error = %e,
            "could not write the plugin source marker"
        );
    }
}

/// Read `plugin_dir`'s source marker, or [`SourceRecord::Unknown`].
pub fn read(plugin_dir: &Path) -> SourceRecord {
    match std::fs::read_to_string(plugin_dir.join(SOURCE_FILE)) {
        Ok(text) => SourceRecord::parse(&text),
        Err(_) => SourceRecord::Unknown,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("lattice-pm8-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn roundtrip(source: PluginSource) -> SourceRecord {
        let dir = tempdir("rt");
        write(&dir, &source);
        read(&dir)
    }

    #[test]
    fn a_local_source_round_trips_with_its_path() {
        // The path is what makes a rebuild possible; losing it would leave a
        // chord that knows the plugin is local but not where from.
        let got = roundtrip(PluginSource::Local(PathBuf::from("/home/u/dev/p")));
        assert_eq!(got, SourceRecord::Local(PathBuf::from("/home/u/dev/p")));
        assert_eq!(got.label(), "local");
    }

    #[test]
    fn a_git_source_round_trips_with_url_and_rev() {
        let got = roundtrip(PluginSource::Git {
            url: "https://example.invalid/p.git".into(),
            rev: Some("abc1234def".into()),
        });
        assert_eq!(
            got,
            SourceRecord::Git {
                url: "https://example.invalid/p.git".into(),
                rev: Some("abc1234def".into()),
            }
        );
        assert_eq!(
            got.label(),
            "git@abc1234",
            "the rev is shortened for the column"
        );
    }

    #[test]
    fn an_unpinned_git_source_round_trips_without_a_rev() {
        let got = roundtrip(PluginSource::Git {
            url: "https://example.invalid/p.git".into(),
            rev: None,
        });
        assert_eq!(got.label(), "git");
        assert!(matches!(got, SourceRecord::Git { rev: None, .. }));
    }

    #[test]
    fn a_prebuilt_source_round_trips_with_its_url() {
        let got = roundtrip(PluginSource::Prebuilt {
            url: "https://example.invalid/p.wasm".into(),
        });
        assert_eq!(got.label(), "prebuilt");
        assert!(
            !got.is_buildable(),
            "there is nothing to build for a prebuilt"
        );
    }

    #[test]
    fn a_directory_with_no_marker_reads_as_unknown_not_as_local() {
        // The distinction matters: claiming `local` for a hand-installed
        // plugin would put a wrong path in the column and offer a rebuild
        // that cannot work.
        let dir = tempdir("bare");
        assert_eq!(read(&dir), SourceRecord::Unknown);
        assert_eq!(read(&dir).label(), "—");
        assert!(!read(&dir).is_buildable());
    }

    #[test]
    fn a_corrupt_marker_degrades_to_unknown() {
        let dir = tempdir("corrupt");
        std::fs::write(dir.join(SOURCE_FILE), "!!! not a record @@@").unwrap();
        assert_eq!(read(&dir), SourceRecord::Unknown);
    }

    #[test]
    fn a_marker_missing_the_field_that_makes_it_usable_is_unknown() {
        // `kind = git` with no url is not a git source we can do anything
        // with; reporting it as one would offer a rebuild that fails.
        let dir = tempdir("partial");
        std::fs::write(dir.join(SOURCE_FILE), "kind = git\n").unwrap();
        assert_eq!(read(&dir), SourceRecord::Unknown);
    }

    #[test]
    fn only_buildable_sources_report_as_buildable() {
        assert!(SourceRecord::Local(PathBuf::from("/x")).is_buildable());
        assert!(
            SourceRecord::Git {
                url: "u".into(),
                rev: None
            }
            .is_buildable()
        );
        assert!(!SourceRecord::Bundled.is_buildable());
        assert!(!SourceRecord::Unknown.is_buildable());
        assert!(
            !SourceRecord::Prebuilt { url: "u".into() }.is_buildable(),
            "a prebuilt is re-downloaded, not rebuilt"
        );
    }

    #[test]
    fn a_buildable_record_converts_back_to_a_resolver_input() {
        // The rebuild chord's whole path: read the marker, hand it to the
        // resolver. A conversion that dropped the rev would silently rebuild
        // the wrong revision.
        let rec = SourceRecord::Git {
            url: "https://example.invalid/p.git".into(),
            rev: Some("abc".into()),
        };
        assert_eq!(
            rec.as_plugin_source(),
            Some(PluginSource::Git {
                url: "https://example.invalid/p.git".into(),
                rev: Some("abc".into()),
            })
        );
        assert_eq!(SourceRecord::Bundled.as_plugin_source(), None);
    }

    #[test]
    fn a_short_rev_is_not_truncated_past_its_length() {
        // Guards the slice: `&rev[..7]` on a 3-char rev would panic, and a
        // user pinning a tag or a short sha is ordinary.
        let rec = SourceRecord::Git {
            url: "u".into(),
            rev: Some("v1".into()),
        };
        assert_eq!(rec.label(), "git@v1");
    }

    /// PRL.3: a release round-trips — pinned or not — with the API generation
    /// it was installed for, and labels itself for `:plugins`.
    #[test]
    fn a_release_round_trips_and_is_labelled() {
        for sha256 in [None, Some("ab".repeat(32))] {
            let got = roundtrip(PluginSource::Release {
                repo: "dhruvasagar/lattice-org-plugin".into(),
                version: Some("v0.2.0".into()),
                sha256: sha256.clone(),
            });
            assert_eq!(
                got,
                SourceRecord::Release {
                    repo: "dhruvasagar/lattice-org-plugin".into(),
                    version: "v0.2.0".into(),
                    sha256,
                    api: lattice_wit::api_generation().to_string(),
                    latest: false,
                }
            );
            assert_eq!(got.label(), "release dhruvasagar/lattice-org-plugin@v0.2.0");
            assert!(!got.is_buildable(), "downloaded, not built");
        }
    }
}
