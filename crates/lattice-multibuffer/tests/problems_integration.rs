//! CM.4 (2026-07-22) integration tests for the `*problems*` view.
//!
//! Exercises `create_problems_view` end-to-end against a `MockActivator`
//! (mirrors `narrow_integration.rs`): error entries across two real
//! temp files → a multibuffer with one excerpt per entry, grouped by
//! file, `ProblemsMinorMode` activated, and the headerline set. The
//! `:copen` / `:cclose` host wiring (empty-list echo, round-trip close)
//! is covered by `lattice-host` dispatch tests; here we pin the
//! substrate contract.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_grammar::CommandRegistry;
use lattice_mode::{BufferStore, BufferStoreHandle, ModeActivator, ModeId, ServiceRegistry};
use lattice_multibuffer::providers::problems::{
    ProblemsMinorMode, create_problems_view, refresh_problems_view,
};
use lattice_multibuffer::{
    HeaderlineStatus, InMemoryMultibufferRegistry, MultibufferRegistryHandle,
};
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};
use lattice_runtime::Document;

#[derive(Debug, Default)]
struct StubBufferStore {
    inserts: Mutex<Vec<(BufferId, BufferKind)>>,
}

impl BufferStore for StubBufferStore {
    fn find_by_name(&self, _name: &str) -> Option<BufferId> {
        None
    }
    fn name_for(&self, _id: BufferId) -> Option<String> {
        None
    }
    fn handle_for(&self, _id: BufferId) -> Option<Arc<dyn Document>> {
        None
    }
    fn insert_document_buffer(
        &self,
        id: BufferId,
        kind: BufferKind,
        _handle: Arc<dyn Document>,
        _flags: BufferFlags,
        _name: Option<String>,
    ) {
        self.inserts.lock().unwrap().push((id, kind));
    }
}

struct MockActivator {
    services: Arc<ServiceRegistry>,
    minor_calls: Vec<(BufferId, ModeId)>,
    mb_registry: MultibufferRegistryHandle,
    /// RV.3: kept so a test can assert refresh inserts no second
    /// buffer.
    store: Arc<StubBufferStore>,
}

impl MockActivator {
    fn new() -> Self {
        let stub_store = Arc::new(StubBufferStore::default());
        let stub_store_keep = Arc::clone(&stub_store);
        let mb_registry: MultibufferRegistryHandle = Arc::new(InMemoryMultibufferRegistry::new());

        let mut services = ServiceRegistry::new();
        let store_dyn: Arc<dyn BufferStore> = stub_store;
        services.register(BufferStoreHandle::new(store_dyn));
        services.register(mb_registry.clone());

        Self {
            services: Arc::new(services),
            minor_calls: Vec::new(),
            mb_registry,
            store: stub_store_keep,
        }
    }

    /// How many buffers were inserted into the store.
    fn insert_count(&self) -> usize {
        self.store.inserts.lock().unwrap().len()
    }
}

impl ModeActivator for MockActivator {
    fn activate_major_for_kind(&mut self, _buffer: BufferId, _kind: BufferKind) {}
    fn activate_minor_by_id(&mut self, buffer: BufferId, mode: ModeId) {
        self.minor_calls.push((buffer, mode));
    }
    fn services(&self) -> Arc<ServiceRegistry> {
        Arc::clone(&self.services)
    }
    fn ensure_named_document(
        &mut self,
        _name: &str,
        _major: ModeId,
        _flags: lattice_core::BufferFlags,
    ) -> BufferId {
        unimplemented!("MockActivator: ensure_named_document is unused in these tests")
    }
}

fn registry_handle() -> lattice_grammar::CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

/// Write `contents` into a fresh unique temp file and return its path.
/// A `TempFiles` guard removes them at test end.
struct TempFiles {
    dir: PathBuf,
}

impl TempFiles {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lattice-problems-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn entry(path: &Path, line: u32, severity: ErrorSeverity, msg: &str) -> ErrorEntry {
    ErrorEntry {
        path: path.to_path_buf(),
        line,
        col: 0,
        severity,
        message: msg.to_string(),
    }
}

const EIGHT_LINES: &str = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\n";
const FOUR_LINES: &str = "b0\nb1\nb2\nb3\n";

#[test]
fn problems_groups_entries_by_file_as_excerpts() {
    let tmp = TempFiles::new("group");
    let file_a = tmp.write("a.rs", EIGHT_LINES);
    let file_b = tmp.write("b.rs", FOUR_LINES);

    // Interleave files (A, B, A) to prove first-seen grouping reorders
    // to A, A, B — and that within-file entries sort by line.
    let entries = vec![
        entry(&file_a, 5, ErrorSeverity::Warning, "unused"),
        entry(&file_b, 1, ErrorSeverity::Error, "type error"),
        entry(&file_a, 2, ErrorSeverity::Error, "borrow"),
    ];

    let mut activator = MockActivator::new();
    let view_id = create_problems_view(&mut activator, &entries, registry_handle(), None)
        .expect("create_problems_view returns a view for non-empty entries");

    let handle = activator
        .mb_registry
        .handle(view_id)
        .expect("registry holds the problems view");

    // One excerpt per entry, grouped by file (A, A, B).
    assert_eq!(handle.excerpt_count(), 3);
    let excerpts = handle.excerpts();
    assert_eq!(excerpts.len(), 3);

    // First two excerpts share source A; the third is source B; A != B.
    let src_a = excerpts[0].source;
    assert_eq!(excerpts[1].source, src_a, "file A entries stay grouped");
    let src_b = excerpts[2].source;
    assert_ne!(src_a, src_b, "different files get distinct source ids");

    // Within file A, entries are ordered by line: 2 then 5.
    // ±2 context, clamped: line 2 → [0,4]; line 5 → [3,7].
    assert_eq!((excerpts[0].start_line, excerpts[0].end_line), (0, 4));
    assert_eq!((excerpts[1].start_line, excerpts[1].end_line), (3, 7));
    // File B line 1, ±2 clamped to last line 3 → [0, 3].
    assert_eq!((excerpts[2].start_line, excerpts[2].end_line), (0, 3));

    // ProblemsMinorMode activated on the view.
    assert!(
        activator
            .minor_calls
            .iter()
            .any(|(b, m)| *b == view_id && *m == ProblemsMinorMode::mode_id()),
        "create_problems_view must activate problems-minor-mode; got {:?}",
        activator.minor_calls
    );

    // Headerline shows the entry + file counts.
    match &*handle.headerline() {
        HeaderlineStatus::Complete { summary, .. } => {
            assert_eq!(summary, "[problems] 3 in 2 files");
        }
        other => panic!("expected Complete headerline, got {other:?}"),
    }
}

#[test]
fn empty_error_returns_none() {
    let mut activator = MockActivator::new();
    let view = create_problems_view(&mut activator, &[], registry_handle(), None);
    assert!(view.is_none(), "empty entry list yields no view");
    assert!(
        activator.minor_calls.is_empty(),
        "no mode activation for an empty list"
    );
}

#[test]
fn all_unreadable_files_returns_none() {
    // A error entry pointing at a nonexistent path: nothing to show.
    let missing = PathBuf::from("/nonexistent/lattice-problems/does-not-exist.rs");
    let entries = vec![entry(&missing, 0, ErrorSeverity::Error, "gone")];
    let mut activator = MockActivator::new();
    let view = create_problems_view(&mut activator, &entries, registry_handle(), None);
    assert!(
        view.is_none(),
        "all-unreadable entry list yields no view (skipped, not panicked)"
    );
}

// ── RV.3: `gr` rebuilds the view in place ─────────────────────────────
//
// The view renders a SNAPSHOT of the error list taken at `:copen` time,
// over sources read from disk at that moment. Both drift — a recompile
// replaces the list, and the files themselves change. Refresh is how
// the user catches up without closing and re-opening.

#[test]
fn refresh_picks_up_entries_added_since_the_view_opened() {
    let tmp = TempFiles::new("refresh-add");
    let file_a = tmp.write("a.rs", EIGHT_LINES);
    let file_b = tmp.write("b.rs", FOUR_LINES);

    let mut activator = MockActivator::new();
    let opened = vec![entry(&file_a, 2, ErrorSeverity::Error, "borrow")];
    let view_id = create_problems_view(&mut activator, &opened, registry_handle(), None).unwrap();
    assert_eq!(
        activator
            .mb_registry
            .handle(view_id)
            .unwrap()
            .excerpt_count(),
        1
    );

    // A second compile run finds two more, one in a file the view had
    // never seen.
    let refreshed = vec![
        entry(&file_a, 2, ErrorSeverity::Error, "borrow"),
        entry(&file_a, 5, ErrorSeverity::Warning, "unused"),
        entry(&file_b, 1, ErrorSeverity::Error, "type error"),
    ];
    let n = refresh_problems_view(&mut activator, view_id, &refreshed)
        .expect("refresh returns the new entry count");
    assert_eq!(n, 3);

    let handle = activator.mb_registry.handle(view_id).unwrap();
    assert_eq!(handle.excerpt_count(), 3, "new entries appear");
    match &*handle.headerline() {
        HeaderlineStatus::Complete { summary, .. } => {
            assert_eq!(summary, "[problems] 3 in 2 files");
        }
        other => panic!("expected Complete headerline, got {other:?}"),
    }
}

/// The whole reason refresh is not a re-open: `create_multibuffer_view`
/// mints a fresh `BufferId` every call, so re-opening would strand the
/// view the user is looking at and add a second `*problems*`.
#[test]
fn refresh_keeps_the_same_buffer_id() {
    let tmp = TempFiles::new("refresh-same-id");
    let file_a = tmp.write("a.rs", EIGHT_LINES);

    let mut activator = MockActivator::new();
    let opened = vec![entry(&file_a, 2, ErrorSeverity::Error, "borrow")];
    let view_id = create_problems_view(&mut activator, &opened, registry_handle(), None).unwrap();

    let shrunk = vec![entry(&file_a, 5, ErrorSeverity::Warning, "unused")];
    refresh_problems_view(&mut activator, view_id, &shrunk).unwrap();

    assert!(
        activator.mb_registry.handle(view_id).is_some(),
        "the original view id must still resolve — refresh is in place"
    );
    // Exactly one buffer was ever inserted: the open. Refresh must not
    // insert a second.
    assert_eq!(
        activator.insert_count(),
        1,
        "refresh must not create another *problems* buffer"
    );
}

/// A refresh that would show nothing leaves the view exactly as it was.
/// Blanking the buffer the user is reading to convey "no results" is the
/// wrong trade — the host arm echoes instead.
#[test]
fn refresh_with_nothing_to_show_leaves_the_view_untouched() {
    let tmp = TempFiles::new("refresh-empty");
    let file_a = tmp.write("a.rs", EIGHT_LINES);

    let mut activator = MockActivator::new();
    let opened = vec![entry(&file_a, 2, ErrorSeverity::Error, "borrow")];
    let view_id = create_problems_view(&mut activator, &opened, registry_handle(), None).unwrap();

    assert_eq!(refresh_problems_view(&mut activator, view_id, &[]), None);
    assert_eq!(
        activator
            .mb_registry
            .handle(view_id)
            .unwrap()
            .excerpt_count(),
        1,
        "an empty refresh must not blank the view"
    );

    // Same for a list whose files have all vanished.
    let gone = vec![entry(
        Path::new("/nonexistent/zzz.rs"),
        0,
        ErrorSeverity::Error,
        "gone",
    )];
    assert_eq!(refresh_problems_view(&mut activator, view_id, &gone), None);
    assert_eq!(
        activator
            .mb_registry
            .handle(view_id)
            .unwrap()
            .excerpt_count(),
        1,
        "unreadable sources must not blank the view either"
    );
}
