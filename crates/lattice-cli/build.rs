//! Embeds lattice's `wit/` API package into the binary so `lattice --init` can
//! write a **self-contained, buildable** starter config into
//! `~/.config/lattice/init/` — the scaffold's `wit/` is exactly the API of the
//! editor that generated it (no separate checkout, no version drift). This is
//! the plugin *API definition* (versioned with the editor), not a plugin binary,
//! so embedding it is appropriate (contrast: plugin `.wasm` ships separately).

use std::path::Path;

fn main() {
    let wit_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit");

    // World-only example/fixture files a user's config scaffold has no use for
    // (the base interfaces they'd need are included below).
    const EXCLUDE: &[&str] = &[
        "auto-pair.wit",
        "init-fixture.wit",
        "multiseam-fixture.wit",
        "trampoline-fixture.wit",
    ];

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&wit_dir)
        .expect("wit/ dir readable at build time")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wit"))
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !EXCLUDE.contains(&name)
        })
        .collect();
    files.sort();

    let mut code = String::from(
        "/// lattice's `wit/` API package, embedded for `--init` scaffolding \
         (build.rs).\npub static WIT_FILES: &[(&str, &str)] = &[\n",
    );
    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap();
        let abs = std::fs::canonicalize(path).unwrap();
        code.push_str(&format!(
            "    ({name:?}, include_str!({:?})),\n",
            abs.to_str().unwrap()
        ));
        println!("cargo:rerun-if-changed={}", path.display());
    }
    code.push_str("];\n");

    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(Path::new(&out).join("wit_assets.rs"), code).unwrap();
    println!("cargo:rerun-if-changed={}", wit_dir.display());
}
