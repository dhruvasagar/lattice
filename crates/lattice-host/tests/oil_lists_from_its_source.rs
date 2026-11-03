//! LS.1: an oil buffer lists, re-lists and names itself from the source
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
use lattice_listing::oil::{
    OilEntry, OilOrigin, OilSource, OilSourceProvider, OilSourceRegistryHandle,
};

/// A tree held in memory, at paths that do not exist on disk — so a
/// listing that came from the filesystem instead would fail, not pass.
#[derive(Debug)]
struct MemSource {
    tree: HashMap<PathBuf, Vec<OilEntry>>,
    read_only: bool,
}

/// The tree's root, where `-` stops.
const TOP: &str = "/nowhere/at/all";

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
    fn parent(&self, dir: &Path) -> Option<PathBuf> {
        (dir != Path::new(TOP)).then(|| dir.parent().map(Path::to_path_buf))?
    }
    /// A file opens as a synthetic buffer named for it, in `target` when
    /// one is given — the shape a source whose files are not on disk has.
    fn open_entry(&self, path: &Path, _is_dir: bool, target: Option<OpenTarget>) -> Effect {
        let open = Effect::OpenSyntheticBuffer {
            name: mem_buffer_name(path),
            mode_id: "plugin-output-mode".to_string(),
            content: Some(format!("{} from mem\n", path.display())),
            cursor: None,
            activate_minor: None,
        };
        match target {
            None => open,
            Some(target) => Effect::InTarget {
                target,
                effect: Box::new(open),
            },
        }
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

/// Apply the minor modes a buffer's major queued for it — `oil-global-mode`,
/// which owns `-`, is a Universal minor. In the app the tail of the keystroke
/// that opened the buffer drains this queue (`run_tick_pending`); a bare
/// `Editor` has no tail, so the test runs that step itself. It is a queue
/// already filled when the buffer opened, not an async result being waited
/// on.
fn activate_queued_minors(editor: &mut Editor) {
    let _ = editor.run_tick_pending();
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

/// LS.2. `x` is the control: it must edit a writable listing, or the
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

// ─── LS.4: `-` from a buffer a provider made, and `<C-v>` on an entry ──

fn mem_buffer_name(path: &Path) -> String {
    format!("*mem:{}*", path.display())
}

/// Claims the `*mem:<path>*` buffers: `-` from one lists `<path>`'s
/// directory from the mem tree, on `<path>`.
#[derive(Debug)]
struct MemProvider;

impl OilSourceProvider for MemProvider {
    fn origin_for(&self, buffer_name: &str) -> Option<OilOrigin> {
        let path = Path::new(buffer_name.strip_prefix("*mem:")?.strip_suffix('*')?);
        Some(OilOrigin {
            source: "mem".into(),
            dir: path.parent()?.to_path_buf(),
            focus: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        })
    }
    fn source(&self, id: &str) -> Option<Arc<dyn OilSource>> {
        (id == "mem").then(|| mem_tree(Path::new(TOP)) as Arc<dyn OilSource>)
    }
}

fn boot_with_provider() -> Editor {
    let editor = boot();
    let registry = editor
        .services
        .get::<OilSourceRegistryHandle>()
        .expect("the host publishes the oil source registry");
    registry.rcu(|current| {
        let mut next = (**current).clone();
        next.register(Arc::new(MemProvider));
        next
    });
    editor
}

#[test]
fn dash_from_a_providers_buffer_lists_its_directory_from_its_source() {
    let mut editor = boot_with_provider();
    let file = Path::new(TOP).join("top.txt");
    editor.open_synthetic_buffer(&mem_buffer_name(&file), "plugin-output-mode");
    activate_queued_minors(&mut editor);

    press(&mut editor, "-");

    let view = editor.active_pane_buffer_id();
    assert_eq!(editor.oil_source_for(view).id(), "mem");
    assert_eq!(listing_text(&editor), "sub\ntop.txt");
    assert_eq!(editor.cursor.line, 1, "lands on the file `-` came from");
}

#[test]
fn dash_at_the_top_of_a_sources_tree_says_so() {
    let mut editor = boot();
    let root = PathBuf::from(TOP);
    editor.open_oil_listing(mem_tree(&root), root.clone(), None);
    activate_queued_minors(&mut editor);
    let view = editor.active_pane_buffer_id();

    press(&mut editor, "-");

    assert_eq!(editor.active_pane_buffer_id(), view);
    assert_eq!(listing_text(&editor), "sub\ntop.txt");
    assert_eq!(echo(&editor), format!("oil: mem:{TOP} is the top"));
}

#[test]
fn an_entry_opened_in_a_split_is_the_sources_buffer() {
    let mut editor = boot();
    let root = PathBuf::from(TOP);
    editor.open_oil_listing(mem_tree(&root), root.clone(), None);
    press(&mut editor, "j");
    let mut partial = Vec::new();
    let _ = editor.dispatch_chord(KeyChord::ctrl('v'), &mut partial);

    assert_eq!(editor.pane_tree.len(), 2, "`<C-v>` splits");
    assert_eq!(
        editor.document.snapshot().buffer.as_string(),
        format!("{} from mem\n", root.join("top.txt").display()),
        "the new pane shows what the source opened, not the path on disk"
    );
}

#[test]
fn a_source_nobody_provides_is_an_error_not_the_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = boot();
    let before = editor.active_pane_buffer_id();
    let _ = editor.handle_effect(Effect::OpenOilSource {
        source: "unclaimed".into(),
        dir: dir.path().to_path_buf(),
        focus: None,
    });
    assert_eq!(editor.active_pane_buffer_id(), before);
    assert_eq!(echo(&editor), "oil: no listing source `unclaimed`");
}
