//! PP.2 — a picker whose results are scoped to a project says which project.
//!
//! Design:
//! [`docs/dev/architecture/picker.md`](../../../docs/dev/architecture/picker.md).
//!
//! ## What is actually at risk
//!
//! Not "does `files` show a root" — that is one field read at seat time. The
//! risk is the pickers that must NOT show one. The host resolves a root for
//! every picker open (`build_picker_context` always fills `workspace_root`),
//! so a wiring that ignored the declaration would put a path on `buffers`,
//! `commands` and `marks` — lists that span every project you have open, on
//! the one line the user reads to know what they are looking at. That failure
//! is invisible in a `files` test and obvious in a `buffers` one, so both are
//! here.
//!
//! The second risk is the value itself. A root that is *present but wrong* is
//! worse than none: the whole reason to show it is that memory is unreliable
//! when several checkouts are open, and a prompt naming the wrong one actively
//! misleads. So the assertions are on the resolved path, never on "is Some".

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("committed\n"))
}

fn root_label(editor: &Editor) -> Option<String> {
    editor.picker.as_ref().and_then(|p| p.root_label.clone())
}

/// A project with a root marker, so the resolver has something to find.
fn project(name: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(format!("{name}.rs")), "fn main() {}\n").unwrap();
    dir
}

/// The headline: `files` names the project it walked.
///
/// Asserted against the ROOT that was passed, not merely against `Some` — a
/// prompt naming the wrong checkout is worse than one naming none, because the
/// whole point is to answer a question the user cannot answer from memory.
#[test]
fn a_rooted_picker_shows_the_root_it_walked() {
    let dir = project("alpha");
    let root = dir.path().canonicalize().unwrap();
    let mut editor = boot();

    let _ =
        editor.open_picker_for_effect("files".to_string(), Vec::new(), Some(root.clone()), None);
    assert!(editor.picker.is_some(), "precondition: `files` seated");

    let shown = root_label(&editor).expect("`files` declares itself rooted");
    assert_eq!(
        shown,
        lattice_core::home::contract_tilde(&root),
        "the prompt names the root the walk actually used"
    );
}

/// Two checkouts, two prompts. This is the case the feature exists for — the
/// rows look identical and only the prompt can tell them apart.
#[test]
fn two_projects_give_two_different_prompts() {
    let alpha = project("alpha");
    let beta = project("beta");
    let mut editor = boot();

    let _ = editor.open_picker_for_effect(
        "files".to_string(),
        Vec::new(),
        Some(alpha.path().canonicalize().unwrap()),
        None,
    );
    let first = root_label(&editor).expect("rooted");

    let _ = editor.open_picker_for_effect(
        "files".to_string(),
        Vec::new(),
        Some(beta.path().canonicalize().unwrap()),
        None,
    );
    let second = root_label(&editor).expect("rooted");

    assert_ne!(
        first, second,
        "a second open must re-resolve; a cached label would name the project \
         you left"
    );
}

/// **The one that could quietly spoil every other picker.** `rooted` defaults
/// to `false`, and a wiring that ignored the default would name a root on
/// lists that span every open project — which is exactly the noise that makes
/// a prompt stop being read.
#[test]
fn a_picker_that_is_not_root_scoped_names_nothing() {
    let mut editor = boot();

    for source in ["buffers", "commands", "marks", "registers"] {
        let _ = editor.open_picker(source.to_string(), Vec::new());
        assert!(
            editor.picker.is_some(),
            "precondition: `{source}` seated a picker"
        );
        assert_eq!(
            root_label(&editor),
            None,
            "`{source}` spans every open project — naming one would say \
             something untrue about its rows"
        );
    }
}

/// `dir-pick` declines it despite being the most path-shaped source there is.
/// Its QUERY is the directory it is listing, so a root beside that would be a
/// second answer to the same question — and a staler one, naming where
/// browsing started rather than where you are now.
#[test]
fn dir_pick_leaves_the_prompt_to_its_query() {
    let dir = project("gamma");
    let root = dir.path().canonicalize().unwrap();
    let mut editor = boot();

    let _ = editor.open_picker(
        lattice_picker::DIR_PICK_SOURCE.to_string(),
        vec![root.to_string_lossy().to_string()],
    );

    assert_eq!(root_label(&editor), None, "no root label…");
    assert_eq!(
        editor.picker.as_ref().map(|p| p.query.clone()),
        Some(format!("{}/", root.to_string_lossy())),
        "…because the query is already the answer"
    );
}

/// The label is for READING: home-contracted, because the home prefix is the
/// least informative part of a path and the part that squeezes out the rest of
/// it on a one-line surface.
#[test]
fn the_label_is_contracted_for_display() {
    let Some(home) = dirs::home_dir() else {
        eprintln!("SKIP: no home directory on this machine");
        return;
    };
    let dir = tempfile::TempDir::new_in(&home).unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = boot();

    let _ = editor.open_picker_for_effect("files".to_string(), Vec::new(), Some(root), None);

    let shown = root_label(&editor).expect("rooted");
    assert!(
        shown.starts_with('~'),
        "a path under home reads with a tilde: {shown}"
    );
}
