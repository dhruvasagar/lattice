//! D.0a.1 (2026-05-29) — Virtual-rows worker.
//!
//! Sibling of `cells_worker`. Owns the rebuild path for the
//! displacing-virtual-row primitive landed in D.0a
//! ([`lattice_cells::VirtualRowMatrix`]) and publishes the
//! result via the shared `virtual_rows_matrix_cell:
//! Arc<ArcSwap<VirtualRowMatrix>>` on `Editor`.
//!
//! ## Why this exists
//!
//! Per paramount goal #1 (`CLAUDE.md`): UI thread does no I/O,
//! no parsing, no shaping. Virtual rows are the renderer's
//! input for inline diff deletion blocks (D.3), multibuffer
//! excerpt headers (M.2), and future inlay-hint /
//! signature-preview consumers. Each consumer registers a
//! [`VirtualRowProvider`]; the worker polls them off-thread
//! and publishes a fresh [`VirtualRowMatrix`] via RCU.
//!
//! ## D.0a.1 scope — minimal
//!
//! - [`VirtualRowProviderRegistry`] — `BufferId`-keyed (as of
//!   D.4.d.2.1.a). Each visible buffer owns its own
//!   `ProviderId → provider` map so baseline + current panes'
//!   filler providers (D.4.c) coexist without colliding on
//!   `ProviderId`, and the worker (post-D.4.d.2.1.b) can
//!   iterate panes and poll each one's scope independently.
//!   This slice keeps the worker single-pane: `recompute`
//!   reads `active_document.document_buffer_id` and snapshots
//!   only that buffer's providers. Today's sole producer (the
//!   D.3.a inline diff overlay) registers against the same
//!   buffer the worker reads, so behaviour is identical for
//!   single-pane flows.
//! - [`recompute`] — sync decision function. Tests call this
//!   directly to assert each branch.
//! - [`run`] — async loop. `wake.notified().await`s the
//!   `VirtualRowsWake` signal, then calls `recompute`.
//! - Cache-hit fingerprinting via a stable hash of
//!   `[(provider_id, provider_version)]` + `source_line_count`.
//!   The worker holds its own monotonic publish counter; the
//!   matrix's `version` only bumps when the fingerprint
//!   changes, so downstream consumers compare versions to
//!   invalidate their derived state cheaply.
//!
//! ## What this slice does NOT do
//!
//! - No production provider yet. The first one lands with
//!   D.3 (inline diff overlay's deletion-block provider) or
//!   M.2 (multibuffer excerpt-header provider), whichever
//!   ships first.
//! - No per-pane matrices. v1 uses one global matrix tied to
//!   the active document. Multi-pane diff (D.4) and
//!   project-wide diff (multibuffer M.6) will introduce
//!   per-pane or per-document indexing then.
//! - No paint-debounce inside the worker. `Notify`-permit
//!   coalescing (mirrors `cells_worker`) handles bursts —
//!   the worker rebuilds once per quiescent burst regardless
//!   of how many wakes arrived.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use tracing::{debug, info};

use lattice_cells::{
    ProviderId, VirtualRow, VirtualRowMatrix, VirtualRowProvider, VirtualRowVersion,
};
use lattice_core::BufferId;

use crate::editor::VirtualRowsWake;
use crate::render_state::RenderState;

/// Process-wide registry of [`VirtualRowProvider`] instances,
/// scoped per [`BufferId`] (D.4.d.2.1.a).
///
/// Each visible buffer that participates in the virtual-rows
/// pipeline owns its own `ProviderId → provider` sub-map.
/// Scoping by `BufferId` is what lets baseline + current
/// panes of a side-by-side diff (D.4.d.3) both register
/// filler providers (D.4.c) without colliding on
/// `ProviderId`, even though the filler ids use side-
/// distinct prefixes — registering against the wrong
/// scope would still mis-route the rows when the worker
/// (post-D.4.d.2.1.b) iterates per-pane.
///
/// Mutation (`register` / `unregister`) is consumer-creation
/// frequency — never per-frame. Read (`snapshot`) is per
/// worker tick. Behind a `std::sync::Mutex`; the hot path
/// holds the lock only long enough to clone out `Arc`
/// references.
#[derive(Debug, Default)]
pub struct VirtualRowProviderRegistry {
    by_buffer: Mutex<HashMap<BufferId, HashMap<ProviderId, Arc<dyn VirtualRowProvider>>>>,
}

impl VirtualRowProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider against `buffer_id`. Returns `false`
    /// if a provider with the same id was already registered in
    /// the same buffer scope (no replacement — the caller is
    /// expected to `unregister` first). Providers with the same
    /// id in *different* buffer scopes do not collide.
    pub fn register(&self, buffer_id: BufferId, provider: Arc<dyn VirtualRowProvider>) -> bool {
        let id = provider.id();
        let mut by_buffer = self
            .by_buffer
            .lock()
            .expect("VirtualRowProviderRegistry mutex poisoned");
        let scope = by_buffer.entry(buffer_id).or_default();
        if scope.contains_key(&id) {
            return false;
        }
        scope.insert(id, provider);
        true
    }

    /// Remove a provider from `buffer_id`'s scope. Returns
    /// `true` if one was removed. Empty scopes are pruned so
    /// `is_empty()` reflects "no providers anywhere".
    pub fn unregister(&self, buffer_id: BufferId, id: ProviderId) -> bool {
        let mut by_buffer = self
            .by_buffer
            .lock()
            .expect("VirtualRowProviderRegistry mutex poisoned");
        let Some(scope) = by_buffer.get_mut(&buffer_id) else {
            return false;
        };
        let removed = scope.remove(&id).is_some();
        if scope.is_empty() {
            by_buffer.remove(&buffer_id);
        }
        removed
    }

    /// Snapshot providers registered against `buffer_id`. Returns
    /// fresh `Arc` clones — callers can hold them past a
    /// concurrent `unregister` (RCU). Order is unspecified.
    /// Returns an empty `Vec` if the buffer has no scope.
    pub fn snapshot(&self, buffer_id: BufferId) -> Vec<Arc<dyn VirtualRowProvider>> {
        self.by_buffer
            .lock()
            .expect("VirtualRowProviderRegistry mutex poisoned")
            .get(&buffer_id)
            .map(|scope| scope.values().cloned().collect())
            .unwrap_or_default()
    }

    /// True iff no buffer scope holds any provider.
    pub fn is_empty(&self) -> bool {
        self.by_buffer
            .lock()
            .expect("VirtualRowProviderRegistry mutex poisoned")
            .is_empty()
    }

    /// Total provider count across all buffer scopes.
    pub fn len(&self) -> usize {
        self.by_buffer
            .lock()
            .expect("VirtualRowProviderRegistry mutex poisoned")
            .values()
            .map(|scope| scope.len())
            .sum()
    }
}

/// AUX‑2: bridge so subsystems without access to
/// `VirtualRowProviderRegistry` can register headerlines.
impl lattice_mode::VirtualRowRegistrar for VirtualRowProviderRegistry {
    fn register(&self, buffer: BufferId, provider: Arc<dyn VirtualRowProvider>) -> bool {
        self.register(buffer, provider)
    }

    fn unregister(&self, buffer: BufferId, id: ProviderId) -> bool {
        self.unregister(buffer, id)
    }
}

/// Recompute decision the worker takes on a wake. Visible for
/// testing; the production loop calls [`recompute`] directly.
///
/// As of D.4.d.2.1.c the worker iterates `rs.cells.panes` and
/// produces one decision per pane; the aggregate decision
/// returned to `run` is the highest-precedence one across
/// panes (`Recomputed > Clear > CacheHit`).
#[derive(Debug, PartialEq, Eq)]
pub enum WorkerDecision {
    /// Pane had no document snapshot (transient race during
    /// buffer close, or non-Document leaf — though the
    /// publisher already filters those out). Worker cleared
    /// that pane's matrix (or noted it was already empty).
    Clear,
    /// Fingerprint of `(providers × versions, source_line_count)`
    /// matches the previously observed fingerprint for this
    /// pane's buffer. No new matrix published; renderer reads
    /// the existing one.
    CacheHit,
    /// Fingerprint changed. Worker polled `collect()` on every
    /// registered provider for the pane's buffer, built a
    /// fresh `VirtualRowMatrix`, and stored it via
    /// `pane.virtual_rows_matrix`.
    Recomputed,
}

/// Worker-local state held across recompute calls.
///
/// `last_fingerprints` lets us short-circuit per buffer when
/// neither the providers nor that buffer's document have
/// changed (D.4.d.2.1.c switched this from a single
/// `Option<u64>` to a `HashMap<BufferId, u64>` so two visible
/// buffers cache-hit independently). `next_publish_version`
/// is a single monotonic counter that stamps every published
/// matrix — across buffers — so downstream consumers compare
/// versions cheaply per matrix.
#[derive(Debug)]
pub struct VirtualRowsWorkerState {
    last_fingerprints: HashMap<BufferId, u64>,
    next_publish_version: u64,
}

impl Default for VirtualRowsWorkerState {
    fn default() -> Self {
        Self {
            last_fingerprints: HashMap::new(),
            // Start at 1; the empty matrix at construction is
            // `VirtualRowVersion::ZERO`, so the first
            // successful publish always carries a strictly
            // higher version.
            next_publish_version: 1,
        }
    }
}

impl VirtualRowsWorkerState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Stable hash of the worker's input axes — providers
/// (each via `id` + `version`) and the document's
/// `source_line_count`. Order-independent over providers
/// (sorted before hashing) so the snapshot order doesn't
/// affect the result.
fn compute_fingerprint(providers: &[Arc<dyn VirtualRowProvider>], source_line_count: u32) -> u64 {
    let mut pairs: Vec<(ProviderId, u64)> =
        providers.iter().map(|p| (p.id(), p.version())).collect();
    pairs.sort_unstable();
    let mut hasher = DefaultHasher::new();
    pairs.hash(&mut hasher);
    source_line_count.hash(&mut hasher);
    hasher.finish()
}

/// Pure sync recompute. Iterates `rs.cells.panes`, dispatches
/// each to [`recompute_pane`], and aggregates the per-pane
/// decisions into a single one for the async loop. Returns the
/// aggregate so tests can assert behaviour without driving
/// `run`.
///
/// D.4.d.2.1.c (2026-05-30): switched from a single global
/// `matrix_cell` write target to per-pane iteration over
/// `rs.cells.panes`. Each pane carries its own
/// `pane.virtual_rows_matrix` registry cell (D.4.d.2.1.b
/// publish-time scaffold), which the worker writes via
/// `pane.virtual_rows_matrix.store(...)`. Active-pane entries
/// share Arc identity with `Editor::virtual_rows_matrix_cell`
/// (D.4.d.2.0 boot seed) so the existing renderer read path
/// through `RenderState.virtual_rows.matrix` is bit-identical
/// for single-pane flows until D.4.d.2.1.d swaps the renderer
/// to per-pane lookup.
///
/// Aggregate decision precedence: `Recomputed > Clear >
/// CacheHit`. Mirrors the cells worker's precedence (minus
/// the incremental path virtual rows don't have). `Recomputed`
/// or `Clear` content changes; the async loop fires
/// `paint_request` on either.
pub fn recompute(
    state: &mut VirtualRowsWorkerState,
    render_state: &ArcSwap<RenderState>,
    providers: &VirtualRowProviderRegistry,
) -> WorkerDecision {
    let rs = render_state.load_full();
    // I.5.2: `cells` is an inner `ArcSwap`; load the snapshot once.
    let cells = rs.cells.load();
    if cells.panes.is_empty() {
        return WorkerDecision::CacheHit;
    }
    let mut any_recomputed = false;
    let mut any_cleared = false;
    for pane in cells.panes.iter() {
        match recompute_pane(pane, state, providers) {
            WorkerDecision::CacheHit => {}
            WorkerDecision::Clear => any_cleared = true,
            WorkerDecision::Recomputed => any_recomputed = true,
        }
    }
    if any_recomputed {
        WorkerDecision::Recomputed
    } else if any_cleared {
        WorkerDecision::Clear
    } else {
        WorkerDecision::CacheHit
    }
}

/// D.4.d.2.1.c (2026-05-30): per-pane recompute. Same shape
/// as `cells_worker::recompute_pane`. Visible for tests that
/// want to assert per-pane decisions without driving the
/// aggregate.
///
/// Writes via `pane.virtual_rows_matrix` (the per-buffer
/// registry cell), so two panes showing the same buffer
/// share a single output cell — the second pane sees a
/// `CacheHit` against the rebuild the first one already
/// published (same `(buffer_id, providers, source_line_count)`
/// fingerprint).
pub fn recompute_pane(
    pane: &crate::render_state::PaneCellsInputs,
    state: &mut VirtualRowsWorkerState,
    providers: &VirtualRowProviderRegistry,
) -> WorkerDecision {
    let Some(snapshot) = pane.snapshot.as_ref() else {
        // No snapshot — buffer closed mid-publish, or no
        // active document for this pane. Clear this pane's
        // matrix if it isn't already empty; idempotent on
        // repeat clears so the second call doesn't churn the
        // Arc. Drop the cached fingerprint so a later
        // re-bind doesn't false-positive cache-hit.
        state.last_fingerprints.remove(&pane.buffer_id);
        let existing = pane.virtual_rows_matrix.load();
        if existing.is_empty() && existing.source_line_count == 0 {
            return WorkerDecision::Clear;
        }
        pane.virtual_rows_matrix
            .store(Arc::new(VirtualRowMatrix::empty()));
        return WorkerDecision::Clear;
    };

    // CV.3: content space — virtual rows anchor to real source lines.
    let source_line_count = snapshot.buffer.content_line_count();
    let provider_snap = providers.snapshot(pane.buffer_id);
    let fingerprint = compute_fingerprint(&provider_snap, source_line_count);

    if state.last_fingerprints.get(&pane.buffer_id) == Some(&fingerprint) {
        return WorkerDecision::CacheHit;
    }
    state.last_fingerprints.insert(pane.buffer_id, fingerprint);

    let mut rows: Vec<VirtualRow> = Vec::new();
    for p in &provider_snap {
        rows.extend(p.collect());
    }

    let publish_version = state.next_publish_version;
    state.next_publish_version = publish_version.wrapping_add(1);
    let new_matrix =
        VirtualRowMatrix::build(rows, source_line_count, VirtualRowVersion(publish_version));
    pane.virtual_rows_matrix.store(Arc::new(new_matrix));
    WorkerDecision::Recomputed
}

/// Worker entry point spawned at boot. Loops forever, awaiting
/// the wake `Notify`. Each wake re-reads the latest
/// `RenderState` + provider registry and calls [`recompute`].
///
/// `paint_request` is the shared `Notify` consumed by the
/// renderer peer — fired on `Recomputed` / `Clear` decisions
/// (content changed). `CacheHit` returns without waking the
/// renderer.
///
/// Coalescing mirrors `cells_worker`: `Notify::notify_one`
/// stores at most one permit, so a burst of wakes during a
/// rebuild collapses to one tail rebuild. No explicit
/// debounce.
pub async fn run(
    render_state: Arc<ArcSwap<RenderState>>,
    wake: VirtualRowsWake,
    providers: Arc<VirtualRowProviderRegistry>,
    paint_request: Arc<tokio::sync::Notify>,
) {
    info!(
        target: "lattice_host::virtual_rows_worker",
        "virtual-rows worker spawned"
    );
    let mut state = VirtualRowsWorkerState::new();
    let mut tick: u64 = 0;
    loop {
        wake.0.notified().await;
        let t0 = std::time::Instant::now();
        let decision = recompute(&mut state, &render_state, &providers);
        let elapsed_us = t0.elapsed().as_micros();
        tick += 1;
        if matches!(decision, WorkerDecision::Recomputed | WorkerDecision::Clear) {
            paint_request.notify_one();
        }
        debug!(
            target: "lattice_host::virtual_rows_worker",
            tick,
            ?decision,
            elapsed_us,
            "virtual-rows worker tick"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lattice_cells::AnchorPosition;
    use lattice_core::{Buffer, BufferId};
    use lattice_runtime::DocumentSnapshot;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The active-document buffer id used across recompute
    /// tests. Mirrors `RenderState::default()`'s
    /// `active_document.document_buffer_id` (also
    /// `BufferId(0)`) so providers registered against this
    /// scope match what `recompute` snapshots.
    const ACTIVE: BufferId = BufferId(0);

    /// Test-only provider that returns a fixed set of rows and
    /// reports a version bumped by tests via `set_version`.
    #[derive(Debug)]
    struct MockProvider {
        id: ProviderId,
        version: AtomicU64,
        rows: Mutex<Vec<VirtualRow>>,
    }

    impl MockProvider {
        fn new(id: ProviderId, rows: Vec<VirtualRow>) -> Self {
            Self {
                id,
                version: AtomicU64::new(1),
                rows: Mutex::new(rows),
            }
        }

        fn bump_version(&self) {
            self.version.fetch_add(1, Ordering::Relaxed);
        }

        fn replace_rows(&self, rows: Vec<VirtualRow>) {
            *self.rows.lock().unwrap() = rows;
            self.bump_version();
        }
    }

    impl VirtualRowProvider for MockProvider {
        fn id(&self) -> ProviderId {
            self.id
        }

        fn version(&self) -> u64 {
            self.version.load(Ordering::Relaxed)
        }

        fn collect(&self) -> Vec<VirtualRow> {
            self.rows.lock().unwrap().clone()
        }
    }

    fn row(anchor: u32, pos: AnchorPosition) -> VirtualRow {
        VirtualRow {
            anchor_line: anchor,
            position: pos,
            cells: Arc::from([] as [lattice_cells::Cell; 0]),
            height: 1,
            kind: lattice_cells::VirtualRowKind::Generic,
            bg: None,
            scales: None,
        }
    }

    /// Build a `PaneCellsInputs` carrying the fields the
    /// virtual-rows worker reads (`buffer_id`, `snapshot`,
    /// `virtual_rows_matrix`) — every other field stays at a
    /// cheap default. Each call mints a fresh `PaneId`.
    fn pane_inputs(
        buffer_id: BufferId,
        snapshot: Option<Arc<DocumentSnapshot>>,
        vr_matrix: Arc<ArcSwap<VirtualRowMatrix>>,
    ) -> crate::render_state::PaneCellsInputs {
        use lattice_core::ui::pane::PaneId;
        crate::render_state::PaneCellsInputs {
            pane_id: PaneId::next(),
            buffer_id,
            matrix: Arc::new(ArcSwap::from_pointee(lattice_cells::CellMatrix::empty())),
            display_matrix: Arc::new(ArcSwap::from_pointee(
                crate::display_matrix::DisplayMatrix::empty(),
            )),
            virtual_rows_matrix: vr_matrix,
            version: lattice_cells::MatrixVersion::ZERO,
            snapshot,
            syntax_handle: None,
            inlay_hints: Arc::from(
                Vec::<crate::render_state::InlayHintRow>::new().into_boxed_slice(),
            ),
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            viewport_height: 10,
            scroll: 0,
            viewport_width: 0,
            wrap: false,
            wrap_reserved_cols: 0,
            foldenable: false,
            last_edit: None,
            excerpt_syntax: Arc::from([]),
            extra_spans: Arc::from([]),
            extra_refine: Arc::from(Vec::new().into_boxed_slice()),
        }
    }

    /// Build an `ArcSwap<RenderState>` whose `cells.panes`
    /// carries the supplied entries verbatim. The
    /// virtual-rows worker now reads `rs.cells.panes` (per
    /// D.4.d.2.1.c) so this is the canonical recompute
    /// fixture.
    fn rs_with_panes(
        panes: Vec<crate::render_state::PaneCellsInputs>,
    ) -> Arc<ArcSwap<RenderState>> {
        let cells = crate::render_state::CellsRenderState {
            panes: Arc::from(panes.into_boxed_slice()),
            ..crate::render_state::CellsRenderState::default()
        };
        let rs = RenderState {
            cells: Arc::new(ArcSwap::from_pointee(cells)),
            ..RenderState::default()
        };
        Arc::new(ArcSwap::from_pointee(rs))
    }

    /// Shorthand: build a fresh `DocumentSnapshot` carrying
    /// the supplied text. Used by the line-count-driven tests.
    fn snapshot_with_text(text: &str) -> Arc<DocumentSnapshot> {
        let snap = DocumentSnapshot {
            buffer: Buffer::from_text(text),
            ..Default::default()
        };
        Arc::new(snap)
    }

    /// Build the canonical single-pane fixture: one pane
    /// scoped to `ACTIVE` carrying a snapshot of `text` and
    /// writing into `cell`.
    fn rs_with_single_pane(
        text: &str,
        cell: Arc<ArcSwap<VirtualRowMatrix>>,
    ) -> Arc<ArcSwap<RenderState>> {
        rs_with_panes(vec![pane_inputs(
            ACTIVE,
            Some(snapshot_with_text(text)),
            cell,
        )])
    }

    // ── Registry ──────────────────────────────────────────────

    #[test]
    fn registry_register_lookup_unregister() {
        let reg = VirtualRowProviderRegistry::new();
        assert!(reg.is_empty());
        let p = Arc::new(MockProvider::new(1, vec![]));
        assert!(reg.register(ACTIVE, p as Arc<dyn VirtualRowProvider>));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.snapshot(ACTIVE).len(), 1);
        assert!(reg.unregister(ACTIVE, 1));
        assert!(reg.is_empty());
    }

    #[test]
    fn registry_duplicate_register_in_same_scope_returns_false() {
        let reg = VirtualRowProviderRegistry::new();
        let p1 = Arc::new(MockProvider::new(1, vec![]));
        let p2 = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
        assert!(reg.register(ACTIVE, p1 as Arc<dyn VirtualRowProvider>));
        assert!(!reg.register(ACTIVE, p2 as Arc<dyn VirtualRowProvider>));
        assert_eq!(reg.len(), 1);
    }

    /// D.4.d.2.1.a: same `ProviderId` in two distinct buffer
    /// scopes coexist — this is what lets baseline + current
    /// panes both run their filler providers without one
    /// rejecting the other. Even though today's filler ids
    /// (`0xD1FF_0001_*` vs `0xD1FF_0002_*`) are side-distinct,
    /// the registry scoping is what makes the per-pane worker
    /// iteration (D.4.d.2.1.b) sound when two providers
    /// happen to share an id.
    #[test]
    fn registry_isolates_providers_by_buffer() {
        let reg = VirtualRowProviderRegistry::new();
        let baseline = BufferId(7);
        let current = BufferId(8);
        let p1 = Arc::new(MockProvider::new(42, vec![]));
        let p2 = Arc::new(MockProvider::new(42, vec![row(0, AnchorPosition::Above)]));
        assert!(reg.register(baseline, p1 as Arc<dyn VirtualRowProvider>));
        assert!(reg.register(current, p2 as Arc<dyn VirtualRowProvider>));
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.snapshot(baseline).len(), 1);
        assert_eq!(reg.snapshot(current).len(), 1);
    }

    #[test]
    fn registry_snapshot_scoped_to_buffer() {
        let reg = VirtualRowProviderRegistry::new();
        let bid_a = BufferId(1);
        let bid_b = BufferId(2);
        reg.register(
            bid_a,
            Arc::new(MockProvider::new(10, vec![])) as Arc<dyn VirtualRowProvider>,
        );
        reg.register(
            bid_b,
            Arc::new(MockProvider::new(20, vec![])) as Arc<dyn VirtualRowProvider>,
        );
        let snap_a = reg.snapshot(bid_a);
        let snap_b = reg.snapshot(bid_b);
        assert_eq!(snap_a.len(), 1);
        assert_eq!(snap_a[0].id(), 10);
        assert_eq!(snap_b.len(), 1);
        assert_eq!(snap_b[0].id(), 20);
        // Unknown buffer → empty snapshot, never a panic.
        assert!(reg.snapshot(BufferId(99)).is_empty());
    }

    #[test]
    fn registry_unregister_only_affects_its_buffer() {
        let reg = VirtualRowProviderRegistry::new();
        let bid_a = BufferId(1);
        let bid_b = BufferId(2);
        reg.register(
            bid_a,
            Arc::new(MockProvider::new(5, vec![])) as Arc<dyn VirtualRowProvider>,
        );
        reg.register(
            bid_b,
            Arc::new(MockProvider::new(5, vec![])) as Arc<dyn VirtualRowProvider>,
        );
        assert!(reg.unregister(bid_a, 5));
        assert!(reg.snapshot(bid_a).is_empty());
        assert_eq!(reg.snapshot(bid_b).len(), 1);
        // Pruning: bid_a scope removed entirely → len drops to 1.
        assert_eq!(reg.len(), 1);
        // Idempotent: removing again is a no-op `false`, not a panic.
        assert!(!reg.unregister(bid_a, 5));
        // Unknown buffer unregister → false, no panic.
        assert!(!reg.unregister(BufferId(99), 5));
    }

    // ── Fingerprint ───────────────────────────────────────────

    #[test]
    fn fingerprint_independent_of_provider_order() {
        let p1: Arc<dyn VirtualRowProvider> = Arc::new(MockProvider::new(1, vec![]));
        let p2: Arc<dyn VirtualRowProvider> = Arc::new(MockProvider::new(2, vec![]));
        let fp_a = compute_fingerprint(&[p1.clone(), p2.clone()], 10);
        let fp_b = compute_fingerprint(&[p2, p1], 10);
        assert_eq!(fp_a, fp_b);
    }

    #[test]
    fn fingerprint_changes_when_source_line_count_changes() {
        let p: Arc<dyn VirtualRowProvider> = Arc::new(MockProvider::new(1, vec![]));
        let fp_a = compute_fingerprint(std::slice::from_ref(&p), 10);
        let fp_b = compute_fingerprint(&[p], 11);
        assert_ne!(fp_a, fp_b);
    }

    #[test]
    fn fingerprint_changes_when_provider_version_changes() {
        let mock = Arc::new(MockProvider::new(1, vec![]));
        let dyn_provider: Arc<dyn VirtualRowProvider> = mock.clone();
        let fp_a = compute_fingerprint(std::slice::from_ref(&dyn_provider), 10);
        mock.bump_version();
        let fp_b = compute_fingerprint(&[dyn_provider], 10);
        assert_ne!(fp_a, fp_b);
    }

    // ── Recompute ─────────────────────────────────────────────

    /// D.4.d.2.1.c: empty `rs.cells.panes` ⇒ `CacheHit`. The
    /// pre-D.4.d.2.1.c semantics emitted `Clear` here (off
    /// the global `matrix_cell`); after the pane-driven cutover
    /// there's no top-level cell to write through, and "no
    /// panes" is observationally indistinguishable from a
    /// quiet tick — matching the cells worker's precedent.
    #[test]
    fn recompute_empty_panes_is_cache_hit() {
        let mut state = VirtualRowsWorkerState::new();
        let rs = rs_with_panes(vec![]);
        let reg = VirtualRowProviderRegistry::new();
        let decision = recompute(&mut state, &rs, &reg);
        assert_eq!(decision, WorkerDecision::CacheHit);
    }

    /// D.4.d.2.1.c: a pane whose `snapshot` is `None` clears
    /// that pane's matrix (transient buffer-close race
    /// behaviour). Aggregate decision is `Clear` since no
    /// pane recomputed.
    #[test]
    fn recompute_pane_without_snapshot_clears_its_matrix() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        // Seed the cell with non-empty content so the clear
        // branch is observable (the no-op clear path returns
        // `Clear` too, but doesn't churn the Arc).
        cell.store(Arc::new(VirtualRowMatrix::build(
            vec![row(0, AnchorPosition::Above)],
            3,
            VirtualRowVersion(7),
        )));
        assert_eq!(cell.load_full().len(), 1);
        let rs = rs_with_panes(vec![pane_inputs(ACTIVE, None, cell.clone())]);
        let reg = VirtualRowProviderRegistry::new();
        let d = recompute(&mut state, &rs, &reg);
        assert_eq!(d, WorkerDecision::Clear);
        assert_eq!(cell.load_full().len(), 0, "matrix cleared");
    }

    #[test]
    fn recompute_with_no_providers_is_cache_hit_after_first() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_single_pane("a\nb\nc\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();
        // First wake: fingerprint not yet seen → Recomputed
        // (publishes an empty matrix tagged with the line count).
        let d1 = recompute(&mut state, &rs, &reg);
        assert_eq!(d1, WorkerDecision::Recomputed);
        // Second wake: identical fingerprint → CacheHit.
        let d2 = recompute(&mut state, &rs, &reg);
        assert_eq!(d2, WorkerDecision::CacheHit);
    }

    #[test]
    fn recompute_publishes_rows_from_provider() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_single_pane("a\nb\nc\nd\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();
        let provider = Arc::new(MockProvider::new(
            1,
            vec![row(1, AnchorPosition::Above), row(2, AnchorPosition::Below)],
        ));
        reg.register(ACTIVE, provider.clone() as Arc<dyn VirtualRowProvider>);

        let d = recompute(&mut state, &rs, &reg);
        assert_eq!(d, WorkerDecision::Recomputed);
        let published = cell.load_full();
        assert_eq!(published.len(), 2);
        assert!(!published.is_empty());
    }

    #[test]
    fn recompute_cache_hit_after_provider_static() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_single_pane("a\nb\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();
        let provider = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
        reg.register(ACTIVE, provider as Arc<dyn VirtualRowProvider>);

        let d1 = recompute(&mut state, &rs, &reg);
        assert_eq!(d1, WorkerDecision::Recomputed);
        let d2 = recompute(&mut state, &rs, &reg);
        assert_eq!(d2, WorkerDecision::CacheHit);
    }

    #[test]
    fn recompute_re_publishes_when_provider_version_bumps() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_single_pane("a\nb\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();
        let provider = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
        reg.register(ACTIVE, provider.clone() as Arc<dyn VirtualRowProvider>);

        recompute(&mut state, &rs, &reg);
        let v1 = cell.load_full().version;

        provider.replace_rows(vec![
            row(0, AnchorPosition::Above),
            row(1, AnchorPosition::Below),
        ]);
        let d = recompute(&mut state, &rs, &reg);
        assert_eq!(d, WorkerDecision::Recomputed);
        let v2 = cell.load_full().version;
        assert!(v2.0 > v1.0, "publish version monotonically bumps");
        assert_eq!(cell.load_full().len(), 2);
    }

    #[test]
    fn recompute_re_publishes_when_document_line_count_changes() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs1 = rs_with_single_pane("a\nb\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();

        let d1 = recompute(&mut state, &rs1, &reg);
        assert_eq!(d1, WorkerDecision::Recomputed);
        let v1 = cell.load_full().version;

        // Re-publish the same cell against a longer document.
        let rs2 = rs_with_single_pane("a\nb\nc\nd\n", cell.clone());
        let d2 = recompute(&mut state, &rs2, &reg);
        assert_eq!(d2, WorkerDecision::Recomputed);
        let v2 = cell.load_full().version;
        assert!(v2.0 > v1.0);
        // CV.3: content space — "a\nb\nc\nd\n" is a FOUR line document.
        // This pinned ropey's raw 5, i.e. the phantom line after the
        // terminating newline, as the virtual-row matrix's extent.
        assert_eq!(cell.load_full().source_line_count, 4);
    }

    #[test]
    fn recompute_merges_rows_from_multiple_providers() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_single_pane("a\nb\nc\nd\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();
        let p1 = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
        let p2 = Arc::new(MockProvider::new(2, vec![row(2, AnchorPosition::Below)]));
        reg.register(ACTIVE, p1 as Arc<dyn VirtualRowProvider>);
        reg.register(ACTIVE, p2 as Arc<dyn VirtualRowProvider>);

        recompute(&mut state, &rs, &reg);
        let published = cell.load_full();
        assert_eq!(published.len(), 2);
    }

    /// D.4.d.2.1.a: the worker scopes its provider snapshot
    /// to each pane's buffer. A provider registered against a
    /// *different* buffer must not contribute rows to a pane
    /// showing a different buffer — load-bearing for
    /// D.4.d.2.1.c's per-pane iteration, where each pane's
    /// matrix only sees its own buffer's providers.
    #[test]
    fn recompute_only_polls_active_doc_providers() {
        let mut state = VirtualRowsWorkerState::new();
        let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_single_pane("a\nb\nc\n", cell.clone());
        let reg = VirtualRowProviderRegistry::new();
        // Register against a buffer that is NOT the pane's.
        let foreign = BufferId(42);
        assert_ne!(foreign, ACTIVE);
        let provider = Arc::new(MockProvider::new(
            1,
            vec![row(0, AnchorPosition::Above), row(1, AnchorPosition::Below)],
        ));
        reg.register(foreign, provider as Arc<dyn VirtualRowProvider>);

        let d = recompute(&mut state, &rs, &reg);
        // Recomputed because the fingerprint of (no providers,
        // line_count=4) is unseen for this buffer; matrix stays empty.
        assert_eq!(d, WorkerDecision::Recomputed);
        assert_eq!(cell.load_full().len(), 0);
    }

    // ── D.4.d.2.1.c: per-pane iteration ───────────────────────

    /// Two panes for distinct buffers, each with its own
    /// provider scoped to its buffer. Both panes recompute on
    /// the same tick; each pane's matrix carries that pane's
    /// provider's rows. Mirror of
    /// `cells_worker::two_panes_distinct_buffers_both_rebuild`.
    #[test]
    fn two_panes_distinct_buffers_both_rebuild() {
        let mut state = VirtualRowsWorkerState::new();
        let bid_a = BufferId(11);
        let bid_b = BufferId(22);
        let cell_a = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let cell_b = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_panes(vec![
            pane_inputs(bid_a, Some(snapshot_with_text("a\n")), cell_a.clone()),
            pane_inputs(bid_b, Some(snapshot_with_text("b\nb\n")), cell_b.clone()),
        ]);
        let reg = VirtualRowProviderRegistry::new();
        let pa = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
        let pb = Arc::new(MockProvider::new(
            2,
            vec![row(0, AnchorPosition::Above), row(1, AnchorPosition::Below)],
        ));
        reg.register(bid_a, pa as Arc<dyn VirtualRowProvider>);
        reg.register(bid_b, pb as Arc<dyn VirtualRowProvider>);

        let d = recompute(&mut state, &rs, &reg);
        assert_eq!(d, WorkerDecision::Recomputed);
        assert_eq!(
            cell_a.load_full().len(),
            1,
            "buffer A: one row from its provider"
        );
        assert_eq!(
            cell_b.load_full().len(),
            2,
            "buffer B: two rows from its provider"
        );
    }

    /// Two panes for distinct buffers. After the initial
    /// publish, bump only buffer A's provider. Only pane A
    /// should rebuild; pane B's matrix Arc must be the
    /// identical `Arc<VirtualRowMatrix>` it carried before
    /// (no `store` call on a cache-hit pane). Mirror of
    /// `cells_worker::per_pane_cache_hit_skips_unchanged_pane`.
    #[test]
    fn per_pane_cache_hit_skips_unchanged_pane() {
        let mut state = VirtualRowsWorkerState::new();
        let bid_a = BufferId(11);
        let bid_b = BufferId(22);
        let cell_a = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let cell_b = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let rs = rs_with_panes(vec![
            pane_inputs(bid_a, Some(snapshot_with_text("a\n")), cell_a.clone()),
            pane_inputs(bid_b, Some(snapshot_with_text("b\n")), cell_b.clone()),
        ]);
        let reg = VirtualRowProviderRegistry::new();
        let pa = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
        let pb = Arc::new(MockProvider::new(2, vec![row(0, AnchorPosition::Above)]));
        reg.register(bid_a, pa.clone() as Arc<dyn VirtualRowProvider>);
        reg.register(bid_b, pb as Arc<dyn VirtualRowProvider>);

        // First tick: both panes publish.
        assert_eq!(recompute(&mut state, &rs, &reg), WorkerDecision::Recomputed);
        let matrix_b_v1 = cell_b.load_full();

        // Bump buffer A's provider only.
        pa.replace_rows(vec![
            row(0, AnchorPosition::Above),
            row(0, AnchorPosition::Below),
        ]);
        assert_eq!(recompute(&mut state, &rs, &reg), WorkerDecision::Recomputed);

        // Pane A rebuilt (now 2 rows); pane B's Arc identity
        // must be unchanged (no `store` on cache-hit).
        assert_eq!(cell_a.load_full().len(), 2);
        let matrix_b_v2 = cell_b.load_full();
        assert!(
            Arc::ptr_eq(&matrix_b_v1, &matrix_b_v2),
            "unchanged pane's matrix Arc must survive untouched"
        );
    }

    /// Two panes showing the *same* buffer share the same
    /// registry cell. The first pane processed recomputes and
    /// writes; the second pane's iteration sees the cached
    /// fingerprint and short-circuits — exactly one write
    /// against the shared cell per tick, no thrash. Mirror of
    /// `cells_worker::two_panes_sharing_buffer_share_one_matrix_write`.
    #[test]
    fn two_panes_sharing_buffer_share_one_matrix_write() {
        let mut state = VirtualRowsWorkerState::new();
        let bid = BufferId(33);
        let shared_cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
        let snap = snapshot_with_text("x\ny\n");
        let rs = rs_with_panes(vec![
            pane_inputs(bid, Some(snap.clone()), shared_cell.clone()),
            pane_inputs(bid, Some(snap.clone()), shared_cell.clone()),
        ]);
        let reg = VirtualRowProviderRegistry::new();
        let provider = Arc::new(MockProvider::new(
            7,
            vec![row(0, AnchorPosition::Above), row(1, AnchorPosition::Below)],
        ));
        reg.register(bid, provider as Arc<dyn VirtualRowProvider>);

        let d = recompute(&mut state, &rs, &reg);
        // Aggregate is `Recomputed` because the first pane
        // rebuilt; the second is a `CacheHit` against the
        // fingerprint already cached for `bid` and writes
        // nothing.
        assert_eq!(d, WorkerDecision::Recomputed);
        assert_eq!(shared_cell.load_full().len(), 2);

        // A second tick over the same `rs` is now a full
        // aggregate `CacheHit` — neither pane writes.
        let d2 = recompute(&mut state, &rs, &reg);
        assert_eq!(d2, WorkerDecision::CacheHit);
    }
}
