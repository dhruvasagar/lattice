//! WT.1 — lattice's `wit/` API package, as a crate.
//!
//! Design: `docs/dev/architecture/wit-ownership.md`.
//!
//! **WIT is the canonical plugin API, and lattice owns it.** Every copy of
//! `wit/` in a plugin tree is a *cache* of this package, never a fork — but
//! until this crate existed nothing said so and nothing enforced it. A plugin
//! got its copy once, at scaffold time, and then it silently drifted: three ABI
//! changes in a day left two installed components unloadable and the editor
//! said nothing at all.
//!
//! The copy exists for a real reason — `wit_bindgen::generate!` resolves its
//! `path` when the macro expands, so the files must be on disk beside the crate
//! being compiled. That is a *build-time* need, and a build-time need is met by
//! the build:
//!
//! ```ignore
//! // a plugin's build.rs
//! fn main() {
//!     lattice_wit::write_to("wit").expect("write the lattice API package");
//! }
//! ```
//!
//! `wit/` becomes generated output, and which ABI a plugin targets becomes a
//! pinned dependency — which is what it always was.
//!
//! ## Zero dependencies, deliberately
//!
//! A plugin needs this at build time without building the editor. A crate that
//! pulled in any editor crate would defeat its own purpose. That constraint is
//! also why [`ABI_FINGERPRINT`] is an FNV-1a rather than a sha2.

use std::io;
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/wit_assets.rs"));

/// Write the embedded package into `dir`, creating it if needed.
///
/// Overwrites whatever is there: the directory is a cache of this package, and
/// a partial refresh — some files current, some stale — resolves into a WIT
/// package that is internally inconsistent, which fails in ways far harder to
/// read than a clean overwrite.
///
/// Files present in `dir` that are **not** part of the package are left alone.
/// A plugin may legitimately keep its own world file beside the package (org
/// does not, but the scaffolds' `world.wit` shape would), and deleting a file
/// this crate did not write would be taking ownership of a directory it only
/// contributes to.
///
/// **A file already holding the right bytes is not rewritten**, and that is
/// load-bearing rather than an optimisation. WT.2b calls this from the plugin
/// build service on every boot, immediately before the source's staleness is
/// judged — and staleness is judged on mtime. An unconditional write would move
/// every file's mtime forward on every boot, so every source would look edited,
/// so every plugin would rebuild from cold on every start. The requirement the
/// build service exists to protect is precisely the opposite one.
pub fn write_to(dir: impl AsRef<Path>) -> io::Result<()> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    for (name, contents) in FILES {
        let path = dir.join(name);
        // `read` rather than metadata+len: a truncated or hand-edited file of
        // coincidentally equal length is exactly the drift this crate exists to
        // repair, and it must not survive the comparison.
        if std::fs::read(&path).is_ok_and(|on_disk| on_disk == contents.as_bytes()) {
            continue;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

/// Names of the files this package writes.
pub fn file_names() -> impl Iterator<Item = &'static str> {
    FILES.iter().map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_package_carries_the_load_bearing_files() {
        assert!(!FILES.is_empty(), "the embedded package is not empty");
        for wanted in ["types.wit", "plugin.wit", "modes.wit", "grammar.wit"] {
            assert!(
                file_names().any(|n| n == wanted),
                "the package carries {wanted}"
            );
        }
    }

    /// The fixture and bundled-plugin worlds stay out. A plugin has no use for
    /// another plugin's world, and shipping the host's test fixtures into every
    /// user's config directory would be noise that also has to be kept current.
    #[test]
    fn fixture_and_bundled_worlds_are_excluded() {
        for unwanted in [
            "auto-pair.wit",
            "init-fixture.wit",
            "multiseam-fixture.wit",
            "trampoline-fixture.wit",
        ] {
            assert!(
                !file_names().any(|n| n == unwanted),
                "{unwanted} is a world no plugin needs"
            );
        }
    }

    #[test]
    fn write_to_produces_every_file_and_creates_the_directory() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-wit-write-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        // The nested path proves `create_dir_all`, not just `create_dir`.
        let target = tmp.join("nested").join("wit");
        write_to(&target).unwrap();
        for name in file_names() {
            let written = std::fs::read_to_string(target.join(name)).unwrap();
            assert!(!written.is_empty(), "{name} written non-empty");
        }
        // A package resolve needs the whole set present at once, so count too.
        assert_eq!(
            std::fs::read_dir(&target).unwrap().count(),
            FILES.len(),
            "every embedded file landed and nothing else did"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A file the package does not own survives a write. The directory is one
    /// this crate contributes to, not one it owns — a plugin may keep its own
    /// world file beside the package.
    #[test]
    fn write_to_leaves_a_file_it_does_not_own_alone() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-wit-keep-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("my-world.wit"), "// mine\n").unwrap();
        write_to(&tmp).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.join("my-world.wit")).unwrap(),
            "// mine\n"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **A second write must not touch a file that already holds the right
    /// bytes.** WT.2b's build service calls `write_to` on every boot just
    /// before judging the source stale by mtime; if the write were
    /// unconditional, every boot would move every mtime forward and every
    /// plugin would rebuild from cold every start — inverting the one
    /// requirement the build cache exists for.
    ///
    /// The sleep is what makes this a test about writing rather than about
    /// timestamp resolution: without a gap the filesystem could report the same
    /// mtime for a rewrite that genuinely happened, and the assertion would
    /// pass on the broken version.
    #[test]
    fn an_identical_file_is_not_rewritten() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-wit-idem-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        write_to(&tmp).unwrap();

        let probe = tmp.join(file_names().next().unwrap());
        let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        let before = mtime(&probe);

        std::thread::sleep(std::time::Duration::from_millis(20));
        write_to(&tmp).unwrap();
        assert_eq!(
            mtime(&probe),
            before,
            "a file already holding the right bytes is left entirely alone"
        );

        // ...and a file that DID drift is repaired, contents and all.
        std::fs::write(&probe, "// someone edited the cache\n").unwrap();
        write_to(&tmp).unwrap();
        let repaired = std::fs::read_to_string(&probe).unwrap();
        assert!(
            !repaired.contains("someone edited"),
            "a drifted file is overwritten from the package"
        );
        assert_ne!(mtime(&probe), before, "and its mtime moves");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The fingerprint is DERIVED FROM the embedded contents.
    ///
    /// Recomputed here with the same FNV-1a `build.rs` uses, so the constant
    /// cannot drift from the files it claims to describe — a constant that was
    /// emitted but not actually hashed over the package would pass every other
    /// test in this file while telling WT.3's staleness check a lie, and the
    /// symptom would be an artifact that never looks stale no matter what the
    /// ABI does.
    #[test]
    fn the_fingerprint_is_derived_from_the_files() {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let fnv = |bytes: &[u8], h: &mut u64| {
            for b in bytes {
                *h ^= u64::from(*b);
                *h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for (name, contents) in FILES {
            fnv(name.as_bytes(), &mut hash);
            fnv(contents.as_bytes(), &mut hash);
        }
        assert_eq!(
            format!("{hash:016x}"),
            ABI_FINGERPRINT,
            "the emitted constant is the hash of the embedded package"
        );
    }

    /// Sorted, so the fingerprint is a function of content and not of the order
    /// the filesystem happened to hand the files back. Without this a rebuild
    /// on another machine could report an ABI change that did not happen.
    #[test]
    fn the_package_is_in_sorted_order() {
        let names: Vec<&str> = file_names().collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    /// The fingerprint is what WT.3 stamps into a built artifact and compares
    /// at load. It has to be stable within a build, and it has to be a function
    /// of the package's CONTENT rather than of readdir order — otherwise every
    /// rebuild would report an ABI change.
    #[test]
    fn the_fingerprint_is_stable_and_content_shaped() {
        assert_eq!(ABI_FINGERPRINT, ABI_FINGERPRINT);
        assert_eq!(ABI_FINGERPRINT.len(), 16, "a 64-bit hash, hex");
        assert!(ABI_FINGERPRINT.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(
            ABI_FINGERPRINT, "0000000000000000",
            "the hash actually ran over the files"
        );
    }
}
