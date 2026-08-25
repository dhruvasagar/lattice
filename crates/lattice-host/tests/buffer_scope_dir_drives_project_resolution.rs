//! PR.6 — a buffer that is not a file still belongs to a project.
//!
//! `:files` and `:search` resolve their root from the active buffer. Both went
//! through one branch on the buffer's **path**, and a synthetic buffer —
//! magit's status, oil, a file tree, a search or agenda view — is
//! `Document::empty()` and has none. So both fell to "resolve the empty path",
//! which is the process working directory, and `:files` in a magit buffer for
//! one checkout listed whichever tree the editor was launched in.
//!
//! The failure was silent, which is what makes it worth a test rather than a
//! fix: a picker full of the wrong project's files looks exactly like a picker
//! that worked.
//!
//! `BufferScopeDir` is the buffer's own answer, consulted first. These tests
//! pin the precedence and the two ways a provider records it.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

use lattice_core::{BufferFlags, Document as CoreDocument};
use lattice_host::editor::Editor;
use lattice_mode::{BufferScopeSource, BufferScopeSourceRegistry, BufferScopeSourceRegistryHandle};

/// Two independent project roots, each a `.git` so the resolver recognises
/// them, plus a file in each.
fn two_projects(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("lattice-scope-{tag}-{}", std::process::id()));
    let a = base.join("alpha");
    let b = base.join("beta");
    for p in [&a, &b] {
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join("f.rs"), "fn main() {}\n").unwrap();
    }
    (a, b)
}

fn boot() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn synthetic(editor: &mut Editor, name: &str) -> lattice_core::BufferId {
    let id = editor.ensure_named_synthetic_document(
        name,
        lattice_mode::ModeId::new("text-mode"),
        BufferFlags::default(),
    );
    let _ = editor.activate_buffer(id);
    id
}

/// The root `:files` would walk, through the same call the picker makes.
fn picker_root(editor: &Editor) -> PathBuf {
    let snap = editor.document.snapshot();
    editor.picker_workspace_root_path(&snap)
}

/// The headline regression. Without the scope dir this returns the process
/// cwd's project; with it, the project the buffer is about.
#[test]
fn a_pathless_buffer_resolves_to_the_directory_it_is_about() {
    let (alpha, _beta) = two_projects("headline");
    let mut editor = boot();
    let buf = synthetic(&mut editor, "*magit: alpha*");

    // Before: nothing recorded, so the answer is whatever the editor was
    // launched in — demonstrably NOT alpha. This is the bug, asserted.
    assert_ne!(
        picker_root(&editor).canonicalize().ok(),
        alpha.canonicalize().ok(),
        "a pathless buffer with no recorded scope cannot know its project"
    );

    editor.set_buffer_scope_dir(buf, alpha.clone());

    assert_eq!(
        picker_root(&editor).canonicalize().unwrap(),
        alpha.canonicalize().unwrap(),
        "`:files` now walks the project the buffer is about"
    );
    assert_eq!(
        editor.active_buffer_project().root.canonicalize().unwrap(),
        alpha.canonicalize().unwrap(),
        "`:search` must agree — two answers from one buffer would be worse \
         than either alone"
    );
}

/// A *subdirectory* records fine: the writer says where the buffer is, the
/// host resolves the project. An oil buffer on `alpha/src` still lists
/// `alpha`, so a provider never has to know what a project is.
#[test]
fn a_scope_dir_resolves_up_to_its_project_root() {
    let (alpha, _beta) = two_projects("subdir");
    let src = alpha.join("src");
    std::fs::create_dir_all(&src).unwrap();

    let mut editor = boot();
    let buf = synthetic(&mut editor, "*oil: src*");
    editor.set_buffer_scope_dir(buf, src);

    assert_eq!(
        picker_root(&editor).canonicalize().unwrap(),
        alpha.canonicalize().unwrap(),
        "the provider recorded a directory; the host resolved the project"
    );
}

/// Two buffers, two projects, one editor — the case a single global "current
/// project" cannot express, and the reason this is per-buffer.
#[test]
fn two_buffers_can_be_about_two_different_projects() {
    let (alpha, beta) = two_projects("two");
    let mut editor = boot();

    let a = synthetic(&mut editor, "*magit: alpha*");
    editor.set_buffer_scope_dir(a, alpha.clone());
    let b = synthetic(&mut editor, "*magit: beta*");
    editor.set_buffer_scope_dir(b, beta.clone());

    // `b` is active (created last).
    assert_eq!(
        picker_root(&editor).canonicalize().unwrap(),
        beta.canonicalize().unwrap()
    );
    let _ = editor.activate_buffer(a);
    assert_eq!(
        picker_root(&editor).canonicalize().unwrap(),
        alpha.canonicalize().unwrap(),
        "switching buffers switches the project the pickers answer for"
    );
}

/// A real file's own path still wins where there is no scope dir — the fix
/// adds a branch ahead of the existing ones rather than replacing them.
#[test]
fn a_real_files_own_path_still_decides_when_no_scope_is_recorded() {
    let (alpha, _beta) = two_projects("realfile");
    let mut editor = boot();
    editor.do_edit(Some(alpha.join("f.rs")), false);

    assert_eq!(
        picker_root(&editor).canonicalize().unwrap(),
        alpha.canonicalize().unwrap()
    );
}

// ─────────────────────────────────────────────────────────────────
// The by-name pull, which is how magit's buffers are covered
// ─────────────────────────────────────────────────────────────────

/// Stands in for `RepoScopes`: knows a name → directory mapping recorded
/// before the buffer existed.
#[derive(Debug)]
struct NameSource(Vec<(String, PathBuf)>);

impl BufferScopeSource for NameSource {
    fn scope_dir_for_name(&self, buffer_name: &str) -> Option<PathBuf> {
        self.0
            .iter()
            .find(|(n, _)| n == buffer_name)
            .map(|(_, d)| d.clone())
    }
}

fn register_source(editor: &Editor, source: NameSource) {
    let reg = editor
        .services
        .get::<BufferScopeSourceRegistryHandle>()
        .expect("the registry is a boot service");
    let source: std::sync::Arc<dyn BufferScopeSource> = std::sync::Arc::new(source);
    reg.rcu(|current| {
        let mut next = (**current).clone();
        next.register(source.clone());
        std::sync::Arc::new(next)
    });
}

/// The path magit actually takes: it never touches the buffer, it returns a
/// name in `Effect::OpenSyntheticBuffer` and the host creates it. So the host
/// asks by name at creation, and the buffer is scoped before anyone reads it.
#[test]
fn a_provider_that_only_knows_the_name_still_scopes_its_buffer() {
    let (alpha, _beta) = two_projects("byname");
    let mut editor = boot();
    register_source(
        &editor,
        NameSource(vec![("*magit: alpha*".to_string(), alpha.clone())]),
    );

    // No explicit `set_buffer_scope_dir` anywhere — creation does it.
    synthetic(&mut editor, "*magit: alpha*");

    assert_eq!(
        picker_root(&editor).canonicalize().unwrap(),
        alpha.canonicalize().unwrap(),
        "the host asked at creation and the provider answered"
    );
}

/// A name no source recognises is not an error — most buffers have no scope,
/// and every source is asked about every name.
#[test]
fn an_unrecognised_name_records_nothing_and_does_not_fail() {
    let (alpha, _beta) = two_projects("unknown");
    let mut editor = boot();
    register_source(
        &editor,
        NameSource(vec![("*magit: alpha*".to_string(), alpha)]),
    );

    let id = synthetic(&mut editor, "*messages*");
    assert!(
        editor.buffer_scope_dir(id).is_none(),
        "no source claimed this name, and that is a normal outcome"
    );
}

/// First-answer-wins across sources, and a second registration does not
/// displace the first — a single-slot service would have let the second
/// provider silently win.
#[test]
fn sources_accumulate_rather_than_replace() {
    let (alpha, beta) = two_projects("accum");
    let mut reg = BufferScopeSourceRegistry::new();
    reg.register(std::sync::Arc::new(NameSource(vec![(
        "*a*".to_string(),
        alpha.clone(),
    )])));
    reg.register(std::sync::Arc::new(NameSource(vec![(
        "*b*".to_string(),
        beta.clone(),
    )])));

    assert_eq!(reg.len(), 2);
    assert_eq!(reg.scope_dir_for_name("*a*"), Some(alpha));
    assert_eq!(
        reg.scope_dir_for_name("*b*"),
        Some(beta),
        "the second registration is still reachable"
    );
    assert_eq!(reg.scope_dir_for_name("*c*"), None);
}

/// The oil / file-tree chokepoints write the generic local as a side effect,
/// so their existing callers need no change and the two cannot drift.
#[test]
fn the_oil_and_file_tree_chokepoints_record_the_scope_too() {
    let (alpha, beta) = two_projects("chokepoints");
    let mut editor = boot();

    let oil = synthetic(&mut editor, "*oil*");
    editor.set_oil_dir(oil, alpha.clone());
    assert_eq!(
        editor.buffer_scope_dir(oil).as_deref(),
        Some(alpha.as_path())
    );

    let tree = synthetic(&mut editor, "*file-tree*");
    editor.set_file_tree_root(tree, beta.clone());
    assert_eq!(
        editor.buffer_scope_dir(tree).as_deref(),
        Some(beta.as_path())
    );
}

/// A scope dir pointing somewhere that no longer exists must not panic or
/// hang — it degrades to whatever the resolver makes of it.
#[test]
fn a_vanished_scope_dir_degrades_rather_than_panicking() {
    let mut editor = boot();
    let buf = synthetic(&mut editor, "*gone*");
    editor.set_buffer_scope_dir(
        buf,
        Path::new("/nonexistent-lattice-scope-xyz").to_path_buf(),
    );
    let _ = picker_root(&editor);
    let _ = editor.active_buffer_project();
}
