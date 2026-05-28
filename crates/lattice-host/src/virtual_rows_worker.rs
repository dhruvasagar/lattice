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
//! - [`VirtualRowProviderRegistry`] — `BufferId`-agnostic for
//!   v1 (single global matrix tied to the active document,
//!   matching the `cells_matrix_cell` shape). Future
//!   per-document matrices land alongside the multi-pane
//!   diff slices.
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

use crate::editor::VirtualRowsWake;
use crate::render_state::RenderState;

/// Process-wide registry of [`VirtualRowProvider`] instances.
///
/// Mutation (`register` / `unregister`) is consumer-creation
/// frequency — never per-frame. Read (`snapshot`) is per
/// worker tick. Behind a `std::sync::Mutex`; the hot path
/// holds the lock only long enough to clone out `Arc`
/// references.
#[derive(Debug, Default)]
pub struct VirtualRowProviderRegistry {
	providers: Mutex<HashMap<ProviderId, Arc<dyn VirtualRowProvider>>>,
}

impl VirtualRowProviderRegistry {
	pub fn new() -> Self {
		Self::default()
	}

	/// Register a provider. Returns `false` if a provider with
	/// the same id was already registered (no replacement —
	/// the caller is expected to `unregister` first).
	pub fn register(&self, provider: Arc<dyn VirtualRowProvider>) -> bool {
		let id = provider.id();
		let mut providers = self
			.providers
			.lock()
			.expect("VirtualRowProviderRegistry mutex poisoned");
		if providers.contains_key(&id) {
			return false;
		}
		providers.insert(id, provider);
		true
	}

	/// Remove a provider. Returns `true` if one was removed.
	pub fn unregister(&self, id: ProviderId) -> bool {
		self.providers
			.lock()
			.expect("VirtualRowProviderRegistry mutex poisoned")
			.remove(&id)
			.is_some()
	}

	/// Snapshot all registered providers. Returns fresh `Arc`
	/// clones — callers can hold them past a concurrent
	/// `unregister` (RCU). Order is unspecified.
	pub fn snapshot(&self) -> Vec<Arc<dyn VirtualRowProvider>> {
		self.providers
			.lock()
			.expect("VirtualRowProviderRegistry mutex poisoned")
			.values()
			.cloned()
			.collect()
	}

	pub fn is_empty(&self) -> bool {
		self.providers
			.lock()
			.expect("VirtualRowProviderRegistry mutex poisoned")
			.is_empty()
	}

	pub fn len(&self) -> usize {
		self.providers
			.lock()
			.expect("VirtualRowProviderRegistry mutex poisoned")
			.len()
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

	let provider_snap = providers.snapshot();
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
	use lattice_core::Buffer;
	use lattice_runtime::DocumentSnapshot;
	use std::sync::atomic::{AtomicU64, Ordering};

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
		assert!(reg.register(p as Arc<dyn VirtualRowProvider>));
		assert_eq!(reg.len(), 1);
		assert_eq!(reg.snapshot().len(), 1);
		assert!(reg.unregister(1));
		assert!(reg.is_empty());
	}

	#[test]
	fn registry_duplicate_register_returns_false() {
		let reg = VirtualRowProviderRegistry::new();
		let p1 = Arc::new(MockProvider::new(1, vec![]));
		let p2 = Arc::new(MockProvider::new(1, vec![row(0, AnchorPosition::Above)]));
		assert!(reg.register(p1 as Arc<dyn VirtualRowProvider>));
		assert!(!reg.register(p2 as Arc<dyn VirtualRowProvider>));
		assert_eq!(reg.len(), 1);
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
		reg.register(provider.clone() as Arc<dyn VirtualRowProvider>);
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
		reg.register(provider as Arc<dyn VirtualRowProvider>);
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
		reg.register(provider.clone() as Arc<dyn VirtualRowProvider>);
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
		reg.register(p1 as Arc<dyn VirtualRowProvider>);
		reg.register(p2 as Arc<dyn VirtualRowProvider>);
		let cell = Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty()));

		recompute(&mut state, &rs, &reg, &cell);
		let published = cell.load_full();
		assert_eq!(published.len(), 2);
	}
}
