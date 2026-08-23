//! Fetches and builds org's grammar to wasm.
//!
//! The grammar is deliberately not vendored — `parser.c` is 2.2 MB of
//! generated C, and design §1's whole argument is that a grammar maintained
//! outside crates.io is the PLUGIN's build artefact, not the editor's. So it
//! is cloned here, on this plugin's own build, which is also what the plugin
//! manager does when a user `require`s this plugin from git.
//!
//! Offline, this writes empty bytes rather than failing. The host rejects an
//! empty grammar with a named reason, so the failure stays legible instead of
//! becoming a build error in something that was only ever a reference.

use std::path::PathBuf;

const REPO: &str = "https://github.com/nvim-orgmode/tree-sitter-org";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=queries");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let dest = out.join("grammar.wasm");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("grammar-src");

    if !src.join("src/parser.c").is_file() {
        let _ = std::fs::remove_dir_all(&src);
        let ok = std::process::Command::new("git")
            .args(["clone", "--depth", "1", "--quiet", REPO])
            .arg(&src)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            println!("cargo:warning=org-plugin: could not fetch the grammar (offline?)");
            std::fs::write(&dest, b"").expect("write placeholder");
            return;
        }
    }

    // The repo's own builder: clang + rustup, no emscripten or docker.
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/build-wasm-grammar.sh");
    let ok = std::process::Command::new("bash")
        .arg(&script)
        .arg("org")
        .arg(src.join("src"))
        .arg(out.join("wasm-grammars"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let built = out.join("wasm-grammars/tree-sitter-org.wasm");
    if ok && built.is_file() {
        std::fs::copy(&built, &dest).expect("copy grammar");
    } else {
        println!("cargo:warning=org-plugin: grammar build failed");
        std::fs::write(&dest, b"").expect("write placeholder");
    }
}
