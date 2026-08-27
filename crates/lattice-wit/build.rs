//! Embeds `wit/` and computes the ABI fingerprint at build time.
//!
//! Both live here rather than at either consumer so the *builder* of a plugin
//! and the *loader* of its artifact cannot disagree about what "the ABI" is —
//! one definition, two readers.

use std::path::Path;

/// World-only fixture files no plugin has a use for: another plugin's world
/// (`auto-pair`) and the host's own test fixtures. The interfaces those worlds
/// compose from ship in their own files and are included.
const EXCLUDE: &[&str] = &[
    "auto-pair.wit",
    "init-fixture.wit",
    "multiseam-fixture.wit",
    "trampoline-fixture.wit",
];

fn main() {
    let wit_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit");

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&wit_dir)
        .expect("wit/ dir readable at build time")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wit"))
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !EXCLUDE.contains(&name)
        })
        .collect();
    // Sorted so the fingerprint is a function of CONTENT, not of readdir order.
    files.sort();

    let mut code = String::from(
        "/// lattice's `wit/` API package, embedded at build time.\n\
         pub static FILES: &[(&str, &str)] = &[\n",
    );
    // FNV-1a over each `(name, contents)` in sorted order. A change DETECTOR,
    // not a security boundary: the zero-dependency rule is why this is not
    // sha2, and an adversarial collision is not in the threat model — the
    // question it answers is "was this artifact built against a different
    // ABI", asked of a file the user's own toolchain produced.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let fnv = |bytes: &[u8], h: &mut u64| {
        for b in bytes {
            *h ^= u64::from(*b);
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap();
        let abs = std::fs::canonicalize(path).unwrap();
        let contents = std::fs::read(path).expect("wit file readable");
        fnv(name.as_bytes(), &mut hash);
        fnv(&contents, &mut hash);
        code.push_str(&format!(
            "    ({name:?}, include_str!({:?})),\n",
            abs.to_str().unwrap()
        ));
        println!("cargo:rerun-if-changed={}", path.display());
    }
    code.push_str("];\n\n");
    code.push_str(&format!(
        "/// Fingerprint of the embedded package — see the crate docs.\n\
         pub const ABI_FINGERPRINT: &str = \"{hash:016x}\";\n"
    ));

    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(Path::new(&out).join("wit_assets.rs"), code).unwrap();
    println!("cargo:rerun-if-changed={}", wit_dir.display());
}
