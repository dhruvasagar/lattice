//! Every screenshot a surface references is present, in both places, in budget.
//!
//! Published screenshots live in two directories and nothing mirrors them:
//!
//!   * `assets/media/screenshots/` — what `README.md` references;
//!   * `site/static/media/` — what the site serves via `get_url(path='media/…')`.
//!
//! Miss the second and the README looks right while the site 404s, which is
//! exactly the failure mode a human reviewer does not catch: the image renders
//! fine in the diff. So the reference is checked against the file rather than
//! against a reviewer's memory.
//!
//! This also guards the *absence* rule the launch plan set: a page never gets a
//! placeholder `<img>` for an asset that does not exist, because a broken image
//! is worse than an absent one. `site/data/gallery.toml` ships with `shots = []`
//! and renders nothing; the moment a shot is added there, this test insists the
//! file is real and mirrored before it can land.
//!
//! Budget (`docs/media/README.md`): screenshots under 400 KB — they load on the
//! landing page and land in every clone.
//!
//! Conventions, per-shot setup and destinations: `docs/media/screenshot-ideas.md`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// Screenshots are capped at 400 KB — `docs/media/README.md`'s size budget.
const MAX_BYTES: u64 = 400 * 1024;

const README_DIR: &str = "assets/media/screenshots";
const SITE_DIR: &str = "site/static/media";

fn workspace_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every `basename` captured by `open`…`close` around a literal in `haystack`.
fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut rest = haystack;
    while let Some(start) = rest.find(open) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(close) else { break };
        found.push(&rest[..end]);
        rest = &rest[end..];
    }
    found
}

/// The shot filenames listed in `site/data/gallery.toml`, ignoring the
/// commented-out template entries the file carries as documentation.
fn gallery_shots() -> Vec<String> {
    read("site/data/gallery.toml")
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let rest = line.strip_prefix("file")?.trim_start().strip_prefix('=')?;
            Some(rest.trim().trim_matches('"').to_owned())
        })
        .collect()
}

fn missing(dir: &str, file: &str) -> Option<String> {
    let path = workspace_root().join(dir).join(file);
    (!path.exists()).then(|| format!("{dir}/{file}"))
}

#[test]
fn readme_screenshot_references_resolve() {
    let readme = read("README.md");
    let refs = between(&readme, "./assets/media/screenshots/", "\"");
    let gaps: Vec<String> = refs
        .iter()
        .filter_map(|file| missing(README_DIR, file))
        .collect();
    assert!(
        gaps.is_empty(),
        "README.md references screenshots that do not exist — GitHub renders a \
         broken image:\n  {}",
        gaps.join("\n  ")
    );
}

#[test]
fn site_media_references_resolve() {
    let mut gaps = Vec::new();
    let templates = workspace_root().join("site/templates");
    for entry in std::fs::read_dir(&templates).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "html") {
            continue;
        }
        let html = std::fs::read_to_string(&path).unwrap();
        // `get_url(path='media/<file>')`. The gallery's own reference is built
        // by concatenation (`'media/' ~ shot.file`) and is covered by
        // `gallery_shots_are_present_in_both_directories` instead.
        for file in between(&html, "get_url(path='media/", "'") {
            if let Some(gap) = missing(SITE_DIR, file) {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                gaps.push(format!("{gap} (referenced by site/templates/{name})"));
            }
        }
    }
    assert!(
        gaps.is_empty(),
        "a site template references media that is not in {SITE_DIR} — the page \
         builds clean and 404s in the browser:\n  {}",
        gaps.join("\n  ")
    );
}

#[test]
fn gallery_shots_are_present_in_both_directories() {
    let mut gaps = Vec::new();
    for file in gallery_shots() {
        gaps.extend(missing(README_DIR, &file));
        gaps.extend(missing(SITE_DIR, &file));
    }
    assert!(
        gaps.is_empty(),
        "site/data/gallery.toml lists shots whose files are missing. Every \
         published screenshot is committed to BOTH directories — nothing \
         mirrors them:\n  {}",
        gaps.join("\n  ")
    );
}

#[test]
fn published_screenshots_stay_within_the_size_budget() {
    let mut over = Vec::new();
    for dir in [README_DIR, SITE_DIR] {
        let path = workspace_root().join(dir);
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries {
            let file: PathBuf = entry.unwrap().path();
            if file.extension().is_none_or(|e| e != "png") {
                continue;
            }
            let bytes = file.metadata().unwrap().len();
            if bytes > MAX_BYTES {
                let name = file.file_name().unwrap().to_string_lossy().into_owned();
                over.push(format!("{dir}/{name} — {} KB", bytes / 1024));
            }
        }
    }
    assert!(
        over.is_empty(),
        "screenshots over the {} KB budget (docs/media/README.md) — these load \
         on the landing page and land in every clone. Downscale to 1920px wide \
         and re-encode:\n  {}",
        MAX_BYTES / 1024,
        over.join("\n  ")
    );
}
