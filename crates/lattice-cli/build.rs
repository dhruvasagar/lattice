//! The build stamp: what `:version` and `lattice --version --verbose` report
//! about the binary they are running in.
//!
//! WT.1 moved the `wit/` embedding to the `lattice-wit` crate, which is what a
//! PLUGIN depends on as well — one copy of the API package rather than one for
//! the scaffold and one for everyone else. What is left here is the stamp.
//!
//! Every value is emitted as a `LATTICE_BUILD_*` compile-time env var, read
//! back with `env!`. Three of them come from git, and git is not always there:
//! a source tarball and a crates.io build have no `.git`. So each git value is
//! taken FIRST from a same-named environment variable — the release pipeline
//! sets them from the tag it is building — then from `git`, and only then
//! reads `unknown`. A stamp that guesses is worse than one that says it does
//! not know: the commit is the first thing a bug report is read for.
//!
//! The commit date is recorded, not the build time. A build-time stamp would
//! make two builds of the same commit differ byte for byte for no information
//! a bug report needs.

use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();

    let sha = from_env_or("LATTICE_GIT_SHA", || {
        git(&manifest_dir, &["rev-parse", "--short=12", "HEAD"])
    });
    let date = from_env_or("LATTICE_GIT_COMMIT_DATE", || {
        git(&manifest_dir, &["log", "-1", "--format=%cs"])
    });
    // Tracked changes only: an untracked scratch file does not make the build
    // differ from its commit. `--no-optional-locks` keeps `status` from
    // refreshing `.git/index`, which this script watches — without it every
    // build would rewrite the index and re-run the script that read it.
    let dirty = from_env_or("LATTICE_GIT_DIRTY", || {
        git(
            &manifest_dir,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=no",
            ],
        )
        .map(|out| if out.is_empty() { "clean" } else { "dirty" }.to_string())
    });

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let features = if std::env::var_os("CARGO_FEATURE_GUI").is_some() {
        "gui"
    } else {
        "tui"
    };

    emit("LATTICE_BUILD_GIT_SHA", sha);
    emit("LATTICE_BUILD_GIT_COMMIT_DATE", date);
    emit("LATTICE_BUILD_GIT_DIRTY", dirty);
    emit("LATTICE_BUILD_RUSTC", rustc_version);
    emit("LATTICE_BUILD_PROFILE", std::env::var("PROFILE").ok());
    emit("LATTICE_BUILD_TARGET", std::env::var("TARGET").ok());
    emit("LATTICE_BUILD_FEATURES", Some(features.to_string()));

    // The plugin API and the wasmtime that runs it, from the lockfile — the
    // versions this binary actually linked, not the ranges a manifest allows.
    // A build with no workspace lockfile beside it reports `unknown`.
    let lock_path = std::path::Path::new(&manifest_dir).join("../../Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap_or_default();
    emit(
        "LATTICE_BUILD_LATTICE_WIT",
        locked_version(&lock, "lattice-wit"),
    );
    emit("LATTICE_BUILD_WASMTIME", locked_version(&lock, "wasmtime"));
    println!("cargo:rerun-if-changed={}", lock_path.display());

    for var in [
        "LATTICE_GIT_SHA",
        "LATTICE_GIT_COMMIT_DATE",
        "LATTICE_GIT_DIRTY",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    // Re-stamp when the commit moves or the index changes. A worktree edit
    // that is not staged does not re-run this script, so `dirty` is as of the
    // last time it ran — the price of not rebuilding the binary on every save.
    if let Some(git_dir) = git(&manifest_dir, &["rev-parse", "--absolute-git-dir"]) {
        for path in ["HEAD", "index"] {
            println!("cargo:rerun-if-changed={git_dir}/{path}");
        }
        if let Some(head_ref) = git(&manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// `var` from the environment when set and non-empty, else `fallback()`.
fn from_env_or(var: &str, fallback: impl FnOnce() -> Option<String>) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(fallback)
}

/// A git command's trimmed stdout, or `None` when git is missing, the
/// directory is not a checkout, or the command fails.
fn git(dir: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The version `Cargo.lock` records for `package`, the newest when it holds
/// several (wasmtime appears twice: the runtime, and an older copy a
/// C-API crate pulls in).
fn locked_version(lock: &str, package: &str) -> Option<String> {
    let name_line = format!("name = \"{package}\"");
    let mut lines = lock.lines();
    let mut found: Vec<Vec<u64>> = Vec::new();
    while let Some(line) = lines.next() {
        if line.trim() != name_line {
            continue;
        }
        if let Some(version) = lines
            .next()
            .and_then(|v| v.trim().strip_prefix("version = \""))
            .and_then(|v| v.strip_suffix('"'))
        {
            found.push(version.split('.').filter_map(|p| p.parse().ok()).collect());
        }
    }
    found.into_iter().max().map(|parts| {
        parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    })
}

fn emit(key: &str, value: Option<String>) {
    let value = value.unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env={key}={value}");
}
