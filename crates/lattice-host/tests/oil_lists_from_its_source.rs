//! OR.1: an oil buffer lists, re-lists and names itself from the source
//! it was opened with, and a listing from one source is never mistaken
//! for another's listing of the same directory.
//!
//! The filesystem is one source among others (a git revision's tree is
//! the second); these pin that oil asks the buffer's source rather than
//! the disk.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::OpenTarget;
use lattice_grammar::Effect;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;
use lattice_listing::oil::{OilEntry, OilSource};

/// A tree held in memory, at paths that do not exist on disk — so a
/// listing that came from the filesystem instead would fail, not pass.
#[derive(Debug)]
struct MemSource {
    tree: HashMap<PathBuf, Vec<OilEntry>>,
    read_only: bool,
}

impl OilSource for MemSource {
    fn id(&self) -> &str {
        "mem"
    }
    fn read_only(&self) -> Option<&str> {
        self.read_only.then_some("a test tree is read-only")
    }
    fn list(&self, dir: &Path) -> std::io::Result<Vec<OilEntry>> {
        self.tree
            .get(dir)
            .cloned()
            .ok_or_else(|| std::io::Error::other("not in the tree"))
    }
    fn display(&self, dir: &Path) -> String {
        format!("mem:{}", dir.display())
    }
    fn open_entry(&self, _path: &Path, _is_dir: bool, _target: Option<OpenTarget>) -> Effect {
        Effect::None
    }
}

fn entry(name: &str, is_dir: bool) -> OilEntry {
    OilEntry {
        name: name.to_string(),
        is_dir,
    }
}

fn tree(root: &Path) -> HashMap<PathBuf, Vec<OilEntry>> {
    HashMap::from([
        (
            root.to_path_buf(),
            vec![entry("sub", true), entry("top.txt", false)],
        ),
        (root.join("sub"), vec![entry("inner.rs", false)]),
    ])
}

fn mem_tree(root: &Path) -> Arc<MemSource> {
    Arc::new(MemSource {
        tree: tree(root),
        read_only: false,
    })
}

fn read_only_tree(root: &Path) -> Arc<MemSource> {
    Arc::new(MemSource {
        tree: tree(root),
        read_only: true,
    })
}

fn press(editor: &mut Editor, keys: &str) {
    let mut partial = Vec::new();
    for c in keys.chars() {
        let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
}

fn echo(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

fn boot() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn listing_text(editor: &Editor) -> String {
    editor.document.snapshot().buffer.as_string()
}

#[test]
fn a_listing_shows_and_names_what_its_source_lists() {
    let root = PathBuf::from("/nowhere/at/all");
    let mut editor = boot();
    editor.open_oil_listing(mem_tree(&root), root.clone(), None);

    assert_eq!(listing_text(&editor), "sub\ntop.txt");
    let view = editor.active_pane_buffer_id();
    assert_eq!(editor.oil_source_for(view).id(), "mem");
    assert_eq!(editor.oil_dir_for(view), Some(root));
}

#[test]
fn descending_re_lists_from_the_same_source() {
    let root = PathBuf::from("/nowhere/at/all");
    let mut editor = boot();
    editor.open_oil_listing(mem_tree(&root), root.clone(), None);
    let view = editor.active_pane_buffer_id();

    editor.apply_oil_navigate(view, root.join("sub"), None);

    assert_eq!(listing_text(&editor), "inner.rs");
    assert_eq!(editor.oil_source_for(view).id(), "mem");
}

#[test]
fn a_directory_open_from_two_sources_is_two_listings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("on-disk.txt"), "").unwrap();
    let root = dir.path().to_path_buf();
    let mut editor = boot();

    editor.open_oil_listing(mem_tree(&root), root.clone(), None);
    let from_mem = editor.active_pane_buffer_id();
    editor.do_open_oil(Some(root.clone()));
    let from_disk = editor.active_pane_buffer_id();

    assert_ne!(from_mem, from_disk, "the disk listing reused the mem one");
    assert_eq!(listing_text(&editor), "on-disk.txt");
    // And each is found again by its own source.
    assert_eq!(editor.oil_listing("mem", &root), Some(from_mem));
    assert_eq!(editor.oil_with_dir(&root), Some(from_disk));
}

#[test]
fn a_source_that_cannot_list_opens_nothing() {
    let mut editor = boot();
    let before = editor.active_pane_buffer_id();
    editor.open_oil_listing(
        mem_tree(Path::new("/elsewhere")),
        PathBuf::from("/not/in/it"),
        None,
    );
    assert_eq!(editor.active_pane_buffer_id(), before);
}

/// OR.2. `x` is the control: it must edit a writable listing, or the
/// read-only assertion below would pass against a key that never reached
/// the buffer. (`x` rather than `dd` for the reason
/// `read_only_mode_gates_edits` gives: the operator path is the one the
/// mode's runner stops, and `x` is its single-chord form.)
#[test]
fn a_read_only_listing_refuses_edits_and_says_why() {
    let root = PathBuf::from("/nowhere/at/all");
    let mut editor = boot();
    editor.open_oil_listing(mem_tree(&root), root.clone(), None);
    press(&mut editor, "x");
    assert_eq!(
        listing_text(&editor),
        "ub\ntop.txt",
        "`x` must edit a writable listing, or this test proves nothing"
    );

    let mut editor = boot();
    editor.open_oil_listing(read_only_tree(&root), root.clone(), None);
    press(&mut editor, "x");
    assert_eq!(listing_text(&editor), "sub\ntop.txt");
    assert!(
        echo(&editor).contains("read-only"),
        "the refusal must be explained; echo was {:?}",
        echo(&editor)
    );
    // Moving and yanking are not edits.
    press(&mut editor, "j");
    assert_eq!(editor.cursor.line, 1);
}

/// `:w` from a read-only listing never reaches the disk — here, a real
/// directory that a write would change, so a write that slipped through
/// would be seen.
#[test]
fn a_read_only_listing_refuses_to_write() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "").unwrap();
    let root = dir.path().to_path_buf();
    let mut editor = boot();
    editor.open_oil_listing(read_only_tree(&root), root.clone(), None);

    editor.do_write(None);

    assert_eq!(echo(&editor), "oil: a test tree is read-only");
    assert!(
        root.join("keep.txt").exists(),
        "a refused write must not touch the directory"
    );
    assert!(!root.join("top.txt").exists());
}
