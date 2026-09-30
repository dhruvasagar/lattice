//! AD.5: the WIT's prose points at things that exist.
//!
//! The reference is generated from `wit/`, so it cannot disagree with the
//! WIT's *shape*. It republishes the WIT's *prose* verbatim, though, and prose
//! rots: the doc comments named `plugins/fuzzy-finder` for months after it was
//! deleted, and cited a `source.rs` line number as if that line would stay
//! put. A reader — and an agent more so — follows those pointers.
//!
//! Every `///` line in `crates/lattice-wit/wit/*.wit` is checked for four
//! kinds of pointer:
//!
//! - a repository path (`plugins/x`, `crates/x/src/y.rs`, `docs/...`) must
//!   exist;
//! - a design-doc name (`plugin-host.md`) must be a file under `docs/` or at
//!   the repository root;
//! - a Rust path into the workspace (`lattice_picker::outcome::PickerAcceptOutcome`)
//!   must name a crate that exists and an item that crate defines;
//! - a `file.rs:NNN` line reference is refused outright — no test can say
//!   whether line NNN is still the right line, so it is guaranteed to rot.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `(file:line, doc text)` for every `///` line in the WIT package.
fn wit_doc_lines() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/lattice-wit/wit");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read wit/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wit"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let name = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let text = std::fs::read_to_string(&f).expect("read a .wit file");
        for (n, line) in text.lines().enumerate() {
            if let Some(doc) = line.trim_start().strip_prefix("///") {
                out.push((format!("{name}:{}", n + 1), doc.to_string()));
            }
        }
    }
    assert!(
        out.len() > 1000,
        "suspiciously few WIT doc lines: {}",
        out.len()
    );
    out
}

/// Tokens in `text` that start with one of `prefixes` at a word boundary,
/// running over path characters. Trailing punctuation a sentence adds is
/// trimmed.
fn tokens_starting<'a>(text: &'a str, prefixes: &[&str]) -> Vec<&'a str> {
    let is_path = |c: char| c.is_ascii_alphanumeric() || "_-./:".contains(c);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    for (i, &(at, _)) in chars.iter().enumerate() {
        let boundary = i == 0 || !is_path(chars[i - 1].1);
        if !boundary || !prefixes.iter().any(|p| text[at..].starts_with(p)) {
            continue;
        }
        let end = text[at..]
            .find(|c: char| !is_path(c))
            .map(|n| at + n)
            .unwrap_or(text.len());
        out.push(text[at..end].trim_end_matches(['.', ':', ',']));
    }
    out
}

#[test]
fn repository_paths_in_wit_docs_exist() {
    let root = repo_root();
    let mut missing = Vec::new();
    for (at, doc) in wit_doc_lines() {
        for path in tokens_starting(
            &doc,
            &["plugins/", "crates/", "docs/", "xtask/", "scripts/"],
        ) {
            if !root.join(path).exists() {
                missing.push(format!("{at}: `{path}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "WIT docs name repository paths that do not exist (renamed? deleted?):\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn design_doc_names_in_wit_docs_exist() {
    let root = repo_root();
    let mut known = BTreeSet::new();
    collect_md(&root.join("docs"), &mut known);
    for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        if let Some(name) = e.file_name().to_str()
            && name.ends_with(".md")
        {
            known.insert(name.to_string());
        }
    }
    let mut missing = Vec::new();
    for (at, doc) in wit_doc_lines() {
        for word in doc.split(|c: char| !(c.is_ascii_alphanumeric() || "_-.".contains(c))) {
            let word = word.trim_end_matches('.');
            if word.ends_with(".md") && !known.contains(word) {
                missing.push(format!("{at}: `{word}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "WIT docs cite design docs that do not exist:\n  {}",
        missing.join("\n  ")
    );
}

fn collect_md(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_md(&p, out);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str())
            && name.ends_with(".md")
        {
            out.insert(name.to_string());
        }
    }
}

/// `lattice_picker::outcome::PickerAcceptOutcome` must name a workspace
/// crate that defines `PickerAcceptOutcome`. Deliberately a name check, not
/// a module-path check: modules get reorganised without the type changing
/// meaning, and a reader searching for the name still finds it.
#[test]
fn rust_paths_in_wit_docs_resolve() {
    let root = repo_root();
    let mut crate_src: std::collections::BTreeMap<String, String> = Default::default();
    let mut unresolved = Vec::new();
    for (at, doc) in wit_doc_lines() {
        for path in tokens_starting(&doc, &["lattice_"]) {
            let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
            let Some(krate) = segs.first() else { continue };
            let dir = root.join("crates").join(krate.replace('_', "-"));
            if !dir.is_dir() {
                unresolved.push(format!("{at}: `{path}` — no crate `{krate}`"));
                continue;
            }
            let Some(item) = segs.last().filter(|_| segs.len() > 1) else {
                continue;
            };
            let src = crate_src
                .entry(krate.to_string())
                .or_insert_with(|| read_rs(&dir.join("src")));
            let defined = [
                "struct", "enum", "trait", "fn", "type", "mod", "const", "static",
            ]
            .iter()
            .any(|kw| src.contains(&format!("{kw} {item}")))
                // An enum variant or a field: `Item,` / `Item(` / `Item {`.
                || [",", "(", " {"]
                    .iter()
                    .any(|tail| src.contains(&format!("{item}{tail}")));
            if !defined {
                unresolved.push(format!("{at}: `{path}` — `{krate}` defines no `{item}`"));
            }
        }
    }
    assert!(
        unresolved.is_empty(),
        "WIT docs name Rust items that do not exist:\n  {}",
        unresolved.join("\n  ")
    );
}

fn read_rs(dir: &Path) -> String {
    let mut out = String::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.push_str(&read_rs(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
        }
    }
    out
}

#[test]
fn no_line_number_references_in_wit_docs() {
    let mut refs = Vec::new();
    for (at, doc) in wit_doc_lines() {
        for word in doc.split(|c: char| c.is_whitespace() || "()`,;".contains(c)) {
            if let Some((file, line)) = word.rsplit_once(':')
                && file.ends_with(".rs")
                && !line.is_empty()
                && line
                    .trim_end_matches('.')
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                refs.push(format!("{at}: `{word}`"));
            }
        }
    }
    assert!(
        refs.is_empty(),
        "WIT docs cite source lines, which go stale on the next edit — name the \
         item instead:\n  {}",
        refs.join("\n  ")
    );
}
