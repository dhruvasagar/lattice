//! AD.4: example coverage only ratchets up.
//!
//! Every function a guest can call or implement has an example in the
//! reference, or is on [`PENDING`] with the reason it has none. The list is
//! shrink-only: a function that gains an example must come off it (the test
//! fails until it does), and a new WIT function without an example fails
//! until someone either writes one or adds it here with a reason — which
//! makes the gap a decision instead of an oversight.
//!
//! The common reason is worth knowing on its own: "no guest calls it" means
//! the function has no guest-side exercise anywhere in the tree — no fixture,
//! no plugin. That is untested API surface, and this list is its inventory.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lattice_plugin_api::catalog;
use lattice_plugin_api::examples::scan;

/// Functions with no example yet, and why. `<seam>.<function>` in the
/// reference's spelling (methods as `<resource>.<method>`).
const PENDING: &[(&str, &str)] = &[
    // --- No guest calls it: untested from the guest side ------------------
    ("buffer.document.byte-len", NO_GUEST),
    ("buffer.document.line-count", NO_GUEST),
    ("config.option-diagnostic", NO_GUEST),
    ("config.set-option-in-buffer", NO_GUEST),
    ("host-services.read-file", NO_GUEST),
    ("host-services.refresh-decorations", NO_GUEST),
    ("host-services.source-line", NO_GUEST),
    ("host-services.store-delete", NO_GUEST),
    ("host-services.view-args", NO_GUEST),
    ("modes.disable-mode", NO_GUEST),
    ("multibuffer-view-registry.refresh-view", NO_GUEST),
    ("theme.set-element-override", NO_GUEST),
    (
        "tree-sitter.node.child-by-field",
        "no guest calls it (treesitter-context moved to `run-query-ranges` in TC.10)",
    ),
    ("tree-sitter.node.is-error", NO_GUEST),
    ("tree-sitter.node.is-named", NO_GUEST),
    ("tree-sitter.node.next-named-sibling", NO_GUEST),
    ("tree-sitter.node.parent", NO_GUEST),
    ("tree-sitter.node.prev-named-sibling", NO_GUEST),
    ("tree-sitter.tree-cursor.current-field", NO_GUEST),
    ("tree-sitter.tree-cursor.goto-next-named-sibling", NO_GUEST),
    ("tree-sitter.tree-cursor.goto-parent", NO_GUEST),
    ("tree-sitter.tree-cursor.reset", NO_GUEST),
    ("tree-sitter.tree-snapshot.node-at", NO_GUEST),
    ("ui.clear-segment", NO_GUEST),
    // --- Called only inside another target's example ---------------------
    // Visible to a reader in that example; no non-overlapping span exists to
    // file it under its own name.
    (
        "tree-sitter.node.named-child-count",
        "shown in `context-guest:tree-sitter.node.named-child`",
    ),
    (
        "tree-sitter.node.walk",
        "shown in `multiseam-guest:tree-sitter.tree-cursor.goto-first-named-child`",
    ),
    (
        "tree-sitter.tree-cursor.current-node",
        "shown in `multiseam-guest:tree-sitter.tree-cursor.goto-first-named-child`",
    ),
    (
        "tree-sitter.tree-snapshot.root",
        "shown in `multiseam-guest:tree-sitter.parse-file` and `…:tree-sitter.node.byte-range`",
    ),
];

/// No fixture and no plugin calls it, so there is no compiled, exercised code
/// to quote. Writing an example means writing the guest-side test first.
const NO_GUEST: &str = "no guest calls it";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every function of every seam a guest calls or implements.
fn guest_facing_functions() -> BTreeSet<String> {
    catalog()
        .interfaces
        .iter()
        .flat_map(|i| {
            i.functions
                .iter()
                .map(move |f| format!("{}.{}", i.name, f.display_name()))
        })
        .collect()
}

fn with_examples() -> BTreeSet<String> {
    scan(&repo_root())
        .items
        .into_iter()
        .map(|e| e.target)
        .collect()
}

#[test]
fn every_function_has_an_example_or_a_reason() {
    let pending: BTreeSet<&str> = PENDING.iter().map(|(t, _)| *t).collect();
    let covered = with_examples();
    let missing: Vec<String> = guest_facing_functions()
        .into_iter()
        .filter(|f| !covered.contains(f) && !pending.contains(f.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "functions with no example in the plugin-API reference:\n  {}\n\n\
         Add a `// @example <target>: <caption>` region to a guest that uses it \
         (see crates/lattice-plugin-api/src/examples.rs), or — if no guest can \
         show it yet — add it to PENDING in this file with the reason.",
        missing.join("\n  ")
    );
}

#[test]
fn pending_only_shrinks() {
    let covered = with_examples();
    let functions = guest_facing_functions();
    let mut stale = Vec::new();
    for (target, reason) in PENDING {
        assert!(
            !reason.trim().is_empty(),
            "`{target}` is PENDING with no reason"
        );
        if covered.contains(*target) {
            stale.push(format!(
                "{target}: has an example now — remove it from PENDING"
            ));
        } else if !functions.contains(*target) {
            stale.push(format!("{target}: no such function (renamed or removed?)"));
        }
    }
    assert!(
        stale.is_empty(),
        "PENDING is out of date:\n  {}",
        stale.join("\n  ")
    );
}

/// Every seam a guest touches has at least one example — no exceptions, no
/// PENDING. A seam page with nothing to copy is the gap this series exists to
/// close.
#[test]
fn every_guest_facing_seam_has_an_example() {
    let covered = with_examples();
    let bare: Vec<&str> = catalog()
        .interfaces
        .iter()
        .filter(|i| !i.functions.is_empty())
        .filter(|i| {
            !covered
                .iter()
                .any(|t| t == &i.name || t.starts_with(&format!("{}.", i.name)))
        })
        .map(|i| i.name.as_str())
        .collect();
    assert!(bare.is_empty(), "seams with no example at all: {bare:?}");
}
