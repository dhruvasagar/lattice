//! PRL.7: what `scripts/build-plugin.sh` publishes is what `require-release`
//! installs.
//!
//! The producer (a shell script a plugin's CI runs) and the consumer (the
//! loader's release installer) agree on three things nothing else ties
//! together: the asset's name, the checksum file's format, and the manifest
//! being inside the component. Each half has its own tests; this is the one
//! that fails when they drift. The real script packages the real bundled
//! `auto-pair` component, and the installer installs those exact files, served
//! at the URLs it asks for.
//!
//! Skips when `auto-pair` has not been built for `wasm32-wasip2` (the
//! plugin-host build script builds it where the target is installed), when
//! there is no `bash` / `python3` to run the script, and on Windows.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use lattice_plugin_loader::resolve::{
    Fetcher, GitRunner, PluginSource, RefreshPolicy, Resolved, release_asset_name,
    release_asset_url, resolve,
};

/// Serves the packaged files at the release URLs; anything else is a 404.
struct Release(HashMap<String, PathBuf>);

impl Fetcher for Release {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        let path = self
            .0
            .get(url)
            .ok_or_else(|| format!("GET {url}: HTTP 404"))?;
        std::fs::copy(path, dest)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// A release install never runs git.
struct NoGit;

impl GitRunner for NoGit {
    fn run(&self, _cwd: &Path, args: &[&str]) -> Result<String, String> {
        panic!("a release install ran git {args:?}")
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn what_the_script_publishes_the_installer_installs() {
    let root = repo_root();
    let plugin = root.join("plugins/auto-pair");
    if !plugin
        .join("target/wasm32-wasip2/release/auto_pair.wasm")
        .is_file()
    {
        eprintln!("skipping: plugins/auto-pair is not built for wasm32-wasip2");
        return;
    }
    // The script runs where it is used: the release workflow, on Linux. On a
    // Windows runner `bash` is WSL's launcher, usually with no distribution
    // behind it, so it fails without saying why.
    if cfg!(windows) {
        eprintln!("skipping: build-plugin.sh runs on Linux (the release workflow)");
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let packaged = match Command::new("bash")
        .arg(root.join("scripts/build-plugin.sh"))
        .arg("--no-build")
        .arg(&plugin)
        .arg(out.path())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("skipping: cannot run bash: {e}");
            return;
        }
    };
    assert!(
        packaged.status.success(),
        "the script packages auto-pair: {}",
        String::from_utf8_lossy(&packaged.stderr)
    );

    // The asset the installer will ask for is the one the script wrote.
    let tag = "v0.0.0";
    let asset = release_asset_name("auto-pair", tag);
    let file = out.path().join(&asset);
    assert!(
        file.is_file(),
        "the script names the asset `{asset}` as the installer expects; it wrote: {}",
        String::from_utf8_lossy(&packaged.stdout)
    );
    let repo = "example/auto-pair";
    let url = release_asset_url(repo, tag, &asset);
    let server = Release(HashMap::from([
        (
            format!("{url}.sha256"),
            out.path().join(format!("{asset}.sha256")),
        ),
        (url, file.clone()),
    ]));

    let user = tempfile::tempdir().unwrap();
    let got = resolve(
        &NoGit,
        &server,
        &PluginSource::Release {
            repo: repo.into(),
            version: Some(tag.into()),
            sha256: None,
        },
        "auto-pair",
        &user.path().join("cache"),
        user.path(),
        RefreshPolicy::UseCache,
    )
    .unwrap_or_else(|e| panic!("the installer accepts what the script published: {e}"));

    let installed = user.path().join("auto-pair").join("auto-pair.wasm");
    assert_eq!(got, Resolved::Artifact(installed.clone()));
    assert_eq!(
        std::fs::read(&installed).unwrap(),
        std::fs::read(&file).unwrap()
    );
    // And the editor would load it as auto-pair, from its embedded manifest.
    let found = lattice_plugin_loader::discover_one(&user.path().join("auto-pair")).unwrap();
    assert_eq!(found.manifest.id, "auto-pair");
}
