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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use tracing::{debug, info};

use lattice_cells::{ProviderId, VirtualRow, VirtualRowMatrix, VirtualRowProvider, VirtualRowVersion};
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
	pub fn register(
		&self,
		buffer_id: BufferId,
		provider: Arc<dyn VirtualRowProvider>,
	) -> bool {
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

/// Recompute decision the worker takes on a wake. Visible for
/// testing; the production loop calls [`recompute`] directly.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkerDecision {
	/// No active document. Worker cleared the matrix (or
	/// noted it was already empty). Renderer should treat
	/// the matrix as having no virtual rows.
	Clear,
	/// Fingerprint of `(providers × versions, source_line_count)`
	/// matches the previously observed fingerprint. No new
	/// matrix published; renderer reads the existing one.
	CacheHit,
	/// Fingerprint changed. Worker polled `collect()` on every
	/// registered provider, built a fresh `VirtualRowMatrix`,
	/// and stored it via the shared `matrix_cell`.
	Recomputed,
}

/// Worker-local state held across recompute calls.
///
/// `last_fingerprint` lets us short-circuit when neither the
/// providers nor the document have changed; `next_publish_version`
/// is the monotonic counter that stamps published matrices so
/// downstream consumers can compare versions cheaply.
#[derive(Debug)]
pub struct VirtualRowsWorkerState {
	last_fingerprint: Option<u64>,
	next_publish_version: u64,
}

impl Default for VirtualRowsWorkerState {
	fn default() -> Self {
		Self {
			last_fingerprint: None,
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
fn compute_fingerprint(
	providers: &[Arc<dyn VirtualRowProvider>],
	source_line_count: u32,
) -> u64 {
	let mut pairs: Vec<(ProviderId, u64)> =
		providers.iter().map(|p| (p.id(), p.version())).collect();
	pairs.sort_unstable();
	let mut hasher = DefaultHasher::new();
	pairs.hash(&mut hasher);
	source_line_count.hash(&mut hasher);
	hasher.finish()
}

/// Pure sync recompute. Reads the current `RenderState` for
/// the active document's line count, snapshots the provider
/// registry, fingerprints the inputs, and either publishes a
/// fresh matrix or returns a cache hit. Returns the decision
/// taken so tests can assert each branch without driving the
/// async loop.
pub fn recompute(
	state: &mut VirtualRowsWorkerState,
	render_state: &ArcSwap<RenderState>,
	providers: &VirtualRowProviderRegistry,
	matrix_cell: &ArcSwap<VirtualRowMatrix>,
) -> WorkerDecision {
	let rs = render_state.load_full();
	let snapshot = rs.cells.snapshot.as_ref();
	let source_line_count = snapshot
		.map(|s| s.buffer.line_count())
		.unwrap_or(0);

	// Clear branch: no active document.
	if snapshot.is_none() {
		let existing = matrix_cell.load();
		if existing.is_empty() && existing.source_line_count == 0 {
			// Already at the initial empty state; no publish
			// needed. Reset our fingerprint so a future
			// rebind doesn't false-positive cache-hit.
			state.last_fingerprint = None;
			return WorkerDecision::Clear;
		}
		matrix_cell.store(Arc::new(VirtualRowMatrix::empty()));
		state.last_fingerprint = None;
		return WorkerDecision::Clear;
	}

	// D.4.d.2.1.a: scope provider snapshot to the active
	// document's buffer. Today's sole producer (D.3.a inline
	// diff overlay) registers against the same buffer the
	// worker reads, so single-pane behaviour is identical.
	// D.4.d.2.1.b will iterate `rs.cells.panes` and snapshot
	// each visible buffer's scope independently.
	let active_buffer_id = rs.active_document.document_buffer_id;
	let provider_snap = providers.snapshot(active_buffer_id);
	let fingerprint = compute_fingerprint(&provider_snap, source_line_count);

	if state.last_fingerprint == Some(fingerprint) {
		return WorkerDecision::CacheHit;
	}
	state.last_fingerprint = Some(fingerprint);

	let mut rows: Vec<VirtualRow> = Vec::new();
	for p in &provider_snap {
		rows.extend(p.collect());
	}

	let publish_version = state.next_publish_version;
	state.next_publish_version = publish_version.wrapping_add(1);
	let new_matrix = VirtualRowMatrix::build(
		rows,
		source_line_count,
		VirtualRowVersion(publish_version),
	);
	matrix_cell.store(Arc::new(new_matrix));
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
	matrix_cell: Arc<ArcSwap<VirtualRowMatrix>>,
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
		let decision = recompute(&mut state, &render_state, &providers, &matrix_cell);
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
		}
	}

	fn make_render_state_with_doc(text: &str) -> Arc<ArcSwap<RenderState>> {
		let mut snap = DocumentSnapshot::default();
		snap.buffer = Buffer::from_text(text);
		let mut rs = RenderState::default();
		rs.cells = Arc::new(crate::render_state::CellsRenderState {
			snapshot: Some(Arc::new(snap)),
			..crate::render_state::CellsRenderState::default()
		});
		Arc::new(ArcSwap::from_pointee(rs))
	}

	fn make_render_state_no_doc() -> Arc<ArcSwap<RenderState>> {
		Arc::new(ArcSwap::from_pointee(RenderState::default()))
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
		let fp_a = compute_fingerprint(&[p.clone()], 10);
		let fp_b = compute_fingerprint(&[p], 11);
		assert_ne!(fp_a, fp_b);
	}

	#[test]
	fn fingerprint_changes_when_provider_version_changes() {
		let mock = Arc::new(MockProvider::new(1, vec![]));
		let dyn_provider: Arc<dyn VirtualRowProvider> = mock.clone();
		let fp_a = compute_fingerprint(&[dyn_provider.clone()], 10);
		mock.bump_version();
		let fp_b = compute_fingerprint(&[dyn_provider], 10);
		assert_ne!(fp_a, fp_b);
	}

	// ── Recompute ─────────────────────────────────────────────

	#[test]
	fn recompute_no_document_emits_clear() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_no_doc();
		let reg = VirtualRowProviderRegistry::new();
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
		let decision = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(decision, WorkerDecision::Clear);
	}

	#[test]
	fn recompute_with_no_providers_is_cache_hit_after_first() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_with_doc("a\nb\nc\n");
		let reg = VirtualRowProviderRegistry::new();
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));
		// First wake: fingerprint not yet seen → Recomputed
		// (publishes an empty matrix tagged with the line count).
		let d1 = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(d1, WorkerDecision::Recomputed);
		// Second wake: identical fingerprint → CacheHit.
		let d2 = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(d2, WorkerDecision::CacheHit);
	}

	#[test]
	fn recompute_publishes_rows_from_provider() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_with_doc("a\nb\nc\nd\n");
		let reg = VirtualRowProviderRegistry::new();
		let provider = Arc::new(MockProvider::new(
			1,
			vec![row(1, AnchorPosition::Above), row(2, AnchorPosition::Below)],
		));
		reg.register(ACTIVE, provider.clone() as Arc<dyn VirtualRowProvider>);
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		let d = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(d, WorkerDecision::Recomputed);
		let published = cell.load_full();
		assert_eq!(published.len(), 2);
		assert!(!published.is_empty());
	}

	#[test]
	fn recompute_cache_hit_after_provider_static() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_with_doc("a\nb\n");
		let reg = VirtualRowProviderRegistry::new();
		let provider = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
		reg.register(ACTIVE, provider as Arc<dyn VirtualRowProvider>);
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		let d1 = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(d1, WorkerDecision::Recomputed);
		let d2 = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(d2, WorkerDecision::CacheHit);
	}

	#[test]
	fn recompute_re_publishes_when_provider_version_bumps() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_with_doc("a\nb\n");
		let reg = VirtualRowProviderRegistry::new();
		let provider = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
		reg.register(ACTIVE, provider.clone() as Arc<dyn VirtualRowProvider>);
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		recompute(&mut state, &rs, &reg, &cell);
		let v1 = cell.load_full().version;

		provider.replace_rows(vec![row(0, AnchorPosition::Above), row(1, AnchorPosition::Below)]);
		let d = recompute(&mut state, &rs, &reg, &cell);
		assert_eq!(d, WorkerDecision::Recomputed);
		let v2 = cell.load_full().version;
		assert!(v2.0 > v1.0, "publish version monotonically bumps");
		assert_eq!(cell.load_full().len(), 2);
	}

	#[test]
	fn recompute_re_publishes_when_document_line_count_changes() {
		let mut state = VirtualRowsWorkerState::new();
		let rs1 = make_render_state_with_doc("a\nb\n");
		let reg = VirtualRowProviderRegistry::new();
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		let d1 = recompute(&mut state, &rs1, &reg, &cell);
		assert_eq!(d1, WorkerDecision::Recomputed);
		let v1 = cell.load_full().version;

		// Replace render_state with one whose document has more lines.
		let rs2 = make_render_state_with_doc("a\nb\nc\nd\n");
		let d2 = recompute(&mut state, &rs2, &reg, &cell);
		assert_eq!(d2, WorkerDecision::Recomputed);
		let v2 = cell.load_full().version;
		assert!(v2.0 > v1.0);
		// ropey counts trailing implicit empty line: "a\nb\nc\nd\n" = 5
		assert_eq!(cell.load_full().source_line_count, 5);
	}

	#[test]
	fn recompute_merges_rows_from_multiple_providers() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_with_doc("a\nb\nc\nd\n");
		let reg = VirtualRowProviderRegistry::new();
		let p1 = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
		let p2 = Arc::new(MockProvider::new(2, vec![row(2, AnchorPosition::Below)]));
		reg.register(ACTIVE, p1 as Arc<dyn VirtualRowProvider>);
		reg.register(ACTIVE, p2 as Arc<dyn VirtualRowProvider>);
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		recompute(&mut state, &rs, &reg, &cell);
		let published = cell.load_full();
		assert_eq!(published.len(), 2);
	}

	/// D.4.d.2.1.a: the worker scopes its provider snapshot
	/// to the active document's buffer. A provider registered
	/// against a *different* buffer must not contribute rows
	/// to the active doc's matrix — load-bearing for
	/// D.4.d.2.1.b's per-pane iteration, where each pane's
	/// matrix only sees its own buffer's providers.
	#[test]
	fn recompute_only_polls_active_doc_providers() {
		let mut state = VirtualRowsWorkerState::new();
		let rs = make_render_state_with_doc("a\nb\nc\n");
		let reg = VirtualRowProviderRegistry::new();
		// Register against a buffer that is NOT the active one.
		let foreign = BufferId(42);
		assert_ne!(foreign, ACTIVE);
		let provider = Arc::new(MockProvider::new(
			1,
			vec![row(0, AnchorPosition::Above), row(1, AnchorPosition::Below)],
		));
		reg.register(foreign, provider as Arc<dyn VirtualRowProvider>);
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		let d = recompute(&mut state, &rs, &reg, &cell);
		// Recomputed because the fingerprint of (no active-doc
		// providers, line_count=4) is unseen; matrix is empty.
		assert_eq!(d, WorkerDecision::Recomputed);
		assert_eq!(cell.load_full().len(), 0);
	}
}
