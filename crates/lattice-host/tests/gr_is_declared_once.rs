//! RV.2 (2026-08-10): `gr` is bound in exactly two places, and this
//! test is the thing that keeps it that way.
//!
//! Design: `docs/dev/architecture/mode-architecture.md` §5.5.
//!
//! ## Why a source-grep test
//!
//! The bug RV.1/RV.2 fixed was not a broken chord — every copy of `gr`
//! worked. It was that **five** modes had each declared their own, and
//! the two views that landed most recently had none, and nobody
//! noticed for months. A behavioural test cannot catch that: it would
//! have passed on all five copies, and passed just as happily on the
//! two views that were missing one.
//!
//! What actually needs pinning is a *source property* — "this chord is
//! declared once" — so that is what this asserts. A sixth copy is a
//! failing test, not a silent regression discovered later.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// The only two files allowed to bind `gr`.
///
/// - `refreshable-view-mode` owns it for every synthetic view.
/// - `lattice-lsp`'s nav mode owns it in ordinary source buffers,
///   where `gr` means **find references**. The two never collide: the
///   shared minor is `ActivationPolicy::Manual` and only arrives via
///   the implies cascade, so it is never active on a document.
const ALLOWED: &[&str] = &[
    "crates/lattice-mode/src/refreshable_view_mode.rs",
    "crates/lattice-lsp/src/modes.rs",
];

fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/crates/lattice-host`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is two levels below the workspace root")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip build artefacts; `target` can be enormous.
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn gr_is_bound_in_exactly_the_two_allowed_places() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);
    assert!(!files.is_empty(), "found no sources — walker is broken");

    let mut offenders: Vec<String> = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !text.contains(r#"chord: "gr""#) {
            continue;
        }
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        // Test modules legitimately construct `gr` bindings to exercise
        // dispatch; only production declarations are constrained.
        if ALLOWED.contains(&rel.as_str()) || rel.contains("/tests/") {
            continue;
        }
        offenders.push(rel);
    }
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "`gr` must be declared once, on `refreshable-view-mode`.\n\
         These files bind it directly: {offenders:?}\n\n\
         A synthetic view does NOT bind `gr`. It declares\n\
         `fn refresh_action(&self) -> Option<&'static str>` naming its own\n\
         action, and the shared minor arrives through the implies cascade.\n\
         See docs/dev/architecture/mode-architecture.md §5.5."
    );
}
