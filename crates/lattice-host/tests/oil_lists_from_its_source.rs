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
use lattice_host::editor::Editor;
use lattice_listing::oil::{OilEntry, OilSource};

/// A tree held in memory, at paths that do not exist on disk — so a
/// listing that came from the filesystem instead would fail, not pass.
#[derive(Debug)]
struct MemSource(HashMap<PathBuf, Vec<OilEntry>>);

impl OilSource for MemSource {
    fn id(&self) -> &str {
        "mem"
    }
    fn list(&self, dir: &Path) -> std::io::Result<Vec<OilEntry>> {
        self.0
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

fn mem_tree(root: &Path) -> Arc<MemSource> {
    Arc::new(MemSource(HashMap::from([
        (
            root.to_path_buf(),
            vec![entry("sub", true), entry("top.txt", false)],
        ),
        (root.join("sub"), vec![entry("inner.rs", false)]),
    ])))
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
