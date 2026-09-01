//! Repro harness for "agenda on refresh breaks (does not load anything back)".
//!
//! Drives `open_scan_view` twice against the same services — which is exactly
//! what `gr` does: `AgendaViewMode`'s refresh handler emits
//! `AppEffect::OpenProviderView { provider: "agenda" }`, and the host's arm
//! calls the registered opener a second time.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_grammar::{Args, CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    BufferStore, BufferStoreHandle, ModeActivator, ModeId, ProviderViewOutcome, ScannedExcerpt,
    ScannedExcerptSource, ScannedExcerptSourceRegistry, ScannedExcerptSourceRegistryHandle,
    ServiceRegistry,
};
use lattice_multibuffer::providers::agenda::{
    AgendaServiceHandle, InMemoryAgendaService, ScanViewIdentity, open_scan_view,
};
use lattice_multibuffer::{
    HeaderlineStatus, InMemoryMultibufferRegistry, MultibufferRegistryHandle,
};
use lattice_runtime::Document;

// ── a buffer store that actually remembers names ────────────────────────────

#[derive(Debug, Default)]
struct NamedBufferStore {
    by_name: Mutex<HashMap<String, BufferId>>,
    handles: Mutex<HashMap<BufferId, Arc<dyn Document>>>,
}

impl BufferStore for NamedBufferStore {
    fn find_by_name(&self, name: &str) -> Option<BufferId> {
        self.by_name.lock().unwrap().get(name).copied()
    }
    fn name_for(&self, id: BufferId) -> Option<String> {
        self.by_name
            .lock()
            .unwrap()
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(k, _)| k.clone())
    }
    fn handle_for(&self, id: BufferId) -> Option<Arc<dyn Document>> {
        self.handles.lock().unwrap().get(&id).cloned()
    }
    fn insert_document_buffer(
        &self,
        id: BufferId,
        _kind: BufferKind,
        handle: Arc<dyn Document>,
        _flags: BufferFlags,
        name: Option<String>,
    ) {
        if let Some(name) = name {
            self.by_name.lock().unwrap().insert(name, id);
        }
        self.handles.lock().unwrap().insert(id, handle);
    }
}

struct MockActivator {
    services: Arc<ServiceRegistry>,
}

impl ModeActivator for MockActivator {
    fn activate_major_for_kind(&mut self, _buffer: BufferId, _kind: BufferKind) {}
    fn activate_minor_by_id(&mut self, _buffer: BufferId, _mode: ModeId) {}
    fn services(&self) -> Arc<ServiceRegistry> {
        Arc::clone(&self.services)
    }
    fn ensure_named_document(
        &mut self,
        _name: &str,
        _major: ModeId,
        _flags: BufferFlags,
    ) -> BufferId {
        unimplemented!("unused")
    }
}

// ── a source shaped like org's: rows per `* TODO <n>` line ──────────────────

#[derive(Debug)]
struct FakeSource {
    id: u64,
    exts: Vec<String>,
    roots: Vec<String>,
    begins: Arc<std::sync::atomic::AtomicUsize>,
    scans: Arc<std::sync::atomic::AtomicUsize>,
    /// OA.0c: hold `scan` open so a test can look at the view MID-scan, which
    /// is the whole window the slice is about.
    blocked: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeSource {
    fn block(&self) {
        self.blocked
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn unblock(&self) {
        self.blocked
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ScannedExcerptSource for FakeSource {
    fn source_id(&self) -> u64 {
        self.id
    }
    fn extensions(&self) -> &[String] {
        &self.exts
    }
    fn roots(&self) -> lattice_mode::scanned_excerpt_source::AgendaRootsFuture<'_> {
        let roots = self.roots.clone();
        Box::pin(async move { Ok(roots) })
    }
    fn begin(&self) -> lattice_mode::scanned_excerpt_source::AgendaBeginFuture<'_> {
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn scan(
        &self,
        _path: PathBuf,
        text: String,
    ) -> lattice_mode::scanned_excerpt_source::AgendaFuture<'_> {
        self.scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let blocked = Arc::clone(&self.blocked);
        Box::pin(async move {
            while blocked.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            Ok(text
                .lines()
                .enumerate()
                .filter_map(|(i, line)| {
                    let rest = line.strip_prefix("* TODO ")?;
                    let key: i64 = rest.trim().parse().ok()?;
                    Some(ScannedExcerpt {
                        line: i as u32,
                        end_line: i as u32,
                        group: format!("day-{key}"),
                        label: format!("Day {key}"),
                        sort_key: key,
                    })
                })
                .collect())
        })
    }
}

fn tempdir() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lattice-agenda-refresh-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn settle(registry: &MultibufferRegistryHandle, view: BufferId) -> HeaderlineStatus {
    for _ in 0..600 {
        if let Some(h) = registry.handle(view) {
            let status = (*h.headerline()).clone();
            if matches!(
                status,
                HeaderlineStatus::Complete { .. } | HeaderlineStatus::Failed { .. }
            ) {
                return status;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the agenda scan never reached a terminal headerline");
}

/// The wiring every test here needs: a named buffer store, a multibuffer
/// registry, one `FakeSource` rooted at `dir`, and the agenda's view identity.
fn harness(
    dir: &std::path::Path,
) -> (
    MockActivator,
    MultibufferRegistryHandle,
    Arc<FakeSource>,
    ScanViewIdentity,
) {
    let store = Arc::new(NamedBufferStore::default());
    let mb_registry: MultibufferRegistryHandle = Arc::new(InMemoryMultibufferRegistry::new());
    let source = Arc::new(FakeSource {
        id: 1,
        exts: vec!["org".to_string()],
        roots: vec![dir.display().to_string()],
        begins: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        scans: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        blocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let mut sources = ScannedExcerptSourceRegistry::new();
    sources.register(source.clone());
    let sources: ScannedExcerptSourceRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(sources));

    let mut services = ServiceRegistry::new();
    let store_dyn: Arc<dyn BufferStore> = store.clone();
    services.register(BufferStoreHandle::new(store_dyn));
    services.register(mb_registry.clone());
    services.register(sources);
    let cmd: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    services.register(cmd);
    let agenda_service: AgendaServiceHandle = InMemoryAgendaService::handle();
    services.register(agenda_service);

    let activator = MockActivator {
        services: Arc::new(services),
    };
    let identity = ScanViewIdentity {
        provider: "agenda".to_string(),
        buffer_name: "*agenda*".to_string(),
        view_mode: None,
        no_rows_message: "no plugin provides rows for it".to_string(),
    };
    (activator, mb_registry, source, identity)
}

/// `gr` re-runs the SAME opener with the SAME args. The rows must come back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_repopulates_the_view() {
    let dir = tempdir();
    std::fs::write(dir.join("a.org"), "* TODO 30\n").unwrap();
    std::fs::write(dir.join("b.org"), "* TODO 10\n* TODO 20\n").unwrap();

    let (mut activator, mb_registry, _source, identity) = harness(&dir);

    let first = open_scan_view(&mut activator, &identity, &Args::None);
    let ProviderViewOutcome::Opened { view, .. } = first else {
        panic!("first open declined: {first:?}");
    };
    let status = settle(&mb_registry, view).await;
    let first_rows = mb_registry.handle(view).unwrap().excerpts().len();
    assert_eq!(first_rows, 3, "first open, got {status:?}");

    let second = open_scan_view(&mut activator, &identity, &Args::None);
    let ProviderViewOutcome::Opened { view: view2, .. } = second else {
        panic!("refresh declined: {second:?}");
    };
    assert_eq!(view2, view, "the refresh must reuse the same view buffer");
    let status = settle(&mb_registry, view2).await;
    assert_eq!(
        mb_registry.handle(view2).unwrap().excerpts().len(),
        3,
        "the refresh must repopulate the view, got {status:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA.0c: a refresh must not blank the view while it is scanning.
///
/// The scan collects every file, sorts once and writes in a single terminal
/// call, so clearing at open left the view empty for the WHOLE scan. With a
/// quadratic walk that window was tens of seconds, which is why the bug was
/// reported as "refresh does not load anything back" rather than as slowness.
///
/// Asserted by making the source block: the rows visible mid-scan must be the
/// PREVIOUS scan's, not nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_keeps_the_old_rows_until_the_new_ones_land() {
    let dir = tempdir();
    std::fs::write(dir.join("a.org"), "* TODO 30\n").unwrap();
    std::fs::write(dir.join("b.org"), "* TODO 10\n* TODO 20\n").unwrap();

    let (mut activator, mb_registry, source, identity) = harness(&dir);

    let first = open_scan_view(&mut activator, &identity, &Args::None);
    let ProviderViewOutcome::Opened { view, .. } = first else {
        panic!("first open declined: {first:?}");
    };
    settle(&mb_registry, view).await;
    assert_eq!(mb_registry.handle(view).unwrap().excerpts().len(), 3);

    // Hold the next scan open, then refresh.
    source.block();
    let second = open_scan_view(&mut activator, &identity, &Args::None);
    let ProviderViewOutcome::Opened { view: view2, .. } = second else {
        panic!("refresh declined: {second:?}");
    };
    assert_eq!(view2, view);

    // Give the scan a chance to reach the blocked source and to have cleared
    // the view if it were going to.
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        mb_registry.handle(view).unwrap().excerpts().len(),
        3,
        "the previous scan's rows must still be on screen while the refresh runs"
    );

    source.unblock();
    settle(&mb_registry, view).await;
    assert_eq!(
        mb_registry.handle(view).unwrap().excerpts().len(),
        3,
        "and the new rows REPLACE them rather than appending to them"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The other half, and the one that makes keeping rows safe: a refresh that
/// genuinely finds nothing must CLEAR. Stale rows are worse than an empty
/// view, because they look exactly like correct ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_that_finds_nothing_clears_the_view() {
    let dir = tempdir();
    std::fs::write(dir.join("a.org"), "* TODO 30\n").unwrap();

    let (mut activator, mb_registry, _source, identity) = harness(&dir);

    let first = open_scan_view(&mut activator, &identity, &Args::None);
    let ProviderViewOutcome::Opened { view, .. } = first else {
        panic!("first open declined: {first:?}");
    };
    settle(&mb_registry, view).await;
    assert_eq!(mb_registry.handle(view).unwrap().excerpts().len(), 1);

    // The rows are gone from disk before the refresh.
    std::fs::remove_file(dir.join("a.org")).unwrap();

    let second = open_scan_view(&mut activator, &identity, &Args::None);
    let ProviderViewOutcome::Opened { view: view2, .. } = second else {
        panic!("refresh declined: {second:?}");
    };
    settle(&mb_registry, view2).await;
    assert_eq!(
        mb_registry.handle(view2).unwrap().excerpts().len(),
        0,
        "an empty result must clear, or a refresh silently shows yesterday"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
