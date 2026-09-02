//! OA.4b — `<Tab>` is declared once, and every view that folds gets it.
//!
//! `foldable-view-mode` binds `<Tab>` / `<S-Tab>` and nothing else: it
//! resolves `<Tab>` to whichever active mode declared a `fold_toggle_action()`
//! and dispatches that. Two ways to get that wrong, and this file guards both,
//! because the bug it replaced was a *gap* rather than a breakage.
//!
//! Before this slice, `magit-nav-mode` bound both chords and `org-agenda-mode`
//! had grown an independent copy, while project search, the LSP references
//! view, `*problems*` and `*compilation*` had **neither** — four foldable
//! grouped views with no way to collapse a block. Nobody noticed, because a
//! gap in a copied set does not announce itself. That is `refreshable-view-mode`'s
//! own history, one chord later, which is why these guards mirror
//! `refreshable_views_declare_their_refresh.rs` exactly.
//!
//! The enumeration comes from the booted mode registry rather than from a
//! grep, for that file's reason: it is the only place every mode-owning crate
//! has registered, and grepping source sweeps in test fixtures until the guard
//! gets weakened into uselessness.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::FoldableViewMode;

/// The only file allowed to bind the chords in a Normal-mode view.
///
/// The Insert-mode `<Tab>` bindings (completion, snippets, the command line)
/// are a different chord in a different mode and are not constrained; the
/// scan below only trips on files that name the chord at all, so they are
/// listed here to keep the guard honest rather than weakened.
const ALLOWED: &[&str] = &[
    "crates/lattice-mode/src/foldable_view_mode.rs",
    // The BUILTIN layer, where `<Tab>` is the terminal alias for `<C-i>`:
    // jump-list forward, in every ordinary document. That binding is what
    // `foldable-view-mode` deliberately shadows in the views that opt in, and
    // it is why the shared minor is `ActivationPolicy::Manual` — the two must
    // coexist, not compete.
    "crates/lattice-keymap/src/keymap_entry.rs",
    // Insert-mode surfaces: completion / snippet / command-line `<Tab>`.
    "crates/lattice-snippet/src/modes.rs",
    "crates/lattice-host/src/command_line_mode.rs",
    "crates/lattice-host/src/command_line_expand_mode.rs",
    // TB.1 — `table-mode`, and the ONLY entry here that is a Normal-mode
    // `<Tab>` outside the shared minor. It is allowed because it is not a
    // fold: inside a pipe table `<Tab>` advances a cell, which is a different
    // meaning rather than a fourth copy of the same one, and it is the
    // meaning users arrive with from every other editor that has tables.
    //
    // What makes it safe is that it DECLINES. Outside a table the body
    // returns `Effect::Declined`, the dispatcher peels this layer, and the
    // chord resolves to whatever `<Tab>` already meant — org's headline
    // cycle, then the builtin jump-forward. So the chain this guard protects
    // is intact; `table-mode` sits above it rather than replacing it, and
    // `table_mode_layering.rs` pins exactly that.
    //
    // It is deliberately NOT expressed as `fold_toggle_action()`. That would
    // make the shared minor dispatch a cell-walk as if it were a fold, which
    // is the vocabulary being wrong to satisfy a lint.
    "crates/lattice-mode/src/modes/table/mode.rs",
];

fn workspace_root() -> PathBuf {
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
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A mode that joins the cascade without declaring a target gets a `<Tab>`
/// that resolves to nothing — a key the user presses, the help lists, and the
/// mode's comments describe, which silently does nothing.
#[test]
fn every_mode_that_inherits_tab_declares_what_it_folds() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let registry = editor.mode_registry.load();
    let foldable = FoldableViewMode::mode_id();

    let mut silent: Vec<String> = registry
        .iter()
        .filter(|(_, mode)| mode.implies().contains(&foldable))
        .filter(|(_, mode)| mode.fold_toggle_action().is_none())
        .map(|(id, _)| id.to_string())
        .collect();
    silent.sort();

    assert!(
        silent.is_empty(),
        "these modes pull in `foldable-view-mode` (so `<Tab>` is bound in \
         their buffers) but declare no `fold_toggle_action()`, leaving \
         `<Tab>` resolving to nothing: {silent:?}"
    );
}

/// Nobody binds the chords themselves.
///
/// A source property, not a behavioural one, for `gr_is_declared_once.rs`'s
/// reason: the bug this replaced was not a broken chord — every copy worked.
/// It was that copies existed at all, and the views that landed most recently
/// had none. A behavioural test passes on all three copies and passes just as
/// happily on the four views that were missing one. A fourth copy must be a
/// failing test, not a regression found months later.
#[test]
fn tab_is_bound_in_exactly_the_one_allowed_place() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);
    assert!(!files.is_empty(), "found no sources — walker is broken");

    let mut offenders: Vec<String> = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !text.contains(r#"chord: "<Tab>""#) && !text.contains(r#"chord: "<S-Tab>""#) {
            continue;
        }
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.contains(&rel.as_str()) || rel.contains("/tests/") {
            continue;
        }
        offenders.push(rel);
    }
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "`<Tab>` / `<S-Tab>` in a Normal-mode view are `foldable-view-mode`'s.\n\
         These files bind them directly: {offenders:?}\n\n\
         A foldable view does NOT bind the chord. It declares\n\
         `fn fold_toggle_action(&self) -> Option<&'static str>` naming its own\n\
         action (or `FOLD_TOGGLE_DEFAULT_ACTION`), and the shared minor\n\
         arrives through the implies cascade."
    );
}

/// The five views this slice was for. Named individually rather than counted,
/// so dropping one is a failure with a name in it rather than an off-by-one.
#[test]
fn the_grouped_views_all_fold() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let registry = editor.mode_registry.load();

    for id in [
        "scan-view-mode",
        "project-search-mode",
        "problems-mode",
        "lsp-references-mode",
        "compilation-mode",
        "magit-core-mode",
    ] {
        let mode_id = lattice_mode::ModeId::new(id);
        let Some(mode) = registry.get(mode_id) else {
            // A crate may be absent from a trimmed build; skip rather than
            // fail, the way the registry-driven guards above do.
            continue;
        };
        assert!(
            mode.fold_toggle_action().is_some(),
            "`{id}` is a grouped, foldable view and must declare \
             `fold_toggle_action()` so `<Tab>` folds a block in it"
        );
    }
}
