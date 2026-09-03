//! OA.14 — can a second (non-multibuffer) virtual-row provider live on a view,
//! and does it survive a refresh?
//!
//! Phase 5 of the org agenda plans three display modes — log, clock report,
//! time grid — each a manual minor that registers ONE virtual-row provider in
//! its `on_activate`. That shape only works if two things hold, and this spike
//! exists to establish them before three slices are written on top:
//!
//! 1. **Capacity.** `register_virtual_row_provider` dedups by `ProviderId`, and
//!    the multibuffer already registers two of its own (excerpt headers +
//!    the status headerline). A third has to be accepted rather than refused.
//! 2. **Lifecycle.** `gr` on an agenda re-emits `OpenProviderView`, which runs
//!    the opener again. If that path rebuilt the view, a provider registered by
//!    a minor would be dropped on every refresh and phase 5 would need a
//!    re-registration hook.
//!
//! The plan flagged (2) as the real risk. It does not fire, and the reason is
//! worth recording rather than re-deriving: the agenda view is `reuse: true`,
//! so the opener returns the EXISTING buffer and `create_multibuffer_view` is
//! never called a second time. A refresh replaces the view's excerpts; it does
//! not rebuild the view. Providers are keyed by `BufferId` in
//! `Editor::virtual_row_providers` and are only ever removed by an explicit
//! `unregister`, which nothing on the refresh path calls.
//!
//! So phase 5 needs no re-registration hook. That is the finding.

use std::sync::Arc;

use lattice_cells::{
    AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_core::{BufferId, Document as CoreDocument};
use lattice_host::editor::Editor;

/// Stands in for `org-agenda-log-mode` and its two siblings: a provider that
/// belongs to a MINOR rather than to the multibuffer, emitting one display-only
/// row. Deliberately not an agenda type — the question is whether the seam
/// carries an arbitrary third provider, and a fake proves that where an agenda
/// one would only prove the agenda works.
#[derive(Debug)]
struct FakeDisplayModeProvider {
    id: ProviderId,
    rows: usize,
}

impl VirtualRowProvider for FakeDisplayModeProvider {
    fn id(&self) -> ProviderId {
        self.id
    }

    /// A static row set, so the worker may cache-hit forever — which is the
    /// documented meaning of a constant version, not a shortcut.
    fn version(&self) -> u64 {
        0
    }

    fn collect(&self) -> Vec<VirtualRow> {
        (0..self.rows)
            .map(|i| VirtualRow {
                media: None,
                anchor_line: i as u32,
                position: AnchorPosition::Above,
                cells: format!("display-mode row {i}")
                    .chars()
                    .map(|c| Cell::with_codepoint(c as u32))
                    .collect::<Vec<_>>()
                    .into(),
                height: 1,
                kind: VirtualRowKind::Sticky,
                bg: None,
                scales: None,
                gutter_line: None,
                gutter_fg: None,
            })
            .collect()
    }
}

/// `ProviderId` is a `u64`, and real providers derive theirs from a stable
/// name + the buffer it belongs to. Same shape here so two views cannot
/// collide, which is the property the dedup contract rests on.
fn fake_provider_id(view: BufferId) -> ProviderId {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "test.display-mode".hash(&mut h);
    view.0.hash(&mut h);
    h.finish()
}

/// A scan source that finds nothing. The agenda opener DECLINES when no source
/// is registered ("no plugin provides agenda rows"), so without one the reopen
/// never reaches the reuse path and the lifecycle question goes untested —
/// which is exactly how the first cut of this spike passed while proving
/// nothing. It needs no rows: the question is the view's lifecycle, not its
/// contents.
#[derive(Debug)]
struct EmptyScanSource {
    exts: Vec<String>,
}

impl lattice_mode::ScannedExcerptSource for EmptyScanSource {
    fn source_id(&self) -> u64 {
        7
    }
    fn extensions(&self) -> &[String] {
        &self.exts
    }
    fn begin(&self, _args: &[String]) -> lattice_mode::ScanBeginFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn scan(&self, _p: std::path::PathBuf, _t: String) -> lattice_mode::ScanFuture<'_> {
        Box::pin(async { Ok(lattice_mode::ScanResult::default()) })
    }
}

/// Boot an editor whose agenda opener will actually open.
fn boot_with_scan_source() -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let handle = editor
        .services
        .get::<lattice_mode::ScannedExcerptSourceRegistryHandle>()
        .expect("boot registers the scanned-excerpt-source registry");
    let mut reg = lattice_mode::ScannedExcerptSourceRegistry::new();
    reg.register(Arc::new(EmptyScanSource {
        exts: vec!["org".to_string()],
    }));
    handle.store(Arc::new(reg));
    editor
}

/// The view this spike opens. Named by the TEST rather than by the host,
/// which is the shape every scan view has now: a plugin declares its own
/// identity and the host supplies only the machinery.
fn identity() -> lattice_multibuffer::providers::scan_view::ScanViewIdentity {
    lattice_multibuffer::providers::scan_view::ScanViewIdentity {
        provider: "test-scan-view".to_string(),
        buffer_name: "*test-scan-view*".to_string(),
        view_mode: None,
        no_rows_message: "no source provides rows for it".to_string(),
    }
}

fn open_the_agenda(editor: &mut Editor) -> BufferId {
    match lattice_multibuffer::providers::scan_view::open_scan_view(
        editor,
        &identity(),
        &lattice_grammar::Args::None,
    ) {
        lattice_mode::ProviderViewOutcome::Opened { view, .. } => view,
        lattice_mode::ProviderViewOutcome::Declined { message } => {
            panic!("the agenda must open for this spike to test anything: {message}")
        }
    }
}

/// (1) A third provider is accepted alongside the multibuffer's own two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_third_provider_registers_alongside_the_multibuffers_two() {
    let mut editor = boot_with_scan_source();
    let view = open_the_agenda(&mut editor);
    assert_eq!(
        editor.virtual_row_providers.snapshot(view).len(),
        3,
        "precondition: the view brings its excerpt-header, status and \
         row-annotation providers (HB.5b added the third)"
    );

    let mine = Arc::new(FakeDisplayModeProvider {
        id: fake_provider_id(view),
        rows: 2,
    });
    assert!(
        editor.virtual_row_providers.register(view, mine.clone()),
        "a third provider is accepted, not refused"
    );
    assert_eq!(
        editor.virtual_row_providers.snapshot(view).len(),
        4,
        "a fourth coexists with the view's own three"
    );

    // Registering the SAME id twice is refused — the dedup contract phase 5
    // relies on, so a mode re-activating cannot double its own rows.
    assert!(
        !editor.virtual_row_providers.register(view, mine),
        "the same ProviderId is refused rather than duplicated"
    );
    assert_eq!(
        editor.virtual_row_providers.snapshot(view).len(),
        4,
        "still four — the refused duplicate added nothing"
    );
}

/// (2) The lifecycle question the plan called the real risk: a provider
/// registered by a minor must outlive a refresh.
///
/// Driven through the opener the refresh actually calls — `gr`'s handler emits
/// `AppEffect::OpenProviderView`, which lands on `open_agenda` — rather than by
/// asserting that nothing calls `unregister`. The latter is the reasoning this
/// test exists to check, so using it as the method would prove nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_minors_provider_survives_a_refresh() {
    let mut editor = boot_with_scan_source();
    let view = open_the_agenda(&mut editor);

    let id = fake_provider_id(view);
    assert!(
        editor
            .virtual_row_providers
            .register(view, Arc::new(FakeDisplayModeProvider { id, rows: 2 }))
    );

    // The refresh: re-open the agenda. `reuse: true` means the opener hands
    // back the EXISTING buffer rather than building a second one.
    let again = open_the_agenda(&mut editor);
    assert_eq!(
        again, view,
        "the agenda reuses its view — if this ever returns a new buffer, every \
         provider a minor registered is orphaned and phase 5 needs a \
         re-registration hook"
    );

    let providers = editor.virtual_row_providers.snapshot(view);
    let survivor = providers
        .iter()
        .find(|p| p.id() == id)
        .expect("the minor's provider outlives a refresh");
    assert_eq!(
        survivor.collect().len(),
        2,
        "…and still emits its rows afterwards"
    );
    assert_eq!(
        providers.len(),
        4,
        "the view's own three are not duplicated by the re-open either"
    );
}
