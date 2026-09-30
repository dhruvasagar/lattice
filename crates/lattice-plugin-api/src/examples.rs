//! AD.3: examples for the plugin-API reference, extracted from guests CI
//! already builds.
//!
//! An example is a region of a real guest's source, marked in place:
//!
//! ```text
//! // @example grammar-callbacks.apply-operator: Toggle comments over a range
//! fn apply_operator(...) -> Result<Vec<Effect>, String> { ... }
//! // @end-example
//! ```
//!
//! The target is `<interface>`, `<interface>.<function>` (a method is
//! `<interface>.<resource>.<method>`, as the reference names it) or
//! `<interface>.<type>`. Regions are scanned from `plugins/*/src` and
//! `crates/lattice-plugin-host/tests/fixtures/*/src` — both compiled against the
//! current `wit/` by `lattice-plugin-host`'s build script, which fails the
//! build when a guest does not compile and the wasm target is installed (as
//! it is in CI). So an example here compiles, and the fixture ones also run
//! under real guest↔host tests.
//!
//! **Why scanned at test time, not in `build.rs` with the catalog.** The
//! catalog is compiled into `lattice-plugin-api`, which `lattice-host` links.
//! Declaring every guest source as a build input would make any edit to a
//! plugin or fixture rebuild this crate and so relink the whole host — the
//! exact cost `lattice-plugin-host/build.rs` documents paying once already.
//! The price of scanning here instead: the editor's in-app
//! `:export-plugin-api` renders the reference without examples; the published
//! pages, the JSON and the agent bundle carry them.

use std::fs;
use std::path::{Path, PathBuf};

/// One extracted example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiExample {
    /// `<guest>:<target>`, e.g. `comment:grammar-callbacks.apply-operator`.
    /// Unique; guides quote an example by this id.
    pub id: String,
    /// What it illustrates: `<interface>` or `<interface>.<item>`.
    pub target: String,
    /// One line saying what the example does.
    pub caption: String,
    /// Repo-relative source file, `/`-separated.
    pub source: String,
    /// The region's code, dedented, without the marker lines.
    pub code: String,
}

impl ApiExample {
    /// The interface the target names.
    pub fn interface(&self) -> &str {
        self.target.split('.').next().unwrap_or(&self.target)
    }

    /// The item within the interface, or `None` for an interface-level
    /// example.
    pub fn item(&self) -> Option<&str> {
        self.target.split_once('.').map(|(_, item)| item)
    }
}

/// Every example found, plus every malformed region — reported together so
/// one test run names all of them.
#[derive(Debug, Default)]
pub struct Examples {
    /// The well-formed examples, sorted by id.
    pub items: Vec<ApiExample>,
    /// One line per malformed region (unterminated, nested, empty, duplicate).
    pub problems: Vec<String>,
}

impl Examples {
    /// The examples whose target is exactly `target`.
    pub fn for_target(&self, target: &str) -> Vec<&ApiExample> {
        self.items.iter().filter(|e| e.target == target).collect()
    }

    /// The example with this id.
    pub fn by_id(&self, id: &str) -> Option<&ApiExample> {
        self.items.iter().find(|e| e.id == id)
    }
}

const START: &str = "// @example ";
const END: &str = "// @end-example";

/// The directories whose immediate subdirectories are guest crates, relative
/// to the repository root.
pub const GUEST_ROOTS: &[&str] = &["plugins", "crates/lattice-plugin-host/tests/fixtures"];

/// Scan every guest under [`GUEST_ROOTS`] in the repository at `repo_root`.
pub fn scan(repo_root: &Path) -> Examples {
    let mut out = Examples::default();
    for root in GUEST_ROOTS {
        let Ok(entries) = fs::read_dir(repo_root.join(root)) else {
            continue;
        };
        let mut guests: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        guests.sort();
        for guest in guests {
            let src = guest.join("src");
            if !src.is_dir() {
                continue;
            }
            let name = guest
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let mut files = Vec::new();
            rust_files(&src, &mut files);
            files.sort();
            for file in files {
                let rel = file
                    .strip_prefix(repo_root)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .replace('\\', "/");
                if let Ok(text) = fs::read_to_string(&file) {
                    extract(&name, &rel, &text, &mut out);
                }
            }
        }
    }
    out.items.sort_by(|a, b| a.id.cmp(&b.id));
    let mut seen = std::collections::BTreeSet::new();
    for e in &out.items {
        if !seen.insert(e.id.clone()) {
            out.problems.push(format!(
                "{}: a second `{}` example in guest `{}` — give each target one example per guest",
                e.source,
                e.target,
                e.id.split(':').next().unwrap_or("?")
            ));
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Extract every region in one file.
fn extract(guest: &str, source: &str, text: &str, out: &mut Examples) {
    // (target, caption, first line number, lines)
    let mut open: Option<(String, String, usize, Vec<&str>)> = None;
    for (n, line) in text.lines().enumerate() {
        let lineno = n + 1;
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(START) {
            if let Some((target, _, start, _)) = &open {
                out.problems.push(format!(
                    "{source}:{lineno}: `@example` inside the `{target}` region opened at \
                     line {start} — regions do not nest"
                ));
            }
            let (target, caption) = match rest.split_once(':') {
                Some((t, c)) => (t.trim().to_string(), c.trim().to_string()),
                None => (rest.trim().to_string(), String::new()),
            };
            if caption.is_empty() {
                out.problems.push(format!(
                    "{source}:{lineno}: `@example {target}` has no caption — write \
                     `// @example {target}: <what it shows>`"
                ));
            }
            open = Some((target, caption, lineno, Vec::new()));
        } else if trimmed == END {
            match open.take() {
                Some((target, caption, start, lines)) => {
                    let code = dedent(&lines);
                    if code.trim().is_empty() {
                        out.problems
                            .push(format!("{source}:{start}: the `{target}` region is empty"));
                        continue;
                    }
                    out.items.push(ApiExample {
                        id: format!("{guest}:{target}"),
                        target,
                        caption,
                        source: source.to_string(),
                        code,
                    });
                }
                None => out.problems.push(format!(
                    "{source}:{lineno}: `@end-example` with no open region"
                )),
            }
        } else if let Some((_, _, _, lines)) = &mut open {
            lines.push(line);
        }
    }
    if let Some((target, _, start, _)) = open {
        out.problems.push(format!(
            "{source}:{start}: the `{target}` region is never closed with `{END}`"
        ));
    }
}

/// Remove the indentation every non-blank line shares, and leading/trailing
/// blank lines. The example reads as if it were written at the left margin.
fn dedent(lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<&str> = lines
        .iter()
        .map(|l| {
            if l.len() >= indent {
                &l[indent..]
            } else {
                l.trim_start()
            }
        })
        .collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Examples {
        let mut out = Examples::default();
        extract("g", "g/src/lib.rs", text, &mut out);
        out
    }

    #[test]
    fn a_region_is_extracted_dedented_with_its_target_and_caption() {
        let ex = run(
            "fn f() {\n    // @example buffer.document.line: Read a line\n        \
             let l = doc.line(0);\n        drop(l);\n    // @end-example\n}\n",
        );
        assert!(ex.problems.is_empty(), "{:?}", ex.problems);
        let e = &ex.items[0];
        assert_eq!(e.id, "g:buffer.document.line");
        assert_eq!(e.interface(), "buffer");
        assert_eq!(e.item(), Some("document.line"));
        assert_eq!(e.caption, "Read a line");
        assert_eq!(e.code, "let l = doc.line(0);\ndrop(l);");
    }

    #[test]
    fn malformed_regions_are_problems_not_panics() {
        let unterminated = run("// @example a.b: x\nlet x = 1;\n");
        assert!(unterminated.problems[0].contains("never closed"));
        let nested = run("// @example a.b: x\n// @example a.c: y\n1;\n// @end-example\n");
        assert!(nested.problems[0].contains("do not nest"));
        let empty = run("// @example a.b: x\n\n// @end-example\n");
        assert!(empty.problems[0].contains("empty"));
        let stray = run("// @end-example\n");
        assert!(stray.problems[0].contains("no open region"));
        let uncaptioned = run("// @example a.b\n1;\n// @end-example\n");
        assert!(uncaptioned.problems[0].contains("no caption"));
    }
}
