//! AD.6: the plugin guides quote code only through synced blocks, and name
//! only API items that exist.
//!
//! The guides (`docs/dev/guides/plugin-*.md`) are hand-written — which world
//! to target, what the host does with a result, where the traps are is
//! judgement, not something to generate. Hand-written code in them is where
//! they went stale: the authoring guide's worked example was a plugin deleted
//! months earlier, in a shape the API had since moved away from. So:
//!
//! - **Code is quoted, never typed.** A fenced block directly after
//!   `<!-- example: <id> -->` must equal that example's extraction (an
//!   `@example` region in a guest CI compiles); one after
//!   `<!-- include: <repo path> -->` must equal that whole file.
//!   `UPDATE_SITE_REFERENCE=1` rewrites them, like the reference itself.
//! - **Names resolve.** `` `<seam>.<item>` `` must name a function or type of
//!   that seam, and `` `<name>` world `` a world the catalog has.

use std::path::{Path, PathBuf};

use lattice_plugin_api::catalog;
use lattice_plugin_api::examples::scan;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The guides under test.
fn guides() -> Vec<PathBuf> {
    let dir = repo_root().join("docs/dev/guides");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read docs/dev/guides")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("plugin-") && n.ends_with(".md"))
        })
        .collect();
    out.sort();
    assert!(out.len() >= 2, "expected the plugin guides, found {out:?}");
    out
}

/// What a synced block should contain, or why it cannot be resolved.
fn source_of(directive: &str, root: &Path) -> Result<String, String> {
    if let Some(id) = directive.strip_prefix("example: ") {
        let ex = scan(root);
        return ex
            .by_id(id.trim())
            .map(|e| e.code.clone())
            .ok_or_else(|| format!("no example with id `{}`", id.trim()));
    }
    if let Some(path) = directive.strip_prefix("include: ") {
        let path = path.trim();
        return std::fs::read_to_string(root.join(path))
            .map(|s| s.trim_end().to_string())
            .map_err(|e| format!("cannot read `{path}`: {e}"));
    }
    Err(format!("unknown directive `{directive}`"))
}

/// Rewrite every synced block in `text` to its source. Returns the new text
/// and the problems (unresolvable directives, a directive with no fence).
fn sync(text: &str, root: &Path) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut problems = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line.to_string());
        let directive = line
            .trim()
            .strip_prefix("<!-- ")
            .and_then(|l| l.strip_suffix(" -->"))
            .filter(|d| d.starts_with("example: ") || d.starts_with("include: "));
        let Some(directive) = directive else {
            i += 1;
            continue;
        };
        let Some(open) = lines.get(i + 1).filter(|l| l.starts_with("```")) else {
            problems.push(format!(
                "line {}: `<!-- {directive} -->` must be followed by a fenced block",
                i + 1
            ));
            i += 1;
            continue;
        };
        let Some(close) = (i + 2..lines.len()).find(|&j| lines[j] == "```") else {
            problems.push(format!("line {}: unterminated fenced block", i + 2));
            i += 1;
            continue;
        };
        out.push(open.to_string());
        match source_of(directive, root) {
            Ok(code) => out.extend(code.lines().map(str::to_string)),
            Err(why) => {
                problems.push(format!("line {}: {why}", i + 1));
                out.extend(lines[i + 2..close].iter().map(|l| l.to_string()));
            }
        }
        out.push("```".to_string());
        i = close + 1;
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    (s, problems)
}

#[test]
fn synced_blocks_match_their_sources() {
    let root = repo_root();
    let update = std::env::var_os("UPDATE_SITE_REFERENCE").is_some();
    let mut problems = Vec::new();
    let mut synced = 0;
    for path in guides() {
        let text = std::fs::read_to_string(&path).expect("read a guide");
        synced += text.matches("<!-- example: ").count() + text.matches("<!-- include: ").count();
        let (fresh, errs) = sync(&text, &root);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        problems.extend(errs.into_iter().map(|e| format!("{name}: {e}")));
        if fresh != text {
            if update {
                std::fs::write(&path, &fresh).expect("rewrite a guide");
            } else {
                problems.push(format!(
                    "{name}: a synced block differs from its source — the quoted code \
                     changed. Regenerate with UPDATE_SITE_REFERENCE=1 cargo test -p \
                     lattice-plugin-api"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "\n  {}", problems.join("\n  "));
    assert!(synced > 10, "suspiciously few synced blocks: {synced}");
}

/// Backticked spans in `text`, outside fenced blocks, as
/// `(line, span, text right after the span)`.
fn code_spans(text: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (n, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let parts: Vec<&str> = line.split('`').collect();
        for k in (1..parts.len()).step_by(2) {
            let after = parts.get(k + 1).copied().unwrap_or("");
            out.push((n + 1, parts[k].to_string(), after.to_string()));
        }
    }
    out
}

#[test]
fn api_names_in_the_guides_resolve() {
    let cat = catalog();
    let mut problems = Vec::new();
    let mut checked = 0;
    for path in guides() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let text = std::fs::read_to_string(&path).expect("read a guide");
        for (line, span, after) in code_spans(&text) {
            // `<seam>.<item>` — a seam name, then an item of it.
            // (`types.wit` is a file, not an item.)
            let is_file = ["wit", "md", "rs", "toml"]
                .iter()
                .any(|ext| span.ends_with(&format!(".{ext}")));
            if let Some((seam, item)) = span.split_once('.')
                && !is_file
                && let Some(iface) = cat.interface(seam)
            {
                checked += 1;
                let known = iface.functions.iter().any(|f| f.display_name() == item)
                    || iface.types.iter().any(|t| t.name == item);
                if !known {
                    problems.push(format!(
                        "{name}:{line}: `{span}` — `{seam}` has no function or type `{item}`"
                    ));
                }
            }
            // `<name>` world
            if after.trim_start().starts_with("world")
                && span.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            {
                checked += 1;
                if cat.world(&span).is_none() {
                    problems.push(format!("{name}:{line}: `{span}` world does not exist"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "\n  {}", problems.join("\n  "));
    assert!(
        checked > 10,
        "suspiciously few API names checked: {checked}"
    );
}
