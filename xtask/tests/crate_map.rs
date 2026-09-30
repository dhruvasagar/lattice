//! AD.9: `docs/dev/reference/crates.md` — the workspace crate map — is
//! GENERATED from the crates themselves, and this test keeps it that way.
//!
//! Forty-odd crates is past the point where anyone holds the dependency graph
//! in their head, and a hand-kept map of it is wrong the first time a crate
//! moves. Every fact on the page comes from the source:
//!
//! - **what a crate is** — the first paragraph of the `//!` overview at the
//!   top of its `src/lib.rs` (or `src/main.rs`), which the crate's author
//!   already maintains next to the code;
//! - **what it depends on** — the `lattice-*` entries of its `Cargo.toml`
//!   `[dependencies]` (dev- and build-dependencies are not the architecture);
//! - **its layer** — the longest dependency chain beneath it, so a crate is
//!   always drawn above everything it builds on.
//!
//! `UPDATE_SITE_REFERENCE=1 cargo test -p xtask` rewrites the page — the same
//! switch that regenerates the plugin-API reference.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

const MAP: &str = "docs/dev/reference/crates.md";
const RUSTDOC: &str = "https://dhruvasagar.github.io/lattice/api";

struct Crate {
    name: String,
    summary: String,
    deps: BTreeSet<String>,
}

/// The first paragraph of the crate root's `//!` docs, on one line.
fn summary(root: &Path) -> String {
    let src = root.join("src");
    let file = ["lib.rs", "main.rs"]
        .iter()
        .map(|f| src.join(f))
        .find(|p| p.is_file());
    let Some(file) = file else {
        return String::new();
    };
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let para = text
        .lines()
        .skip_while(|l| !l.starts_with("//!"))
        .take_while(|l| l.starts_with("//!"))
        .map(|l| l.trim_start_matches("//!").trim())
        // A leading `# Title` is the crate's name, not what it is.
        .skip_while(|l| l.is_empty() || l.starts_with('#'))
        .take_while(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    flatten_links(&para)
}

/// `[text](target)` → `text`. A crate overview's links are relative to its
/// `lib.rs` (or intra-doc paths), so none of them resolves from the map.
fn flatten_links(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](").map(|m| open + m) else {
            break;
        };
        let Some(close) = rest[mid..].find(')').map(|c| mid + c) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..mid]);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Does `summary` open with a slice ID (`PO.4 —`, `PL8.H.2 —`, `M.2.b.1`,
/// `Phase 5.7:`)? Those are how a contributor finds the rationale, and they
/// belong in the overview — but the first words are what an index shows, and
/// to a reader who was not there they say nothing.
fn opens_with_slice_id(summary: &str) -> bool {
    let first = summary
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([':', ',', '—']);
    if first == "Phase" {
        return true;
    }
    let Some((head, tail)) = first.split_once('.') else {
        return false;
    };
    !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && head.starts_with(|c: char| c.is_ascii_uppercase())
        && (tail.starts_with(|c: char| c.is_ascii_digit())
            || head.chars().any(|c| c.is_ascii_digit()))
}

/// The `lattice-*` crates in `[dependencies]` — whatever form the entry takes
/// (`x.workspace = true`, `x = { path = … }`, `x = { workspace = true }`).
fn workspace_deps(manifest: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // `[dependencies]` and target-specific `[target.'…'.dependencies]`.
            in_deps = line == "[dependencies]" || line.ends_with(".dependencies]");
            continue;
        }
        if !in_deps || line.starts_with('#') {
            continue;
        }
        let key = line
            .split(['=', '.', ' '])
            .next()
            .unwrap_or("")
            .trim_matches('"');
        if key.starts_with("lattice-") {
            out.insert(key.to_string());
        }
    }
    out
}

fn crates() -> Vec<Crate> {
    let dir = repo_root().join("crates");
    let mut out: Vec<Crate> = std::fs::read_dir(&dir)
        .expect("read crates/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .map(|p| {
            let manifest = std::fs::read_to_string(p.join("Cargo.toml")).unwrap_or_default();
            Crate {
                name: p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string(),
                summary: summary(&p),
                deps: workspace_deps(&manifest),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Longest dependency chain beneath each crate (0 = depends on no workspace
/// crate).
fn layers(crates: &[Crate]) -> BTreeMap<String, usize> {
    fn depth(
        name: &str,
        deps: &BTreeMap<&str, &BTreeSet<String>>,
        memo: &mut BTreeMap<String, usize>,
        stack: &mut Vec<String>,
    ) -> usize {
        if let Some(&d) = memo.get(name) {
            return d;
        }
        assert!(
            !stack.iter().any(|s| s == name),
            "dependency cycle through {name}: {stack:?}"
        );
        stack.push(name.to_string());
        let d = deps
            .get(name)
            .map(|ds| {
                ds.iter()
                    .filter(|d| deps.contains_key(d.as_str()))
                    .map(|d| 1 + depth(d, deps, memo, stack))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        stack.pop();
        memo.insert(name.to_string(), d);
        d
    }
    let deps: BTreeMap<&str, &BTreeSet<String>> =
        crates.iter().map(|c| (c.name.as_str(), &c.deps)).collect();
    let mut memo = BTreeMap::new();
    for c in crates {
        depth(&c.name, &deps, &mut memo, &mut Vec::new());
    }
    memo
}

fn render(crates: &[Crate]) -> String {
    let layer = layers(crates);
    let mut used_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for c in crates {
        for d in &c.deps {
            used_by.entry(d.as_str()).or_default().push(c.name.as_str());
        }
    }
    let top = layer.values().copied().max().unwrap_or(0);

    let mut out = String::from(
        "<!-- @generated from crates/*/Cargo.toml and each crate root's `//!` overview\n     \
         by xtask/tests/crate_map.rs. Do not edit: run \
         `UPDATE_SITE_REFERENCE=1 cargo test -p xtask`. -->\n\n",
    );
    out.push_str("# Crate map\n\n");
    out.push_str(&format!(
        "The workspace's {} crates, layered by dependency: a crate appears in a \
         layer above everything it depends on, so layer 0 is the foundation and \
         layer {top} is the binary. Each summary is the first paragraph of the \
         crate's own overview (`src/lib.rs`), and each name links to its \
         published rustdoc. Only `lattice-*` runtime dependencies are shown — \
         dev- and build-dependencies are not the architecture.\n\n\
         Why a crate exists at all is a design rule, not an accident of history: \
         a new crate needs a new *mechanism* — a dependency surface it must keep \
         out, enforced structurally — not merely a new feature. See heuristic #6 \
         in `CLAUDE.md`.\n",
        crates.len()
    ));
    for l in 0..=top {
        let mut in_layer: Vec<&Crate> = crates.iter().filter(|c| layer[&c.name] == l).collect();
        if in_layer.is_empty() {
            continue;
        }
        in_layer.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str(&format!("\n## Layer {l}\n\n"));
        out.push_str("| Crate | What it is | Depends on | Used by |\n|---|---|---|---|\n");
        for c in in_layer {
            let list = |names: Vec<&str>| {
                if names.is_empty() {
                    "—".to_string()
                } else {
                    names
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            out.push_str(&format!(
                "| [`{}`]({RUSTDOC}/{}/) | {} | {} | {} |\n",
                c.name,
                c.name.replace('-', "_"),
                c.summary.replace('|', "\\|"),
                list(c.deps.iter().map(String::as_str).collect()),
                list(used_by.get(c.name.as_str()).cloned().unwrap_or_default()),
            ));
        }
    }
    out
}

#[test]
fn the_crate_map_matches_the_workspace() {
    let rendered = render(&crates());
    let path = repo_root().join(MAP);
    if std::env::var_os("UPDATE_SITE_REFERENCE").is_some() {
        std::fs::write(&path, &rendered).expect("write the crate map");
        return;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        on_disk == rendered,
        "\n{MAP} is stale — a crate was added, removed, re-depended or its \
         overview changed.\nRegenerate with:\n  UPDATE_SITE_REFERENCE=1 cargo test -p xtask\n"
    );
}

/// The map is only as good as the overviews it quotes: every crate root must
/// open with a `//!` paragraph saying what the crate is — in words, not a
/// slice ID.
#[test]
fn every_crate_root_opens_with_an_overview() {
    let mut problems = Vec::new();
    for c in crates() {
        if c.summary.len() < 20 {
            problems.push(format!(
                "{}: src/lib.rs does not open with a `//!` paragraph saying what the crate is",
                c.name
            ));
        } else if opens_with_slice_id(&c.summary) {
            problems.push(format!(
                "{}: the overview opens with a slice ID (\"{}\") — say what the crate \
                 is first, and keep the ID for later in the sentence",
                c.name,
                c.summary.split_whitespace().next().unwrap_or("")
            ));
        }
    }
    assert!(problems.is_empty(), "\n  {}", problems.join("\n  "));
}

#[test]
fn slice_ids_and_links_are_recognised() {
    for yes in [
        "PO.4 — x",
        "PL8.H.2 — x",
        "M.2.b.1 (x)",
        "Phase 5.7: x",
        "NOTIF.1a — x",
    ] {
        assert!(opens_with_slice_id(yes), "{yes}");
    }
    for no in [
        "Core editor state.",
        "`lattice-lsp` -- the LSP client",
        "Vertico-style picker",
    ] {
        assert!(!opens_with_slice_id(no), "{no}");
    }
    assert_eq!(
        flatten_links("see [`x`](../y.md) and [z](#a)."),
        "see `x` and z."
    );
}

#[test]
fn workspace_deps_reads_every_entry_form() {
    let deps = workspace_deps(
        "[package]\nname = \"x\"\n\
         [dependencies]\nlattice-a.workspace = true\nlattice-b = { path = \"../b\" }\n\
         lattice-c = { workspace = true }\nserde = \"1\"\n# lattice-z = \"0\"\n\
         [dev-dependencies]\nlattice-d = { path = \"../d\" }\n\
         [target.'cfg(unix)'.dependencies]\nlattice-e.workspace = true\n",
    );
    let names: Vec<&str> = deps.iter().map(String::as_str).collect();
    assert_eq!(names, ["lattice-a", "lattice-b", "lattice-c", "lattice-e"]);
}
