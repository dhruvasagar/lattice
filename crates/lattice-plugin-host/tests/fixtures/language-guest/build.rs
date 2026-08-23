//! Builds the fixture's grammar to wasm, the way a real plugin would.
//!
//! LG.4 expects a language plugin to compile its grammar as part of its own
//! build; this fixture does the same thing with `tree-sitter-markdown`, whose
//! C sources are already vendored in the cargo registry as a dependency of
//! `lattice-syntax`. The repo's `scripts/build-wasm-grammar.sh` does the work
//! — clang plus a rustup toolchain, no emscripten or docker.
//!
//! When the grammar cannot be built the fixture still compiles, embedding
//! empty bytes. That keeps a missing toolchain a SKIPPED test rather than a
//! build failure across the whole workspace, and the host rejects empty
//! grammar bytes anyway, so the failure mode stays legible.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let dest = out.join("grammar.wasm");

    // .../crates/lattice-plugin-host/tests/fixtures/language-guest → repo root
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .expect("fixture sits five levels below the repo root")
        .to_path_buf();
    let script = repo.join("scripts/build-wasm-grammar.sh");

    if let Some(src) = markdown_src() {
        let status = std::process::Command::new("bash")
            .arg(&script)
            .arg("markdown")
            .arg(&src)
            .arg(out.join("wasm-grammars"))
            .current_dir(&repo)
            .status();
        let built = out.join("wasm-grammars/tree-sitter-markdown.wasm");
        if matches!(status, Ok(s) if s.success()) && built.is_file() {
            std::fs::copy(&built, &dest).expect("copy grammar");
            return;
        }
        println!("cargo:warning=language-guest: grammar build failed; embedding empty bytes");
    } else {
        println!("cargo:warning=language-guest: tree-sitter-md sources not found");
    }
    std::fs::write(&dest, b"").expect("write placeholder");
}

/// The vendored `tree-sitter-markdown` C sources, found the same way
/// `lattice-syntax`'s own test finds them.
fn markdown_src() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LATTICE_TS_MD_SRC") {
        let p = PathBuf::from(p);
        return p.join("parser.c").is_file().then_some(p);
    }
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .ok()?;
    let mut best: Option<PathBuf> = None;
    for index in std::fs::read_dir(cargo_home.join("registry/src")).ok()? {
        let Ok(index) = index else { continue };
        let Ok(entries) = std::fs::read_dir(index.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with("tree-sitter-md-") {
                continue;
            }
            let src = e.path().join("tree-sitter-markdown/src");
            if src.join("parser.c").is_file()
                && best.as_ref().is_none_or(|b| b.as_path() < src.as_path())
            {
                best = Some(src);
            }
        }
    }
    best
}
